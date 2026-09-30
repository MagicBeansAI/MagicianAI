//! Auth routes: login, logout, session — the human door.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md`
//! §3.1 and §7A. All additive on the v2 scope; no existing route changes.
//! Errors follow the repo convention: `{"error": snake_code, "message": …}`.
//!
//! The login route is also the **bootstrap**: when no identity exists at
//! all and `allow_signup` is on, the first successful login *creates* the
//! owner identity (adopting `scopes/anonymous/`). After the first identity
//! exists, login never creates anything — usernames are not an enrollment
//! surface.

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use magician::config::AuthConfig;
use magician::magician_v2::attention::learning::AttentionLearningService;
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::auth::credentials::{hash_password, CredentialKind, Provider};
use magician::magician_v2::auth::middleware::{authenticated, AuthRuntime};
use magician::magician_v2::auth::workspace_registry;
use magician::magician_v2::auth::{
    floored_tools, social, AuthError, AuthStore, MintGrantSpec, GRANT_TTL_MAX_HOURS,
    GRANT_TTL_MIN_HOURS,
};

// ------------------------------------------------------------------------
// Login throttle
// ========================================================================

/// In-memory sliding-window throttle for the password door. Process-local
/// by design: this is a single install, and an attacker able to restart the
/// process to clear the ledger already owns the machine. Two caps — per
/// username (slows a focused guesser) and per peer (slows spraying across
/// many usernames). Success clears the username's window so an honest
/// typo-prone owner is never locked out; peer failures persist so an
/// attacker cannot reset the spray cap by alternating with one success.
pub(crate) struct LoginThrottle {
    window: Duration,
    max_per_username: usize,
    max_per_peer: usize,
    by_username: Mutex<HashMap<String, Vec<Instant>>>,
    by_peer: Mutex<HashMap<String, Vec<Instant>>>,
}

impl LoginThrottle {
    fn new(window: Duration, max_per_username: usize, max_per_peer: usize) -> Self {
        Self {
            window,
            max_per_username,
            max_per_peer,
            by_username: Mutex::new(HashMap::new()),
            by_peer: Mutex::new(HashMap::new()),
        }
    }

    fn prune(events: &mut Vec<Instant>, window: Duration) {
        let cutoff = Instant::now() - window;
        let first_live = events.partition_point(|at| *at < cutoff);
        events.drain(..first_live);
    }

    /// `Some(retry_after_secs)` when either cap is exceeded.
    pub(crate) fn check(&self, username: &str, peer: &str) -> Option<u64> {
        let now = Instant::now();
        let mut username_blocked_until: Option<Instant> = None;
        if let Ok(mut ledger) = self.by_username.lock() {
            if let Some(events) = ledger.get_mut(username) {
                Self::prune(events, self.window);
                if events.len() >= self.max_per_username {
                    // `unwrap_or(now)`: an Instant near its maximum cannot be
                    // advanced by the window; the `.max(1)` below still gives
                    // the caller a sane floor instead of a panic.
                    let oldest = events[0];
                    username_blocked_until = Some(oldest.checked_add(self.window).unwrap_or(now));
                }
            }
        }
        let mut peer_blocked_until: Option<Instant> = None;
        if let Ok(mut ledger) = self.by_peer.lock() {
            if let Some(events) = ledger.get_mut(peer) {
                Self::prune(events, self.window);
                if events.len() >= self.max_per_peer {
                    let oldest = events[0];
                    peer_blocked_until = Some(oldest.checked_add(self.window).unwrap_or(now));
                }
            }
        }
        // The binding cap is whichever window drains soonest — the caller
        // may retry then, and `check` re-evaluates on every attempt.
        username_blocked_until
            .into_iter()
            .chain(peer_blocked_until)
            .min()
            .map(|until| until.saturating_duration_since(now).as_secs().max(1))
    }

    pub(crate) fn record_failure(&self, username: &str, peer: &str) {
        let now = Instant::now();
        if let Ok(mut ledger) = self.by_username.lock() {
            let events = ledger.entry(username.to_string()).or_default();
            events.push(now);
            Self::prune(events, self.window);
        }
        if let Ok(mut ledger) = self.by_peer.lock() {
            let events = ledger.entry(peer.to_string()).or_default();
            events.push(now);
            Self::prune(events, self.window);
        }
    }

    pub(crate) fn record_success(&self, username: &str, _peer: &str) {
        if let Ok(mut ledger) = self.by_username.lock() {
            ledger.remove(username);
        }
    }
}

static LOGIN_THROTTLE: OnceLock<LoginThrottle> = OnceLock::new();

/// Reserved ledger key for the admin-secret doors inside the shared login
/// throttle — reserved by convention (a member literally named this would
/// share its window, an availability nuisance the owner controls).
pub(crate) const ADMIN_SECRET_THROTTLE_KEY: &str = "__admin_secret__";

fn login_throttle() -> &'static LoginThrottle {
    LOGIN_THROTTLE.get_or_init(|| LoginThrottle::new(Duration::from_secs(15 * 60), 8, 40))
}

/// The admin-secret doors (`/chat/enroll/approve`, the orphaned-scopes
/// reconciler) share the password door's ledger under a reserved key: a
/// guessable admin secret is a password with more privileges and gets the
/// same sliding-window 429 treatment. The key is reserved by convention —
/// a member literally named `__admin_secret__` would share its window,
/// which is at worst an availability nuisance the owner controls.
pub(crate) fn admin_secret_throttle() -> &'static LoginThrottle {
    login_throttle()
}

/// The throttle's peer key: the remote **IP**, not `ip:port` — the source
/// port is ephemeral, so a per-connection key would hand every reconnect a
/// fresh spray budget. Shared by the login door and both admin doors.
pub(crate) fn peer_of(req: &HttpRequest) -> String {
    req.peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn configure_auth_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/login", web::post().to(login_handler))
        .route(
            "/auth/identities",
            web::post().to(create_member_identity_handler),
        )
        .route("/auth/logout", web::post().to(logout_handler))
        .route("/auth/session", web::get().to(session_handler))
        .route(
            "/auth/session/scope",
            web::post().to(rotate_session_scope_handler),
        )
        .route("/auth/social/{provider}/start", web::get().to(social_start))
        .route(
            "/auth/link/{provider}/start",
            web::post().to(social_link_start),
        )
        .route("/auth/callback/{provider}", web::get().to(social_callback))
        .route("/auth/tokens", web::post().to(mint_api_token_handler))
        .route("/auth/tokens", web::get().to(list_api_tokens_handler))
        .route(
            "/auth/tokens/{id}",
            web::delete().to(revoke_api_token_handler),
        )
        .route("/auth/grants", web::post().to(mint_grant_handler))
        .route("/auth/grants", web::get().to(list_grants_handler))
        .route("/auth/grants/{id}", web::delete().to(revoke_grant_handler))
        .route(
            "/auth/admin/orphaned-scopes",
            web::get().to(orphaned_scopes_handler),
        )
        .route("/workspaces", web::get().to(list_workspaces_handler))
        .route("/workspaces", web::post().to(create_workspace_handler))
        .route(
            "/workspaces/{id}",
            web::patch().to(update_workspace_handler),
        )
        .route(
            "/workspaces/{id}",
            web::delete().to(delete_workspace_handler),
        );
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Debug, Serialize)]
struct IdentityPayload {
    name: String,
    display_name: String,
}

#[derive(Debug, Serialize)]
struct LoginResponse {
    token: String,
    token_type: &'static str,
    expires_at: chrono::DateTime<chrono::Utc>,
    principal: String,
    workspace: String,
    identity: IdentityPayload,
}

async fn login_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<LoginRequest>,
) -> impl Responder {
    let username = body.username.trim();
    let password = body.password.as_str();
    if username.is_empty() || password.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "username and password are required"
        }));
    }

    // The throttle rejects before any argon2 work: a focused guesser or a
    // username spray both hit a cheap 429, and the door stays cheap to
    // defend. Honest owners clear their own window by signing in
    // successfully.
    let peer = peer_of(&req);
    if let Some(retry_after_secs) = login_throttle().check(username, &peer) {
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", retry_after_secs.to_string()))
            .json(serde_json::json!({
                "error": "too_many_attempts",
                "message": "Too many failed sign-in attempts. Try again later.",
                "retry_after_secs": retry_after_secs
            }));
    }

    let identity = match runtime.store.verify_password(username, password) {
        Ok(Some(identity)) => Some(identity),
        Ok(None)
            if runtime.store.identities_empty().unwrap_or(false) && runtime.config.allow_signup =>
        {
            // First-identity bootstrap (workspace design §3.2 provisioning
            // rule): the first identity created by any method becomes the
            // owner, adopting scopes/anonymous via scope_root aliasing.
            match runtime.store.create_identity(
                username,
                username,
                CredentialKind::Password {
                    hash: match hash_password(password) {
                        Ok(hash) => hash,
                        Err(error) => {
                            return HttpResponse::InternalServerError().json(serde_json::json!({
                                "error": "password_hash_failed",
                                "message": error.to_string()
                            }));
                        },
                    },
                },
            ) {
                Ok(identity) => Some(identity),
                Err(AuthError::InvalidPrincipalName(name)) => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "invalid_username",
                        "message": format!(
                            "Username {name:?} must be 1-32 chars of a-z, 0-9, '_' or '-'."
                        )
                    }));
                },
                Err(error) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "identity_creation_failed",
                        "message": error.to_string()
                    }));
                },
            }
        },
        Ok(None) => None,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "auth_store_failure",
                "message": error.to_string()
            }));
        },
    };

    let Some(identity) = identity else {
        login_throttle().record_failure(username, &peer);
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "invalid_credentials",
            "message": "username or password is incorrect"
        }));
    };

    login_throttle().record_success(username, &peer);

    match runtime.store.mint_session(
        &identity.name,
        body.workspace.as_deref().unwrap_or("default"),
        magician::magician_v2::auth::AuthMethod::Password,
        runtime.config.session_ttl_days,
    ) {
        Ok((session, token)) => {
            tracing::info!(
                identity = %identity.name,
                principal = %identity.scope_root,
                workspace = %session.workspace,
                method = "password",
                "[AUTH] login minted session"
            );
            HttpResponse::Created().json(LoginResponse {
                token,
                token_type: "Bearer",
                expires_at: session.expires_at,
                principal: identity.scope_root.clone(),
                workspace: session.workspace,
                identity: IdentityPayload {
                    name: identity.name.clone(),
                    display_name: identity.display_name.clone(),
                },
            })
        },
        Err(AuthError::UnownedWorkspace {
            principal,
            workspace,
        }) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unowned_workspace",
            "message": format!("Principal {principal} does not own workspace {workspace}.")
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "session_mint_failed",
            "message": error.to_string()
        })),
    }
}

#[derive(Debug, Deserialize)]
struct CreateMemberRequest {
    username: String,
    password: String,
    display_name: Option<String>,
}

/// `POST /auth/identities` — the owner provisions a member identity for a
/// shared install (a family member, a collaborator). Roles are not modeled
/// yet; the interim admin rule is the smallest honest one: the first
/// identity in the registry — the one that booted this install and adopted
/// `scopes/anonymous` — is the owner, and only it may create members. The
/// member gets a fresh scope root named after themselves plus the default
/// workspace, and signs in via `/auth/login` like anyone else. No token is
/// returned: provisioning is not impersonation. A login **session** must
/// be behind the request — a PAT or terminal grant is a delegable
/// automation credential, fine for everything else, deliberately not for
/// making new humans.
async fn create_member_identity_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<CreateMemberRequest>,
) -> impl Responder {
    let stamped = match require_session(&req) {
        Ok(stamped) => stamped,
        Err(response) => return response,
    };
    if !runtime.config.allow_signup {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "signup_disabled",
            "message": "allow_signup is off; no new identities may be created."
        }));
    }
    let owner_name = match runtime.store.list_identities() {
        Ok(identities) => identities.first().map(|identity| identity.name.clone()),
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "auth_store_failure",
                "message": error.to_string()
            }));
        },
    };
    if owner_name.as_deref() != Some(stamped.identity_name.as_str()) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "owner_required",
            "message": "Only the owner (the first identity on this install) may provision members."
        }));
    }
    let username = body.username.trim();
    let password = body.password.as_str();
    if username.is_empty() || password.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "username and password are required"
        }));
    }
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(username);
    if display_name.chars().count() > 64 {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_display_name",
            "message": "display_name must be at most 64 characters."
        }));
    }
    let hash = match hash_password(password) {
        Ok(hash) => hash,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "password_hash_failed",
                "message": error.to_string()
            }));
        },
    };
    match runtime
        .store
        .create_identity(username, display_name, CredentialKind::Password { hash })
    {
        Ok(identity) => HttpResponse::Created().json(serde_json::json!({
            "identity": {
                "name": identity.name,
                "display_name": identity.display_name,
                "scope_root": identity.scope_root,
                "created_at": identity.created_at,
            },
            "message": "Member created. They sign in via POST /auth/login with these credentials."
        })),
        Err(AuthError::InvalidPrincipalName(name)) => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_username",
                "message": format!(
                    "Username {name:?} must be 1-32 chars of a-z, 0-9, '_' or '-'."
                )
            }))
        },
        Err(AuthError::IdentityExists(name)) => HttpResponse::Conflict().json(serde_json::json!({
            "error": "identity_exists",
            "message": format!("An identity named {name:?} already exists.")
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "identity_creation_failed",
            "message": error.to_string()
        })),
    }
}

async fn logout_handler(req: HttpRequest, runtime: web::Data<AuthRuntime>) -> impl Responder {
    let Some(stamped) = authenticated(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }));
    };
    match stamped.bearer {
        magician::magician_v2::auth::BearerKind::Session(_) => {},
        magician::magician_v2::auth::BearerKind::ApiToken => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "not_a_session",
                "message": "API tokens are revoked via DELETE /auth/tokens/{id}."
            }));
        },
        magician::magician_v2::auth::BearerKind::Grant { .. } => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "not_a_session",
                "message": "Terminal grants are revoked via DELETE /auth/grants/{id}."
            }));
        },
        magician::magician_v2::auth::BearerKind::Bot { .. } => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "not_a_session",
                "message": "Bot tokens are revoked by the runtime when the bot stops."
            }));
        },
    }
    let revoked = match bearer_token_of(&req) {
        Some(token) => runtime.store.revoke_session(&token),
        None => Ok(false),
    };
    match revoked {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        })),
    }
}

fn bearer_token_of(req: &HttpRequest) -> Option<String> {
    magician::magician_v2::auth::middleware::bearer_token(req.headers())
}

#[derive(Debug, Serialize)]
struct WorkspacePayload {
    id: String,
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_default: bool,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    /// `None` for a runtime-minted bot token: no person stands behind it.
    identity: Option<IdentityPayload>,
    method: String,
    /// The bot the runtime minted this bearer for; absent for every other kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    bot: Option<String>,
    principal: String,
    workspace: String,
    workspaces: Vec<WorkspacePayload>,
}

/// The client switcher payload (§7A): who is logged in and which
/// workspaces they own. Without a bearer this 401s even during open-mode
/// bootstrap: there is no authenticated identity or workspace switcher to
/// describe.
async fn session_handler(req: HttpRequest, runtime: web::Data<AuthRuntime>) -> impl Responder {
    let Some(stamped) = authenticated(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }));
    };
    // A bot daemon asks this route for the scope the runtime engraved at
    // spawn (the bot SDK's `resolveBearerScope`, before its adapter starts).
    // There is no identity to look up and no workspace roster to offer: the
    // scope is the whole answer, and the bot is named in place of a person.
    if let magician::magician_v2::auth::BearerKind::Bot { bot_name } = &stamped.bearer {
        return HttpResponse::Ok().json(SessionResponse {
            identity: None,
            method: "bot_token".to_string(),
            bot: Some(bot_name.clone()),
            principal: stamped.scope.principal().to_string(),
            workspace: stamped.scope.workspace().to_string(),
            workspaces: Vec::new(),
        });
    }
    let Some(identity_name) = stamped.identity_name.as_deref() else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "The presented bearer carries no login identity."
        }));
    };
    let identity = match runtime.store.find_identity(identity_name) {
        Ok(identity) => identity,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "auth_store_failure",
                "message": error.to_string()
            }));
        },
    };
    let Some(identity) = identity else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "The session's identity no longer exists."
        }));
    };
    let scopes_root = runtime.store.runtime_root().join("scopes");
    let workspaces = match workspace_registry::list(&scopes_root, &identity) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "workspace_registry_failure",
                "message": error.to_string()
            }));
        },
    };
    let method = match stamped.bearer {
        magician::magician_v2::auth::BearerKind::Session(method) => match method {
            magician::magician_v2::auth::AuthMethod::Password => "password",
            magician::magician_v2::auth::AuthMethod::Google => "google",
            magician::magician_v2::auth::AuthMethod::Github => "github",
        },
        magician::magician_v2::auth::BearerKind::ApiToken => "api_token",
        magician::magician_v2::auth::BearerKind::Grant { .. } => "terminal_grant",
        magician::magician_v2::auth::BearerKind::Bot { .. } => "bot_token",
    };
    HttpResponse::Ok().json(SessionResponse {
        identity: Some(IdentityPayload {
            name: identity.name.clone(),
            display_name: identity.display_name.clone(),
        }),
        method: method.to_string(),
        bot: None,
        principal: stamped.scope.principal().to_string(),
        workspace: stamped.scope.workspace().to_string(),
        workspaces: workspaces
            .into_iter()
            .map(|workspace| WorkspacePayload {
                id: workspace.id,
                display_name: workspace.display_name,
                description: workspace.description,
                is_default: workspace.is_default,
            })
            .collect(),
    })
}

#[derive(Debug, Deserialize)]
struct RotateSessionScopeRequest {
    workspace: String,
}

/// Atomically replace the presented login session with one bound to another
/// owned workspace. The token value changes; callers must discard the old one.
async fn rotate_session_scope_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<RotateSessionScopeRequest>,
) -> impl Responder {
    let stamped = match require_session(&req) {
        Ok(stamped) => stamped,
        Err(response) => return response,
    };
    let Some(current_token) = bearer_token_of(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }));
    };
    if body.workspace.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "workspace is required"
        }));
    }
    match runtime
        .store
        .rotate_session_scope(&current_token, body.workspace.trim())
    {
        Ok(Some((session, token))) => HttpResponse::Ok().json(serde_json::json!({
            "token": token,
            "token_type": "Bearer",
            "expires_at": session.expires_at,
            "principal": stamped.scope.principal(),
            "workspace": session.workspace,
            "identity": stamped.identity_name,
        })),
        Ok(None) => HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "The presented session is no longer valid."
        })),
        Err(AuthError::UnownedWorkspace {
            principal,
            workspace,
        }) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unowned_workspace",
            "message": format!("Principal {principal} does not own workspace {workspace}.")
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "session_scope_rotation_failed",
            "message": error.to_string()
        })),
    }
}

/// Build the shared runtime for `app_data` — used by magician-bin and tests.
pub fn auth_runtime(store: Arc<AuthStore>, config: AuthConfig) -> web::Data<AuthRuntime> {
    auth_runtime_with_http(store, config, Arc::new(social::ReqwestSocialHttp))
}

/// Same, with an injected social HTTP transport — tests use this to keep
/// every flow offline.
pub fn auth_runtime_with_http(
    store: Arc<AuthStore>,
    config: AuthConfig,
    social_http: Arc<dyn social::SocialHttp>,
) -> web::Data<AuthRuntime> {
    web::Data::new(AuthRuntime {
        store,
        config,
        social: social::SocialFlows::default(),
        social_http,
    })
}

// ---- social login (§3.2) ----------------------------------------------------

fn provider_config(
    config: &AuthConfig,
    provider: Provider,
) -> Option<&magician::config::AuthProviderConfig> {
    match provider {
        Provider::Google => Some(&config.providers.google),
        Provider::Github => Some(&config.providers.github),
    }
}

/// The callback URL as the provider will see it — derived from the request
/// Host so any bind address works without configuration (§3.2).
fn redirect_uri(req: &HttpRequest, provider: Provider) -> String {
    let host = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| !host.is_empty() && host.len() < 256 && !host.contains(' '))
        .unwrap_or("127.0.0.1");
    format!(
        "http://{host}/api/magician/v2/auth/callback/{}",
        provider.as_str()
    )
}

fn parse_provider(raw: &str) -> Option<Provider> {
    Provider::parse(raw)
}

async fn social_start_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    provider: web::Path<String>,
    link_to: Option<String>,
) -> HttpResponse {
    let Some(provider) = parse_provider(&provider) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown_provider",
            "message": "Provider must be 'google' or 'github'."
        }));
    };
    let Some(config) = provider_config(&runtime.config, provider) else {
        return HttpResponse::InternalServerError().finish();
    };
    if !config.is_configured(provider) {
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "provider_not_configured",
            "message": format!("{} client id/secret are not configured.", provider.as_str())
        }));
    }
    let ticket = match runtime.social.mint(provider, link_to) {
        Ok(ticket) => ticket,
        Err(social::SocialError::RegistryFull) => {
            return HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "too_many_login_flows",
                "message": "Too many concurrent login flows; retry shortly."
            }));
        },
        Err(_) => {
            return HttpResponse::InternalServerError().finish();
        },
    };
    let challenge = social::pkce_challenge(&ticket.pkce_verifier);
    let url = social::authorize_url(
        provider,
        &config.client_id,
        &redirect_uri(&req, provider),
        &ticket.state,
        &challenge,
    );
    HttpResponse::Found()
        .insert_header(("Location", url))
        .finish()
}

pub async fn social_start(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    provider: web::Path<String>,
) -> impl Responder {
    social_start_handler(req, runtime, provider, None).await
}

/// Explicit linking (§3.2): like `start`, but the ticket binds to the
/// session's identity and the callback attaches a credential instead of
/// creating one. No auto-link by email, ever.
pub async fn social_link_start(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    provider: web::Path<String>,
) -> impl Responder {
    let Some(stamped) = authenticated(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "Linking requires an authenticated session."
        }));
    };
    let Some(link_to) = stamped.identity_name.clone() else {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "session_required",
            "message": "Linking a provider requires a login identity; a bot token carries none."
        }));
    };
    social_start_handler(req, runtime, provider, Some(link_to)).await
}

#[derive(Debug, Deserialize)]
pub(crate) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The callback renders to a browser, so every interpolated byte is
/// escaped — provider query params and error strings are untrusted input.
fn callback_page(status: actix_web::http::StatusCode, title: &str, detail: &str) -> HttpResponse {
    let title = html_escape(title);
    let detail = html_escape(detail);
    HttpResponse::build(status)
        .content_type("text/html; charset=utf-8")
        .body(format!(
            "<!doctype html><html><head><title>{title}</title></head>\
             <body style=\"font-family:system-ui;padding:3rem\">\
             <h1>{title}</h1><p>{detail}</p></body></html>"
        ))
}

pub(crate) async fn social_callback(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    provider: web::Path<String>,
    query: web::Query<CallbackQuery>,
) -> impl Responder {
    let Some(provider) = parse_provider(&provider) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown_provider",
            "message": "Provider must be 'google' or 'github'."
        }));
    };
    if let Some(error) = query.error.as_deref() {
        return callback_page(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Login canceled",
            &format!(
                "The provider reported: {}",
                error.chars().take(200).collect::<String>()
            ),
        );
    }
    let (Some(code), Some(state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return callback_page(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Incomplete callback",
            "Both code and state are required.",
        );
    };
    let ticket = match runtime.social.consume(provider, state) {
        Ok(ticket) => ticket,
        Err(_) => {
            return callback_page(
                actix_web::http::StatusCode::BAD_REQUEST,
                "Unknown login state",
                "The login attempt is unknown, expired, or already used. Start again.",
            );
        },
    };
    let Some(config) = provider_config(&runtime.config, provider) else {
        return HttpResponse::InternalServerError().finish();
    };
    let method = match provider {
        Provider::Google => magician::magician_v2::auth::AuthMethod::Google,
        Provider::Github => magician::magician_v2::auth::AuthMethod::Github,
    };
    let token = match social::exchange_code(
        runtime.social_http.as_ref(),
        provider,
        &config.client_id,
        &config.effective_secret(provider),
        code,
        &redirect_uri(&req, provider),
        &ticket.pkce_verifier,
    )
    .await
    {
        Ok(token) => token,
        Err(error) => {
            return callback_page(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "Login failed",
                &format!("Token exchange failed: {error}"),
            );
        },
    };
    let profile = match social::fetch_profile(runtime.social_http.as_ref(), provider, &token).await
    {
        Ok(profile) => profile,
        Err(error) => {
            return callback_page(
                actix_web::http::StatusCode::BAD_GATEWAY,
                "Login failed",
                &format!("Profile fetch failed: {error}"),
            );
        },
    };

    // Resolve by (provider, subject) — never by email.
    let known_identity = match runtime
        .store
        .resolve_oauth_credential(&provider, &profile.subject)
    {
        Ok(identity) => identity,
        Err(error) => {
            return callback_page(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Login failed",
                &format!("Identity store lookup failed: {error}"),
            );
        },
    };
    let identity = if let Some(identity) = known_identity {
        match runtime.store.find_identity(&identity) {
            Ok(identity) => identity,
            Err(error) => {
                return callback_page(
                    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Login failed",
                    &format!("Identity store lookup failed: {error}"),
                );
            },
        }
    } else if let Some(link_to) = ticket.link_to.as_deref() {
        match runtime.store.link_credential(
            link_to,
            magician::magician_v2::auth::CredentialKind::OAuthLink {
                provider,
                subject: profile.subject.clone(),
            },
        ) {
            Ok(()) => match runtime.store.find_identity(link_to) {
                Ok(identity) => identity,
                Err(error) => {
                    return callback_page(
                        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "Linking failed",
                        &format!("Identity store lookup failed: {error}"),
                    );
                },
            },
            Err(AuthError::ProviderSubjectAlreadyLinked { identity, .. }) => {
                match runtime.store.find_identity(&identity) {
                    Ok(identity) => identity,
                    Err(error) => {
                        return callback_page(
                            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "Linking failed",
                            &format!("Identity store lookup failed: {error}"),
                        );
                    },
                }
            },
            Err(error) => {
                return callback_page(
                    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Linking failed",
                    &format!("{error}"),
                );
            },
        }
    } else if runtime.config.allow_signup {
        match runtime.store.create_social_identity(
            provider,
            &profile.subject,
            &profile.display_name,
        ) {
            Ok(identity) => Some(identity),
            Err(error) => {
                return callback_page(
                    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Signup failed",
                    &format!("{error}"),
                );
            },
        }
    } else {
        return callback_page(
            actix_web::http::StatusCode::FORBIDDEN,
            "Signup disabled",
            "No identity matches this account and allow_signup is off.",
        );
    };
    let Some(identity) = identity else {
        return callback_page(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Login failed",
            "The resolved identity no longer exists.",
        );
    };
    match runtime.store.mint_session(
        &identity.name,
        "default",
        method,
        runtime.config.session_ttl_days,
    ) {
        Ok((session, token)) => {
            // v1 handoff: the session token is returned in the body. JSON
            // for programmatic clients; an HTML page for the browser flow
            // (the desktop/web integration picks it up from here — the
            // /authorize OAuth server for MCP clients is the later plan).
            let wants_html = req
                .headers()
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .map(|accept| accept.contains("text/html") && !accept.contains("application/json"))
                .unwrap_or(false);
            let display = identity.display_name.clone();
            if wants_html {
                callback_page(
                    actix_web::http::StatusCode::OK,
                    "Logged in",
                    &format!(
                        "Signed in as {}. Session token (copy into your client): {}",
                        display, token
                    ),
                )
            } else {
                HttpResponse::Ok().json(LoginResponse {
                    token,
                    token_type: "Bearer",
                    expires_at: session.expires_at,
                    principal: identity.scope_root.clone(),
                    workspace: session.workspace,
                    identity: IdentityPayload {
                        name: identity.name.clone(),
                        display_name: identity.display_name.clone(),
                    },
                })
            }
        },
        Err(error) => callback_page(
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Login failed",
            &format!("{error}"),
        ),
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

// ---- API tokens (§3.3) ------------------------------------------------------
//
// Minting is behind a *session* only (workspace design §3.3: "settings UI /
// CLI, behind a session") — a token cannot mint tokens.

/// A stamped request that proved a login session — the only bearer kind
/// that may mint, rotate, or provision. Narrower than `AuthenticatedRequest`
/// so the identity is a fact of the type, not an `Option` every minter must
/// re-check.
pub(crate) struct SessionRequest {
    pub(crate) scope: magician::magician_v2::auth::ScopeRef,
    pub(crate) identity_name: String,
}

pub(crate) fn require_session(req: &HttpRequest) -> Result<SessionRequest, HttpResponse> {
    let stamped = authenticated(req).ok_or_else(|| {
        HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }))
    })?;
    match stamped.bearer {
        magician::magician_v2::auth::BearerKind::Session(_) => {},
        magician::magician_v2::auth::BearerKind::ApiToken => {
            return Err(HttpResponse::Forbidden().json(serde_json::json!({
                "error": "session_required",
                "message": "API tokens are minted from a login session, not from another token."
            })));
        },
        magician::magician_v2::auth::BearerKind::Grant { .. } => {
            return Err(HttpResponse::Forbidden().json(serde_json::json!({
                "error": "session_required",
                "message": "Terminal grants are minted from a login session, not from a grant."
            })));
        },
        magician::magician_v2::auth::BearerKind::Bot { .. } => {
            return Err(HttpResponse::Forbidden().json(serde_json::json!({
                "error": "session_required",
                "message": "Bot tokens are minted by the runtime and cannot mint or rotate."
            })));
        },
    }
    // A session bearer is resolved from the store against a live identity;
    // the middleware never stamps one without a name.
    let Some(identity_name) = stamped.identity_name else {
        return Err(HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "The presented session carries no login identity."
        })));
    };
    Ok(SessionRequest {
        scope: stamped.scope,
        identity_name,
    })
}

#[derive(Debug, Deserialize)]
struct MintTokenRequest {
    label: String,
    #[serde(default)]
    workspace: Option<String>,
}

async fn mint_api_token_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<MintTokenRequest>,
) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    if body.label.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "label is required"
        }));
    }
    let workspace = body
        .workspace
        .as_deref()
        .unwrap_or_else(|| session.scope.workspace());
    match runtime
        .store
        .mint_api_token(&session.identity_name, workspace, body.label.trim())
    {
        Ok((token, value)) => HttpResponse::Created().json(serde_json::json!({
            "id": token.id.to_string(),
            "label": token.label,
            "workspace": token.workspace,
            "created_at": token.created_at,
            // The only time the value is ever shown.
            "token": value,
        })),
        Err(AuthError::UnownedWorkspace {
            principal,
            workspace,
        }) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unowned_workspace",
            "message": format!("Principal {principal} does not own workspace {workspace}.")
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "token_mint_failed",
            "message": error.to_string()
        })),
    }
}

async fn list_api_tokens_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    match runtime.store.list_api_tokens(&session.identity_name) {
        Ok(tokens) => HttpResponse::Ok().json(
            // The stored rows never contain the value — only its hash — and
            // the hash is not exposed either.
            tokens
                .into_iter()
                .map(|token| {
                    serde_json::json!({
                        "id": token.id.to_string(),
                        "label": token.label,
                        "workspace": token.workspace,
                        "created_at": token.created_at,
                        "last_used": token.last_used,
                    })
                })
                .collect::<Vec<_>>(),
        ),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        })),
    }
}

async fn revoke_api_token_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    path: web::Path<String>,
) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    let Ok(id) = uuid::Uuid::parse_str(&path.into_inner()) else {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_token_id",
            "message": "Token id must be a UUID."
        }));
    };
    match runtime.store.revoke_api_token(&session.identity_name, id) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        })),
    }
}

// ---- terminal grants (plane plan Task 10, landed here) ----------------------
//
// The plane's durable `plt_` grants — the third mint path of the same
// store. Minting is behind a *session* only (the same rule as API tokens:
// no credential mints another). The workspace is ENGRAVED at mint and
// request headers never select or override it; the tool
// allowlist is floored (`NEVER_ON_THE_PLANE`) at mint and re-checked by
// `TerminalGrant::permits`. See `docs/components/magician/auth.md`.

#[derive(Debug, Deserialize)]
struct MintGrantRequest {
    label: String,
    workspace: String,
    agent_identity: String,
    /// Required and explicit — "magician" is a deliberate anti-pattern
    /// pin, never a silent fallback.
    harness_engine: String,
    #[serde(default)]
    allowed_tools: Vec<String>,
    ttl_hours: u64,
    #[serde(default)]
    max_usd: Option<f64>,
    #[serde(default)]
    max_wall_clock_secs: Option<u64>,
    #[serde(default)]
    max_concurrent_runs: Option<usize>,
}

async fn mint_grant_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<MintGrantRequest>,
) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    for (field, value) in [
        ("label", &body.label),
        ("workspace", &body.workspace),
        ("agent_identity", &body.agent_identity),
        ("harness_engine", &body.harness_engine),
    ] {
        if value.trim().is_empty() {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_request",
                "message": format!("{field} is required")
            }));
        }
    }
    if !(GRANT_TTL_MIN_HOURS..=GRANT_TTL_MAX_HOURS).contains(&body.ttl_hours) {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_ttl",
            "message": "ttl_hours must be 1..=2160 (an hour to 90 days)."
        }));
    }
    let requested_tools: Vec<String> = body
        .allowed_tools
        .iter()
        .map(|tool| tool.trim().to_string())
        .filter(|tool| !tool.is_empty())
        .collect();
    // The floor is not negotiable, but the drop is not silent either: the
    // mint response reports exactly what was removed.
    let floor_filtered = floored_tools(&requested_tools);
    let spec = MintGrantSpec {
        label: body.label.trim().to_string(),
        workspace: body.workspace.trim().to_string(),
        agent_identity: body.agent_identity.trim().to_string(),
        harness_engine: body.harness_engine.trim().to_string(),
        allowed_tools: requested_tools,
        ttl_hours: body.ttl_hours,
        max_usd: body.max_usd,
        max_wall_clock_secs: body.max_wall_clock_secs,
        max_concurrent_runs: body.max_concurrent_runs,
    };
    match runtime.store.mint_grant(&session.identity_name, spec) {
        Ok((grant, value)) => HttpResponse::Created().json(serde_json::json!({
            "id": grant.id.to_string(),
            "label": grant.label,
            "workspace": grant.workspace,
            "agent_identity": grant.agent_identity,
            "harness_engine": grant.harness_engine,
            "allowed_tools": grant.allowed_tools,  // post-floor
            "floor_filtered_tools": floor_filtered,
            "expires_at": grant.expires_at,
            // The only time the value is ever shown.
            "token": value,
        })),
        // Refused at mint, not discovered at dispatch: an engraved
        // workspace must be owned by the minting identity.
        Err(AuthError::UnownedWorkspace {
            principal,
            workspace,
        }) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unowned_workspace",
            "message": format!("Principal {principal} does not own workspace {workspace}.")
        })),
        Err(AuthError::InvalidGrantTtl(hours)) => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_ttl",
                "message": format!("ttl_hours {hours} is out of range: must be 1..=2160.")
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "grant_mint_failed",
            "message": error.to_string()
        })),
    }
}

async fn list_grants_handler(req: HttpRequest, runtime: web::Data<AuthRuntime>) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    match runtime.store.list_grants(&session.identity_name) {
        // No token value (never stored) and no hash either.
        Ok(grants) => HttpResponse::Ok().json(
            grants
                .into_iter()
                .map(|grant| {
                    serde_json::json!({
                        "id": grant.id.to_string(),
                        "label": grant.label,
                        "workspace": grant.workspace,
                        "agent_identity": grant.agent_identity,
                        "harness_engine": grant.harness_engine,
                        "allowed_tools": grant.allowed_tools,
                        "created_at": grant.created_at,
                        "expires_at": grant.expires_at,
                    })
                })
                .collect::<Vec<_>>(),
        ),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        })),
    }
}

async fn revoke_grant_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    path: web::Path<String>,
) -> impl Responder {
    let session = match require_session(&req) {
        Ok(session) => session,
        Err(response) => return response,
    };
    let Ok(id) = uuid::Uuid::parse_str(&path.into_inner()) else {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_grant_id",
            "message": "Grant id must be a UUID."
        }));
    };
    match runtime.store.revoke_grant(&session.identity_name, id) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        })),
    }
}

// ---- orphaned-scope reconciler (§6) -----------------------------------------
//
// Read-only. Reports scope roots with no owning identity **before** any
// enforcement flip, recognizing the system/eval principals that are
// legitimate non-login scopes. Deletes and moves nothing.

/// Non-login scope roots that live beside user data by design (measured in
/// production, 2026-08-07; workspace design §6).
const RECOGNIZED_SYSTEM_ROOTS: &[&str] = &["system", "live-eval", "local", "storage-live-eval"];

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn orphaned_scopes_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
) -> impl Responder {
    // Same treatment as the password door: the admin secret is guessed
    // against the shared sliding-window ledger before any comparison work.
    let peer = peer_of(&req);
    if let Some(retry_after_secs) = admin_secret_throttle().check(ADMIN_SECRET_THROTTLE_KEY, &peer)
    {
        return HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", retry_after_secs.to_string()))
            .json(serde_json::json!({
                "error": "too_many_attempts",
                "message": "Too many failed admin attempts. Try again later.",
                "retry_after_secs": retry_after_secs
            }));
    }
    let admin_secret = std::env::var("MAGICIAN_ADMIN_SECRET").ok();
    let Some(secret) = admin_secret else {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "admin_secret_not_configured",
            "message": "Set MAGICIAN_ADMIN_SECRET to use the reconciler."
        }));
    };
    let presented = req
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), format!("Bearer {secret}").as_bytes()) {
        admin_secret_throttle().record_failure(ADMIN_SECRET_THROTTLE_KEY, &peer);
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "invalid_admin_secret"
        }));
    }
    admin_secret_throttle().record_success(ADMIN_SECRET_THROTTLE_KEY, &peer);
    let scopes_root = runtime.store.runtime_root().join("scopes");
    let owned = runtime.store.scope_roots().unwrap_or_default();
    let mut orphaned: Vec<String> = Vec::new();
    let mut recognized: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(&scopes_root) {
        Ok(entries) => entries,
        Err(_) => {
            return HttpResponse::Ok().json(serde_json::json!({
                "orphaned_scope_roots": [],
                "recognized_system_roots": [],
                "owned_scope_roots": owned,
            }));
        },
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !entry.metadata().map(|meta| meta.is_dir()).unwrap_or(false) {
            continue;
        }
        if owned.iter().any(|root| root == &name) {
            continue;
        }
        if RECOGNIZED_SYSTEM_ROOTS.contains(&name.as_str()) {
            recognized.push(name);
        } else {
            orphaned.push(name);
        }
    }
    orphaned.sort();
    recognized.sort();
    HttpResponse::Ok().json(serde_json::json!({
        "orphaned_scope_roots": orphaned,
        "recognized_system_roots": recognized,
        "owned_scope_roots": owned,
    }))
}

// ---- workspace CRUD (§7A) ---------------------------------------------------
//
// These surfaces always require an authenticated bearer, in either mode.
// Open mode exposes local anonymous/default only until the first identity is
// created; it never treats caller scope headers as identity.

fn authenticated_identity(
    req: &HttpRequest,
    runtime: &web::Data<AuthRuntime>,
) -> Result<magician::magician_v2::auth::Identity, HttpResponse> {
    let stamped = authenticated(req).ok_or_else(|| {
        HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }))
    })?;
    let Some(identity_name) = stamped.identity_name.as_deref() else {
        return Err(HttpResponse::Forbidden().json(serde_json::json!({
            "error": "session_required",
            "message": "This route needs a login identity; a bot token carries none."
        })));
    };
    match runtime.store.find_identity(identity_name) {
        Ok(Some(identity)) => Ok(identity),
        Ok(None) => Err(HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "The session's identity no longer exists."
        }))),
        Err(error) => Err(HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "auth_store_failure",
            "message": error.to_string()
        }))),
    }
}

fn auth_error_response(error: AuthError) -> HttpResponse {
    match error {
        AuthError::InvalidPrincipalName(name) => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_workspace_slug",
                "message": format!(
                    "Workspace slug {name:?} must be 1-32 chars of a-z, 0-9, '_' or '-'."
                )
            }))
        },
        AuthError::WorkspaceExists(slug) => HttpResponse::Conflict().json(serde_json::json!({
            "error": "workspace_exists",
            "message": format!("Workspace {slug:?} already exists.")
        })),
        AuthError::UnownedWorkspace {
            principal,
            workspace,
        } => HttpResponse::NotFound().json(serde_json::json!({
            "error": "workspace_not_found",
            "message": format!("Principal {principal} does not own workspace {workspace}.")
        })),
        AuthError::DefaultWorkspaceProtected => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "default_workspace_protected",
                "message": "The default workspace cannot be deleted or renamed."
            }))
        },
        AuthError::WorkspaceHasLiveState(slug) => {
            HttpResponse::Conflict().json(serde_json::json!({
                "error": "workspace_has_live_state",
                "message": format!(
                    "Workspace {slug:?} still has live state on disk; clear it before deleting."
                )
            }))
        },
        AuthError::WorkspacePendingPurge(slug) => {
            HttpResponse::Conflict().json(serde_json::json!({
                "error": "workspace_pending_purge",
                "message": format!(
                    "Workspace {slug:?} was deleted with its data; the name is free again after Magician restarts."
                )
            }))
        },
        other => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "workspace_registry_failure",
            "message": other.to_string()
        })),
    }
}

/// Best-effort summary card counts (§7A, March's shape). Directory-level
/// reads only — no store parses. `frozen` stays `false` until a
/// resource-authority freeze marker gains a cheap read path.
fn workspace_summary(
    workspace_dir: &std::path::Path,
) -> (u64, u64, bool, Option<chrono::DateTime<chrono::Utc>>) {
    let count_entries = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().count() as u64)
            .unwrap_or(0)
    };
    let agent_count = count_entries(&workspace_dir.join("agent_runtime").join("definitions"));
    let task_count = count_entries(&workspace_dir.join("tasks"));
    let last_activity = std::fs::read_dir(workspace_dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok()?.modified().ok())
                .max()
        })
        .and_then(|mtime| {
            chrono::DateTime::from_timestamp(
                mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                0,
            )
        });
    (agent_count, task_count, false, last_activity)
}

#[derive(Debug, Serialize)]
struct WorkspaceCard {
    id: String,
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_default: bool,
    agent_count: u64,
    active_task_count: u64,
    frozen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity: Option<chrono::DateTime<chrono::Utc>>,
}

async fn list_workspaces_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
) -> impl Responder {
    let identity = match authenticated_identity(&req, &runtime) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let scopes_root = runtime.store.runtime_root().join("scopes");
    match workspace_registry::list(&scopes_root, &identity) {
        Ok(workspaces) => {
            let cards = workspaces
                .into_iter()
                .map(|workspace| {
                    let (agent_count, active_task_count, frozen, last_activity) = workspace_summary(
                        &scopes_root.join(&identity.scope_root).join(&workspace.id),
                    );
                    WorkspaceCard {
                        id: workspace.id,
                        display_name: workspace.display_name,
                        description: workspace.description,
                        is_default: workspace.is_default,
                        agent_count,
                        active_task_count,
                        frozen,
                        last_activity,
                    }
                })
                .collect::<Vec<_>>();
            HttpResponse::Ok().json(cards)
        },
        Err(error) => auth_error_response(AuthError::from(error)),
    }
}

#[derive(Debug, Deserialize)]
struct CreateWorkspaceRequest {
    slug: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_workspace_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    body: web::Json<CreateWorkspaceRequest>,
) -> impl Responder {
    let identity = match authenticated_identity(&req, &runtime) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let slug = body.slug.trim();
    if slug.is_empty() || body.display_name.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "slug and display_name are required"
        }));
    }
    let scopes_root = runtime.store.runtime_root().join("scopes");
    match workspace_registry::create(
        &scopes_root,
        &identity,
        slug,
        &body.display_name,
        body.description.clone(),
    ) {
        Ok(workspace) => HttpResponse::Created().json(serde_json::json!({
            "id": workspace.id,
            "display_name": workspace.display_name,
            "description": workspace.description,
            "is_default": workspace.is_default,
        })),
        Err(error) => auth_error_response(error),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateWorkspaceRequest {
    #[serde(default)]
    display_name: Option<String>,
    /// `null` clears the description; absent leaves it untouched.
    #[serde(default, deserialize_with = "deserialize_explicit_option")]
    description: Option<Option<String>>,
}

fn deserialize_explicit_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

async fn update_workspace_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    path: web::Path<String>,
    body: web::Json<UpdateWorkspaceRequest>,
) -> impl Responder {
    let identity = match authenticated_identity(&req, &runtime) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let scopes_root = runtime.store.runtime_root().join("scopes");
    match workspace_registry::update(
        &scopes_root,
        &identity,
        &path.into_inner(),
        body.display_name.clone(),
        body.description.clone(),
    ) {
        Ok(workspace) => HttpResponse::Ok().json(serde_json::json!({
            "id": workspace.id,
            "display_name": workspace.display_name,
            "description": workspace.description,
            "is_default": workspace.is_default,
        })),
        Err(error) => auth_error_response(error),
    }
}

#[derive(Debug, Default, Deserialize)]
struct DeleteWorkspaceQuery {
    /// Delete the workspace's data too. Without it the delete refuses a
    /// workspace that still holds files.
    #[serde(default)]
    purge: bool,
}

async fn delete_workspace_handler(
    req: HttpRequest,
    runtime: web::Data<AuthRuntime>,
    learning: Option<web::Data<AttentionLearningService>>,
    resurfacing: Option<web::Data<ResurfacingStore>>,
    path: web::Path<String>,
    query: web::Query<DeleteWorkspaceQuery>,
) -> impl Responder {
    let identity = match authenticated_identity(&req, &runtime) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let scopes_root = runtime.store.runtime_root().join("scopes");
    let workspace = path.into_inner();
    // `purge` removes the registry row now and the directory at the next
    // start (see `workspace_registry::drain_pending_purges` for why not now).
    let purge = query.purge;
    let deleted = if purge {
        workspace_registry::delete_and_schedule_purge(&scopes_root, &identity, &workspace)
    } else {
        workspace_registry::delete(&scopes_root, &identity, &workspace)
    };
    if let Err(error) = deleted {
        return auth_error_response(error);
    }
    // The registry row is gone — and either the scope held no files, or it is
    // queued for removal at the next start. Retire what keyed off it: scope discovery reads
    // these databases, so a row left here re-proposes the workspace on the
    // next pass and the deletion does not stick. Best effort by design — the
    // workspace IS deleted, and a store that is unavailable must not turn that
    // into a failure the caller could misread as "still there". A leftover row
    // is inert on its own (see the discovery filter), so the cost of a failure
    // here is residue, not resurrection.
    let principal = identity.scope_root.as_str();
    if let Some(learning) = learning.as_ref() {
        // `delete_scope` already knows this schema, including the
        // `attention_delivery_*` tables that are keyed through a decision id
        // rather than carrying the scope themselves. It had no production
        // caller until now.
        match learning
            .store()
            .delete_scope(principal, &workspace, true)
            .await
        {
            Ok(report) => tracing::info!(
                principal,
                workspace,
                rows = report.affected_rows.values().sum::<u64>(),
                tables = report.affected_rows.len(),
                "retired attention learning rows for a deleted workspace"
            ),
            Err(error) => tracing::warn!(
                principal,
                workspace,
                error = %error,
                "could not retire attention learning rows for a deleted workspace"
            ),
        }
    }
    if let Some(resurfacing) = resurfacing.as_ref() {
        match resurfacing.retire_scope(principal, &workspace).await {
            Ok(rows) => tracing::info!(
                principal,
                workspace,
                rows,
                "retired resurfacing rows for a deleted workspace"
            ),
            Err(error) => tracing::warn!(
                principal,
                workspace,
                error = %error,
                "could not retire resurfacing rows for a deleted workspace"
            ),
        }
    }
    if purge {
        // 202, not 204: the row and the database rows are gone, but the
        // directory is removed at the next start, and a caller should be told.
        return HttpResponse::Accepted().json(serde_json::json!({
            "workspace": workspace,
            "deleted": true,
            "data_removal": "scheduled_for_next_start",
        }));
    }
    HttpResponse::NoContent().finish()
}

#[cfg(test)]
mod tests {
    use super::{auth_runtime_with_http, configure_auth_routes};
    use actix_web::dev::Service;
    use actix_web::middleware::from_fn;
    use actix_web::{test, web, App, HttpRequest, HttpResponse};
    use magician::config::{AuthConfig, AuthMode};
    use magician::magician_v2::auth::middleware::{authenticate_request, AuthRuntime};
    use magician::magician_v2::auth::{social, AuthStore};
    use magician::magician_v2::cloudflare_access::verify_access_middleware;

    /// Offline social transport: any exchange yields a token; the Google
    /// userinfo returns a fixed subject. No flow test touches the network.
    struct FakeSocialHttp;

    #[async_trait::async_trait]
    impl social::SocialHttp for FakeSocialHttp {
        async fn post_form_json(
            &self,
            _url: &str,
            _client_id: &str,
            _client_secret: &str,
            _form: &[(&'static str, String)],
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({"access_token": "fake-at", "token_type": "Bearer"}))
        }
        async fn get_bearer_json(
            &self,
            url: &str,
            _token: &str,
        ) -> Result<serde_json::Value, String> {
            if url.contains("userinfo") {
                Ok(serde_json::json!({"sub": "google-sub-1", "name": "Ada Lovelace"}))
            } else if url.contains("/user") {
                Ok(serde_json::json!({"id": 63647, "login": "ada"}))
            } else {
                Ok(serde_json::json!([]))
            }
        }
    }

    fn runtime(dir: &std::path::Path, mode: AuthMode) -> web::Data<AuthRuntime> {
        let store = std::sync::Arc::new(AuthStore::open(dir).expect("store"));
        let config = AuthConfig {
            mode,
            ..AuthConfig::default()
        };
        auth_runtime_with_http(store, config, std::sync::Arc::new(FakeSocialHttp))
    }

    fn runtime_with_config(config: AuthConfig, dir: &std::path::Path) -> web::Data<AuthRuntime> {
        let store = std::sync::Arc::new(AuthStore::open(dir).expect("store"));
        auth_runtime_with_http(store, config, std::sync::Arc::new(FakeSocialHttp))
    }

    /// The full gate stack exactly as main.rs wires it: auth middleware
    /// outermost, then the auth routes, then a probe route that echoes the
    /// engraved scope values a downstream handler would resolve.
    macro_rules! auth_app {
        ($dir:expr, $mode:expr) => {{
            let auth = runtime(&$dir, $mode);
            test::init_service(
                App::new().app_data(auth).service(
                    web::scope("/api/magician/v2")
                        .wrap(from_fn(authenticate_request))
                        .configure(configure_auth_routes)
                        .route(
                            "/probe",
                            web::get().to(|req: HttpRequest| async move {
                                let header = |name: &str| {
                                    req.headers()
                                        .get(name)
                                        .and_then(|value| value.to_str().ok())
                                        .unwrap_or("<none>")
                                        .to_string()
                                };
                                HttpResponse::Ok().json(serde_json::json!({
                                    "principal": header("x-principal"),
                                    "workspace": header("x-workspace"),
                                }))
                            }),
                        ),
                ),
            )
            .await
        }};
    }

    macro_rules! auth_app_with_config {
        ($dir:expr, $config:expr) => {{
            let auth = runtime_with_config($config, &$dir);
            test::init_service(
                App::new().app_data(auth).service(
                    web::scope("/api/magician/v2")
                        .wrap(from_fn(authenticate_request))
                        .configure(configure_auth_routes)
                        .route(
                            "/probe",
                            web::get().to(|req: HttpRequest| async move {
                                let header = |name: &str| {
                                    req.headers()
                                        .get(name)
                                        .and_then(|value| value.to_str().ok())
                                        .unwrap_or("<none>")
                                        .to_string()
                                };
                                HttpResponse::Ok().json(serde_json::json!({
                                    "principal": header("x-principal"),
                                    "workspace": header("x-workspace"),
                                }))
                            }),
                        ),
                ),
            )
            .await
        }};
    }

    macro_rules! login_token {
        ($app:expr) => {{
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/auth/login")
                .set_json(serde_json::json!({"username": "owner", "password": "pw"}))
                .to_request();
            let response = test::call_service(&$app, request).await;
            assert_eq!(response.status().as_u16(), 201, "first login bootstraps the identity");
            let body: serde_json::Value =
                serde_json::from_slice(&test::read_body(response).await).unwrap();
            body["token"].as_str().unwrap().to_string()
        }};
    }

    macro_rules! probe {
        ($app:expr, $bearer:expr, $principal:expr, $workspace:expr) => {{
            let mut request = test::TestRequest::get().uri("/api/magician/v2/probe");
            if let Some(bearer) = $bearer {
                request = request.insert_header(("Authorization", format!("Bearer {bearer}")));
            }
            if !$principal.is_empty() {
                request = request.insert_header(("X-Principal", $principal));
            }
            if !$workspace.is_empty() {
                request = request.insert_header(("X-Workspace", $workspace));
            }
            let response = match $app.call(request.to_request()).await {
                Ok(response) => response.into_parts().1,
                Err(error) => error.error_response(),
            };
            let status = response.status().as_u16();
            let bytes = actix_web::body::to_bytes(response.into_body())
                .await
                .expect("body buffered");
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, body)
        }};
    }

    #[actix_web::test]
    async fn login_logout_session_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let token = login_token!(app);

        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["identity"]["name"], "owner");
        assert_eq!(body["principal"], "anonymous");
        assert_eq!(body["workspace"], "default");
        assert_eq!(body["workspaces"][0]["id"], "default");

        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/logout")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 204);

        // Revoked: the next request carrying that now-invalid bearer 401s;
        // open-mode fallback applies only when no bearer was presented.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(response.status().as_u16(), 401);
    }

    /// Both gates in the order main.rs wires them: the auth gate runs first
    /// and engraves the session, then the access layer sees the same request.
    /// A browser session is a bearer with no device-id header, and the access
    /// layer must leave it alone; it once read every bare bearer as an
    /// incomplete mobile credential and rejected each session as it was minted.
    #[actix_web::test]
    async fn session_bearer_passes_the_access_layer_without_a_device_id() {
        let dir = tempfile::tempdir().unwrap();
        let auth = runtime(dir.path(), AuthMode::Open);
        let app = test::init_service(
            App::new().app_data(auth).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(verify_access_middleware))
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes),
            ),
        )
        .await;
        let token = login_token!(app);

        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["identity"]["name"], "owner");

        // A device id with no bearer is still an incomplete device credential.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("X-Magician-Device-Id", "device-1"))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(response.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn wrong_password_is_401_and_never_creates() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let _ = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "owner", "password": "wrong"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 401);
        // A second identity never bootstraps from login once one exists.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "intruder", "password": "pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            401,
            "login never creates after the first identity"
        );
    }

    #[actix_web::test]
    async fn open_mode_without_bearer_ignores_caller_scope() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let (status, body) = probe!(app, Option::<&str>::None, "whoever", "default");
        assert_eq!(status, 200);
        assert_eq!(body["principal"], "anonymous");
        assert_eq!(body["workspace"], "default");
    }

    #[actix_web::test]
    async fn open_mode_closes_once_an_identity_exists() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        // The first login bootstraps the owner identity adopting scopes/anonymous.
        let token = login_token!(app);

        // Bearerless access is now refused even in open mode: the fallback is
        // a bootstrap posture, and the owner's adopted tree must not stay
        // reachable without a credential until someone flips the config.
        let (status, _) = probe!(app, Option::<&str>::None, "whoever", "default");
        assert_eq!(
            status, 401,
            "open mode must not outlive the identity it was scaffolding"
        );

        // The bearer still works, and the public login route stays reachable
        // so recovery is always possible without a config edit.
        let (status, body) = probe!(app, Some(&token), "", "");
        assert_eq!(status, 200);
        assert_eq!(body["principal"], "anonymous");
        assert_eq!(body["workspace"], "default");
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "owner", "password": "pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201, "login stays public");
    }

    #[actix_web::test]
    async fn owner_provisions_members_who_log_into_their_own_scope() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let owner = login_token!(app);

        // Provisioning needs the owner session.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(response.status().as_u16(), 401, "bearerless is refused");

        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["identity"]["name"], "alice");
        assert_eq!(
            body["identity"]["scope_root"], "alice",
            "only the first identity adopts anonymous; members get their own root"
        );
        assert!(
            body.get("token").is_none(),
            "provisioning returns no token — the member signs in themselves"
        );

        // Duplicate name is a conflict, not an overwrite.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "other"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 409);

        // The member signs in and lands in their own scope, not the owner's.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "alice");
        assert_eq!(body["workspace"], "default");
        let alice = body["token"].as_str().unwrap().to_string();

        // A member cannot provision further identities, and cannot name the
        // owner's workspaces: ownership is the only authorization.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({"username": "bob", "password": "pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 403, "only the owner provisions");
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/session/scope")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({"workspace": "company"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_ne!(
            response.status().as_u16(),
            200,
            "a member cannot switch into a workspace they do not own"
        );
    }

    #[actix_web::test]
    async fn member_provisioning_respects_allow_signup() {
        let dir = tempfile::tempdir().unwrap();
        // Bootstrap the owner while signup is allowed, then re-open the same
        // store with signup off: an existing owner must not be able to add
        // members past the closed gate.
        let bootstrap_app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let owner = login_token!(bootstrap_app);
        drop(bootstrap_app);

        let mut config = AuthConfig::default();
        config.allow_signup = false;
        let app = auth_app_with_config!(dir.path().to_path_buf(), config);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 403);
    }

    /// The login throttle is a process-static: parallel tests share it, so
    /// each throttle test resets it and uses line-unique usernames to stay
    /// off every other test's ledger. The shared per-peer ledger ("unknown"
    /// in tests without a peer_addr) is left alone — total failures across
    /// the suite stay far under the peer cap.
    fn reset_login_throttle_for_tests() {
        let throttle = super::login_throttle();
        if let Ok(mut ledger) = throttle.by_username.lock() {
            ledger.clear();
        }
        if let Ok(mut ledger) = throttle.by_peer.lock() {
            ledger.clear();
        }
    }

    #[actix_web::test]
    async fn login_throttle_blocks_after_repeated_failures_and_clears_on_success() {
        reset_login_throttle_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let _ = login_token!(app); // owner exists; failures below are refused, not bootstraps

        let victim = "throttle-victim";
        for _ in 0..8 {
            let wrong = test::TestRequest::post()
                .uri("/api/magician/v2/auth/login")
                .set_json(serde_json::json!({"username": victim, "password": "nope"}))
                .to_request();
            let response = test::call_service(&app, wrong).await;
            assert_eq!(
                response.status().as_u16(),
                401,
                "failures are 401 until the cap"
            );
        }

        // The cap reached: even the (unknown) username's next attempt is a
        // cheap 429 with a Retry-After, before any argon2 work.
        let blocked = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": victim, "password": "nope"}))
            .to_request();
        let response = test::call_service(&app, blocked).await;
        assert_eq!(response.status().as_u16(), 429);
        assert!(response.headers().contains_key("Retry-After"));

        // Per-username isolation: another username on the same peer is fine.
        let bystander = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "owner", "password": "pw"}))
            .to_request();
        let response = test::call_service(&app, bystander).await;
        assert_eq!(
            response.status().as_u16(),
            201,
            "the owner is not collateral"
        );

        // Success clears the username's window: 7 failures, one success,
        // 7 more failures — never reaching the cap of 8.
        let tired = "throttle-tired";
        for round in 0..2 {
            for _ in 0..7 {
                let request = test::TestRequest::post()
                    .uri("/api/magician/v2/auth/login")
                    .set_json(serde_json::json!({"username": tired, "password": "nope"}))
                    .to_request();
                let response = test::call_service(&app, request).await;
                assert_eq!(response.status().as_u16(), 401, "round {round} failure");
            }
            if round == 0 {
                // Provision `tired` mid-test so the success round can sign in.
                let owner = login_token!(app);
                let request = test::TestRequest::post()
                    .uri("/api/magician/v2/auth/identities")
                    .insert_header(("Authorization", format!("Bearer {owner}")))
                    .set_json(serde_json::json!({
                        "username": tired, "password": "right-pw"
                    }))
                    .to_request();
                let response = test::call_service(&app, request).await;
                assert_eq!(response.status().as_u16(), 201);
                let request = test::TestRequest::post()
                    .uri("/api/magician/v2/auth/login")
                    .set_json(serde_json::json!({"username": tired, "password": "right-pw"}))
                    .to_request();
                let response = test::call_service(&app, request).await;
                assert_eq!(response.status().as_u16(), 201, "success after 7 failures");
            }
        }
    }

    #[actix_web::test]
    async fn member_provisioning_requires_a_session_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let owner = login_token!(app);

        // The owner mints an API token — a perfectly valid owner bearer.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/tokens")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"label": "CI"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let pat = body["token"].as_str().unwrap().to_string();

        // Identity creation refuses it: automation credentials do not make
        // new humans, however legitimate their owner is.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {pat}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 403, "PAT cannot provision");

        // The owner's session still can.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201, "the session can");
    }

    #[actix_web::test]
    async fn enrollment_auto_approve_assigns_the_callers_principal() {
        let dir = tempfile::tempdir().unwrap();
        // The enrollment API on the same runtime root as the auth store:
        // one scopes/ tree, so the enrolled records land where identities live.
        let enrollment = web::Data::new(crate::enrollment_api::EnrollmentApi::new(
            magician::magician_v2::chat::enrollment::EnrollmentStoreResolver::new(
                magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                    dir.path().to_path_buf(),
                ),
            ),
            magician::config::EnrollmentConfig {
                auto_approve: true,
                default_principal: "anonymous".to_string(),
                pending_ttl_hours: 24,
            },
        ));
        let auth = runtime(&dir.path().to_path_buf(), AuthMode::Open);
        let app = test::init_service(
            App::new().app_data(auth).app_data(enrollment).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/chat/enroll",
                        web::post().to(crate::enrollment_api::enroll_handler),
                    )
                    .route(
                        "/chat/enroll/status",
                        web::get().to(crate::enrollment_api::enrollment_status_handler),
                    ),
            ),
        )
        .await;

        let owner = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let alice = body["token"].as_str().unwrap().to_string();

        // The member's channel auto-approves into the MEMBER's tree — not
        // default_principal, which is where every auto-approval pointed
        // before and bled member channels into the owner's scope.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "chat-alice-1"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], true);
        assert_eq!(body["principal"], "alice");

        // End-to-end resolution: the status resolver scans per-principal
        // trees and must find the record where the enroll put it — alice's
        // tree, not the default registry.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/chat/enroll/status?channel_type=telegram&channel_address=chat-alice-1")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], true);
        assert_eq!(
            body["principal"], "alice",
            "the member's record resolves from the member's tree"
        );

        // The owner enrolling the same kind of channel lands in the owner's
        // adopted root — the config default is the caller-scoped answer now.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "chat-owner-1"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "anonymous");

        // Cross-tree takeover guard: the member re-enrolls the OWNER's
        // channel address. Member trees are separate stores, so without the
        // global pre-scan this would create a duplicate — and the inbound
        // resolver scans principals alphabetically, where "alice" beats
        // "anonymous". It must get the existing mapping back instead.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "chat-owner-1"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(
            body["enrolled"], true,
            "an existing mapping is idempotent, not re-mintable"
        );
        assert_eq!(
            body["principal"], "anonymous",
            "the owner's channel does not reroute to the member"
        );
        // And resolution agrees: the address still routes to the owner.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/chat/enroll/status?channel_type=telegram&channel_address=chat-owner-1")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "anonymous");
    }

    #[actix_web::test]
    async fn pending_enrollments_stay_in_the_default_tree_for_admin_approval() {
        let dir = tempfile::tempdir().unwrap();
        let enrollment = web::Data::new(crate::enrollment_api::EnrollmentApi::new(
            magician::magician_v2::chat::enrollment::EnrollmentStoreResolver::new(
                magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                    dir.path().to_path_buf(),
                ),
            ),
            magician::config::EnrollmentConfig {
                auto_approve: false,
                default_principal: "anonymous".to_string(),
                pending_ttl_hours: 24,
            },
        ));
        let auth = runtime(&dir.path().to_path_buf(), AuthMode::Open);
        let app = test::init_service(
            App::new().app_data(auth).app_data(enrollment).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/chat/enroll",
                        web::post().to(crate::enrollment_api::enroll_handler),
                    )
                    .route(
                        "/chat/enroll/status",
                        web::get().to(crate::enrollment_api::enrollment_status_handler),
                    )
                    .route(
                        "/chat/enroll/approve",
                        web::post().to(crate::enrollment_api::approve_handler),
                    ),
            ),
        )
        .await;

        let owner = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let alice = body["token"].as_str().unwrap().to_string();

        // auto_approve off: the member's enroll answers with a code, and
        // the pending record must sit in the DEFAULT tree — the only tree
        // the admin approve flow scans. A member-tree pending record would
        // be silently unapprovable.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "chat-alice-pending"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], false);
        let code = body["code"].as_str().unwrap().to_string();

        // The code travels exactly once — in this creation response. A
        // status query for the same address (by any member) answers
        // 'pending' with no code at all.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/chat/enroll/status?channel_type=telegram&channel_address=chat-alice-pending")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], false, "pending is visible as pending");
        assert!(
            body.get("code").is_none(),
            "status never repeats a pending code — it is shown once, at creation"
        );

        // The admin approves it into the member's tree — proving the code
        // was where the approve flow looks.
        std::env::set_var("MAGICIAN_ADMIN_SECRET", "test-admin-secret");
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/approve")
            .insert_header(("Authorization", "Bearer test-admin-secret"))
            .set_json(serde_json::json!({"code": code, "principal": "alice"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], true);
        assert_eq!(body["principal"], "alice");
    }

    #[actix_web::test]
    async fn revoke_unenrolls_own_channels_and_the_owner_administers_any() {
        let dir = tempfile::tempdir().unwrap();
        let enrollment = web::Data::new(crate::enrollment_api::EnrollmentApi::new(
            magician::magician_v2::chat::enrollment::EnrollmentStoreResolver::new(
                magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                    dir.path().to_path_buf(),
                ),
            ),
            magician::config::EnrollmentConfig {
                auto_approve: true,
                default_principal: "anonymous".to_string(),
                pending_ttl_hours: 24,
            },
        ));
        let auth = runtime(&dir.path().to_path_buf(), AuthMode::Open);
        let app = test::init_service(
            App::new().app_data(auth).app_data(enrollment).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/chat/enroll",
                        web::post().to(crate::enrollment_api::enroll_handler),
                    )
                    .route(
                        "/chat/enroll/status",
                        web::get().to(crate::enrollment_api::enrollment_status_handler),
                    )
                    .route(
                        "/chat/enroll/revoke",
                        web::post().to(crate::enrollment_api::revoke_handler),
                    ),
            ),
        )
        .await;

        // Owner + member, both enrolled.
        let owner = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let alice = body["token"].as_str().unwrap().to_string();
        let owner_channel = "chat-owner-rv";
        let alice_channel = "chat-alice-rv";
        for (bearer, address) in [
            (owner.clone(), owner_channel),
            (alice.clone(), alice_channel),
        ] {
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/chat/enroll")
                .insert_header(("Authorization", format!("Bearer {bearer}")))
                .set_json(serde_json::json!({
                    "channel_type": "telegram", "channel_address": address
                }))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status().as_u16(), 200);
        }

        // A member cannot revoke another principal's channel.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/revoke")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": owner_channel
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            403,
            "only the holder or the owner"
        );

        // The holder revokes their own.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/revoke")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": alice_channel
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["revoked"], true);
        assert_eq!(body["principal"], "alice");

        // Reassignment: the owner revokes their own channel, then the
        // member claims it — the pre-scan no longer blocks because the
        // mapping is gone.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/revoke")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": owner_channel
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": owner_channel
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(
            body["principal"], "alice",
            "a revoked channel is claimable — revoke-then-enroll is the reassignment flow"
        );

        // The owner (interim admin) revokes the member's held channel.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/revoke")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": owner_channel
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "alice", "the owner administers anyone's");

        // Revoking an unknown channel is a 404, not a 200-with-lie.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/revoke")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "never-existed"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 404);
    }

    #[actix_web::test]
    async fn pending_cancellation_by_code_and_owner_by_channel() {
        let dir = tempfile::tempdir().unwrap();
        let enrollment = web::Data::new(crate::enrollment_api::EnrollmentApi::new(
            magician::magician_v2::chat::enrollment::EnrollmentStoreResolver::new(
                magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
                    dir.path().to_path_buf(),
                ),
            ),
            magician::config::EnrollmentConfig {
                auto_approve: false,
                default_principal: "anonymous".to_string(),
                pending_ttl_hours: 24,
            },
        ));
        let auth = runtime(&dir.path().to_path_buf(), AuthMode::Open);
        let app = test::init_service(
            App::new().app_data(auth).app_data(enrollment).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/chat/enroll",
                        web::post().to(crate::enrollment_api::enroll_handler),
                    )
                    .route(
                        "/chat/enroll/status",
                        web::get().to(crate::enrollment_api::enrollment_status_handler),
                    )
                    .route(
                        "/chat/enroll/cancel",
                        web::post().to(crate::enrollment_api::cancel_handler),
                    ),
            ),
        )
        .await;

        let owner = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/identities")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/login")
            .set_json(serde_json::json!({"username": "alice", "password": "family-pw"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let alice = body["token"].as_str().unwrap().to_string();

        // Two pendings: one alice cancels by code, one the owner cancels
        // by channel identity.
        let mut codes = Vec::new();
        for address in ["cancel-by-code-1", "cancel-by-owner-1"] {
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/chat/enroll")
                .insert_header(("Authorization", format!("Bearer {alice}")))
                .set_json(serde_json::json!({
                    "channel_type": "telegram", "channel_address": address
                }))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status().as_u16(), 200);
            let body: serde_json::Value = test::read_body_json(response).await;
            codes.push(body["code"].as_str().unwrap().to_string());
        }

        // Capability arm: presenting the code cancels it.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/cancel")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({"code": codes[0]}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["cancelled"], true);
        assert_eq!(body["channel_address"], "cancel-by-code-1");

        // The cancelled pending is gone: its code is dead, its status unknown.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/cancel")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({"code": codes[0]}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            404,
            "a code dies with its pending"
        );
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/chat/enroll/status?channel_type=telegram&channel_address=cancel-by-code-1")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["enrolled"], false);

        // A member cannot use the owner's by-channel arm.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/cancel")
            .insert_header(("Authorization", format!("Bearer {alice}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "cancel-by-owner-1"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            403,
            "by-channel is the owner's arm"
        );

        // The owner cancels by channel identity without ever holding the code.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/cancel")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({
                "channel_type": "telegram", "channel_address": "cancel-by-owner-1"
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["cancelled"], true);
        assert_eq!(body["channel_address"], "cancel-by-owner-1");

        // Neither arm given is a 400.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/chat/enroll/cancel")
            .insert_header(("Authorization", format!("Bearer {owner}")))
            .set_json(serde_json::json!({}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);
    }

    #[actix_web::test]
    async fn open_mode_with_valid_bearer_engraves_proven_scope() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let token = login_token!(app);
        // The client tries to name someone else alongside the token.
        let (status, body) = probe!(app, Some(&token), "eve", "");
        assert_eq!(status, 200);
        assert_eq!(
            body["principal"], "anonymous",
            "engraved from the session, aliasing the adopted anonymous root"
        );
        assert_eq!(body["workspace"], "default");
    }

    #[actix_web::test]
    async fn credentials_mode_requires_bearer_and_overrides_client_claims() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Credentials);
        // Bootstrap happens on the public login route even in credentials mode.
        let token = login_token!(app);

        let (status, _) = probe!(app, Option::<&str>::None, "eve", "");
        assert_eq!(status, 401);

        let (status, body) = probe!(app, Some(&token), "eve", "");
        assert_eq!(status, 200);
        assert_eq!(
            body["principal"], "anonymous",
            "client-claimed principal is overwritten"
        );
    }

    #[actix_web::test]
    async fn terminal_grant_is_refused_as_a_general_api_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Credentials);
        let session = login_token!(app);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .set_json(serde_json::json!({
                "label": "review terminal",
                "workspace": "default",
                "agent_identity": "review-agent",
                "harness_engine": "magician",
                "allowed_tools": ["read_file"],
                "ttl_hours": 1
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value = test::read_body_json(response).await;
        let grant = body["token"].as_str().expect("one-time grant value");

        let (status, _) = probe!(app, Some(grant), "", "");
        assert_eq!(status, 401, "a plane grant cannot call arbitrary REST APIs");
    }

    #[actix_web::test]
    async fn credentials_mode_401_carries_www_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Credentials);
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/probe")
            .insert_header(("X-Principal", "eve"))
            .to_request();
        // The middleware rejects with InternalError::from_response — the
        // test helper panics on Err, so drive the service directly and
        // recover the carried response.
        let error = app.call(request).await.expect_err("rejected");
        let response = error.error_response();
        assert_eq!(response.status().as_u16(), 401);
        assert_eq!(
            response
                .headers()
                .get("WWW-Authenticate")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer")
        );
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("body buffered");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "authentication_required");
    }

    /// The wave-1 Task 10 promise, made concrete: streaming handshakes (SSE
    /// and WebSocket upgrades are ordinary HTTP requests first) pass the
    /// auth gate at handshake — a credentials-mode handshake without a
    /// bearer is rejected before any stream begins; with a valid session
    /// bearer it reaches the streaming handler.
    #[actix_web::test]
    async fn streaming_handshake_passes_the_auth_gate_before_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Credentials);
        let token = login_token!(app);

        // A streaming route in the same scope, shaped like the SSE/WS
        // handshake surface: the middleware wraps it like any other route.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/probe")
            .insert_header(("Accept", "text/event-stream"))
            .to_request();
        let error = app
            .call(request)
            .await
            .expect_err("handshake without bearer is rejected");
        let response = error.error_response();
        assert_eq!(
            response.status().as_u16(),
            401,
            "rejected before the stream starts"
        );
        assert_eq!(
            response
                .headers()
                .get("WWW-Authenticate")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer")
        );

        // With the bearer, the handshake reaches the handler.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/probe")
            .insert_header(("Accept", "text/event-stream"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(
            response.status().as_u16(),
            200,
            "authenticated handshake streams"
        );
    }

    #[actix_web::test]
    async fn caller_workspace_header_cannot_change_a_valid_bearer_scope() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let token = login_token!(app);
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/probe")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("X-Workspace", "not-owned"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "anonymous");
        assert_eq!(body["workspace"], "default");
    }

    #[actix_web::test]
    async fn workspace_crud_roundtrip_with_summary_cards() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let token = login_token!(app);
        let auth = format!("Bearer {token}");

        // Unauthenticated access is refused even in open mode: workspace
        // administration requires an authenticated owner identity. Since the
        // open-mode tighten this 401 comes from the MIDDLEWARE (an Err
        // short-circuit), so the call tolerates an error response the same
        // way the logout test does.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/workspaces")
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(response.status().as_u16(), 401);

        let request = test::TestRequest::post()
            .uri("/api/magician/v2/workspaces")
            .insert_header(("Authorization", auth.clone()))
            .set_json(serde_json::json!({"slug": "company", "display_name": "Company"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);

        // Duplicate slug conflicts; path tricks are refused.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/workspaces")
            .insert_header(("Authorization", auth.clone()))
            .set_json(serde_json::json!({"slug": "company", "display_name": "Again"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 409);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/workspaces")
            .insert_header(("Authorization", auth.clone()))
            .set_json(serde_json::json!({"slug": "../etc", "display_name": "Evil"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);

        let request = test::TestRequest::get()
            .uri("/api/magician/v2/workspaces")
            .insert_header(("Authorization", auth.clone()))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let ids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"default") && ids.contains(&"company"));
        let company = body
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["id"] == "company")
            .unwrap();
        assert_eq!(company["agent_count"], 0);
        assert_eq!(company["active_task_count"], 0);

        // PATCH renames the display; DELETE refuses the default and the
        // slug is immutable by construction (PATCH targets by id).
        let request = test::TestRequest::patch()
            .uri("/api/magician/v2/workspaces/company")
            .insert_header(("Authorization", auth.clone()))
            .set_json(serde_json::json!({"display_name": "Company 2"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["display_name"], "Company 2");

        let request = test::TestRequest::delete()
            .uri("/api/magician/v2/workspaces/default")
            .insert_header(("Authorization", auth.clone()))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);

        let request = test::TestRequest::delete()
            .uri("/api/magician/v2/workspaces/company")
            .insert_header(("Authorization", auth))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 204);
    }

    #[actix_web::test]
    async fn social_flow_start_callback_signup_and_replay_protection() {
        use magician::config::{AuthProviderConfig, AuthProvidersConfig};
        let dir = tempfile::tempdir().unwrap();
        let config = AuthConfig {
            providers: AuthProvidersConfig {
                google: AuthProviderConfig {
                    client_id: "cid".into(),
                    client_secret: "sec".into(),
                },
                github: AuthProviderConfig::default(),
            },
            ..AuthConfig::default()
        };
        let auth = runtime_with_config(config, dir.path());
        let app = test::init_service(
            App::new().app_data(auth).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/probe",
                        web::get().to(|req: HttpRequest| async move {
                            let header = |name: &str| {
                                req.headers()
                                    .get(name)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or("<none>")
                                    .to_string()
                            };
                            HttpResponse::Ok().json(serde_json::json!({
                                "principal": header("x-principal"),
                                "workspace": header("x-workspace"),
                            }))
                        }),
                    ),
            ),
        )
        .await;

        // An unconfigured provider answers 503, not a broken redirect.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/social/github/start")
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 503);

        // Start mints a single-use state and redirects to the provider.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/social/google/start")
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 302);
        let location = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .expect("redirect")
            .to_string();
        assert!(location.starts_with("https://accounts.google.com/"));
        assert!(location.contains("code_challenge_method=S256"));
        let state = location
            .split("state=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("state in redirect")
            .to_string();

        // Callback exchanges, resolves the subject, signs up the first
        // identity (adopting anonymous), and returns a session token.
        let request = test::TestRequest::get()
            .uri(&format!(
                "/api/magician/v2/auth/callback/google?code=good&state={state}"
            ))
            .insert_header(("Accept", "application/json"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let token = body["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("mag_"));
        assert!(body["identity"]["name"]
            .as_str()
            .unwrap()
            .starts_with("google-"));

        // The session works as a bearer.
        let (status, body) = probe!(app, Some(&token), "eve", "");
        assert_eq!(status, 200);
        assert_eq!(
            body["principal"], "anonymous",
            "first social identity adopts anonymous"
        );

        // Replay of the same state is dead.
        let request = test::TestRequest::get()
            .uri(&format!(
                "/api/magician/v2/auth/callback/google?code=good&state={state}"
            ))
            .insert_header(("Accept", "application/json"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400, "state is single-use");
    }

    #[actix_web::test]
    async fn social_signup_is_refused_when_disabled() {
        use magician::config::{AuthProviderConfig, AuthProvidersConfig};
        let dir = tempfile::tempdir().unwrap();
        let config = AuthConfig {
            allow_signup: false,
            providers: AuthProvidersConfig {
                google: AuthProviderConfig {
                    client_id: "cid".into(),
                    client_secret: "sec".into(),
                },
                github: AuthProviderConfig::default(),
            },
            ..AuthConfig::default()
        };
        let auth = runtime_with_config(config, dir.path());
        let app = test::init_service(
            App::new().app_data(auth).service(
                web::scope("/api/magician/v2")
                    .wrap(from_fn(authenticate_request))
                    .configure(configure_auth_routes)
                    .route(
                        "/probe",
                        web::get().to(|req: HttpRequest| async move {
                            let header = |name: &str| {
                                req.headers()
                                    .get(name)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or("<none>")
                                    .to_string()
                            };
                            HttpResponse::Ok().json(serde_json::json!({
                                "principal": header("x-principal"),
                                "workspace": header("x-workspace"),
                            }))
                        }),
                    ),
            ),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/social/google/start")
            .to_request();
        let response = test::call_service(&app, request).await;
        let location = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .expect("redirect")
            .to_string();
        let state = location
            .split("state=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .expect("state")
            .to_string();
        let request = test::TestRequest::get()
            .uri(&format!(
                "/api/magician/v2/auth/callback/google?code=good&state={state}"
            ))
            .insert_header(("Accept", "application/json"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            403,
            "signup disabled refuses unknown subjects"
        );
    }

    #[actix_web::test]
    async fn api_token_lifecycle_mint_use_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let session = login_token!(app);

        // Mint from the session.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/tokens")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .set_json(serde_json::json!({"label": "CI runner"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let pat = body["token"].as_str().unwrap().to_string();
        let id = body["id"].as_str().unwrap().to_string();
        assert!(pat.starts_with("mag_pat_"));

        // A token cannot mint tokens.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/tokens")
            .insert_header(("Authorization", format!("Bearer {pat}")))
            .set_json(serde_json::json!({"label": "escalate"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 403, "tokens do not mint tokens");

        // The token works as a bearer and engraves identity.
        let (status, body) = probe!(app, Some(&pat), "eve", "");
        assert_eq!(status, 200);
        assert_eq!(body["principal"], "anonymous");

        // Listing never shows the value.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/tokens")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        let listed = test::read_body(response).await;
        assert!(!String::from_utf8_lossy(&listed).contains(&pat));

        // Revocation kills it.
        let request = test::TestRequest::delete()
            .uri(&format!("/api/magician/v2/auth/tokens/{id}"))
            .insert_header(("Authorization", format!("Bearer {session}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 204);
        let (status, _) = probe!(app, Some(&pat), "", "");
        assert_eq!(
            status, 401,
            "an explicitly presented revoked bearer never falls back to open mode"
        );
    }

    #[actix_web::test]
    async fn terminal_grant_stays_bound_to_the_plane_door_and_can_be_revoked() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let session = login_token!(app);

        // An engraved, non-default workspace the identity owns.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/workspaces")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .set_json(serde_json::json!({"slug": "company", "display_name": "Company"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);

        // Mint: the floor filters the dangerous names out of the allowlist
        // and says so in the response.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .set_json(serde_json::json!({
                "label": "laptop terminal",
                "workspace": "company",
                "agent_identity": "plane-agent",
                "harness_engine": "claude_code",
                "allowed_tools": ["read_file", "run_coding_task", "claude"],
                "ttl_hours": 24
            }))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let grant_token = body["token"].as_str().unwrap().to_string();
        let grant_id = body["id"].as_str().unwrap().to_string();
        assert!(grant_token.starts_with("plt_"));
        assert_eq!(body["workspace"], "company");
        assert_eq!(
            body["allowed_tools"],
            serde_json::json!(["read_file"]),
            "floored names never reach the record"
        );
        assert_eq!(
            body["floor_filtered_tools"],
            serde_json::json!(["run_coding_task", "claude"]),
            "the drop is reported, not silent"
        );

        // A terminal grant is a route-local plane capability, not a general
        // API bearer. Caller scope assertions cannot widen it into REST.
        let (status, _) = probe!(app, Some(&grant_token), "eve", "default");
        assert_eq!(status, 401);

        // Listing shows everything except the value (and never a hash).
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", format!("Bearer {session}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let listed = String::from_utf8_lossy(&test::read_body(response).await).to_string();
        assert!(listed.contains(&grant_id));
        assert!(!listed.contains(&grant_token));
        assert!(!listed.contains("token_hash"));

        // Revocation removes the durable capability record. An explicitly
        // presented dead credential is still rejected in open mode.
        let request = test::TestRequest::delete()
            .uri(&format!("/api/magician/v2/auth/grants/{grant_id}"))
            .insert_header(("Authorization", format!("Bearer {session}")))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 204);
        let (status, _) = probe!(app, Some(&grant_token), "", "");
        assert_eq!(
            status, 401,
            "open mode never accepts an explicitly presented dead grant"
        );
    }

    #[actix_web::test]
    async fn grant_mint_refusals_and_non_session_minters() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let session = login_token!(app);
        let auth = format!("Bearer {session}");
        let valid = serde_json::json!({
            "label": "laptop terminal",
            "workspace": "default",
            "agent_identity": "plane-agent",
            "harness_engine": "claude_code",
            "allowed_tools": [],
            "ttl_hours": 24
        });

        // Unowned workspace is refused at mint.
        let mut unowned = valid.clone();
        unowned["workspace"] = serde_json::json!("not-owned");
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", auth.clone()))
            .set_json(unowned)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["error"], "unowned_workspace");

        // ttl_hours out of range on both sides.
        for ttl in [0u64, 2161] {
            let mut bad = valid.clone();
            bad["ttl_hours"] = serde_json::json!(ttl);
            let request = test::TestRequest::post()
                .uri("/api/magician/v2/auth/grants")
                .insert_header(("Authorization", auth.clone()))
                .set_json(bad)
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status().as_u16(), 400, "ttl {ttl} is out of range");
            let body: serde_json::Value =
                serde_json::from_slice(&test::read_body(response).await).unwrap();
            assert_eq!(body["error"], "invalid_ttl");
        }

        // harness_engine is a deliberate act, never defaulted: empty is
        // refused rather than falling back to "magician".
        let mut no_engine = valid.clone();
        no_engine["harness_engine"] = serde_json::json!("");
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", auth.clone()))
            .set_json(no_engine)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["error"], "invalid_request");

        // An API token cannot mint grants (403, like tokens-for-tokens).
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/tokens")
            .insert_header(("Authorization", auth.clone()))
            .set_json(serde_json::json!({"label": "CI"}))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let pat = body["token"].as_str().unwrap().to_string();
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", format!("Bearer {pat}")))
            .set_json(valid.clone())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(
            response.status().as_u16(),
            403,
            "an API token does not mint grants"
        );

        // A grant cannot mint grants either. It is rejected at the general
        // API admission boundary before the route-level session check.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", auth.clone()))
            .set_json(valid)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 201);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        let grant = body["token"].as_str().unwrap().to_string();
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/grants")
            .insert_header(("Authorization", format!("Bearer {grant}")))
            .set_json(serde_json::json!({
                "label": "escalate",
                "workspace": "default",
                "agent_identity": "plane-agent",
                "harness_engine": "magician",
                "allowed_tools": [],
                "ttl_hours": 24
            }))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(
            response.status().as_u16(),
            401,
            "a plane grant is not admitted to the general grant-minting API"
        );

        // An unknown grant id is a 404, not a 500.
        let request = test::TestRequest::delete()
            .uri("/api/magician/v2/auth/grants/00000000-0000-0000-0000-000000000000")
            .insert_header(("Authorization", auth))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 404);
    }

    /// A bot daemon the runtime spawned learns its scope the same way every
    /// other bearer does: `GET /auth/session`. The bot SDK's
    /// `resolveBearerScope` calls exactly this before the adapter starts,
    /// so a 401 here is a bot that never comes up. The token carries no
    /// login identity, so the answer names the bot instead of a person and
    /// the session-only doors (mint, rotate, logout-as-session) stay shut.
    #[actix_web::test]
    async fn bot_token_resolves_its_session_scope_but_cannot_mint() {
        let dir = tempfile::tempdir().unwrap();
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);
        let token = magician::magician_v2::auth::bot_token_registry().mint(
            magician::magician_v2::auth::BotGrant {
                principal: "anonymous".into(),
                workspace: "default".into(),
                bot_name: "kapso".into(),
            },
        );
        let auth = format!("Bearer {token}");

        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("Authorization", auth.clone()))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["workspace"], "default");
        assert_eq!(body["principal"], "anonymous");
        assert_eq!(body["method"], "bot_token");
        assert_eq!(body["bot"], "kapso");
        assert!(
            body["identity"].is_null(),
            "a bot token carries no login identity"
        );
        assert_eq!(body["workspaces"], serde_json::json!([]));

        // Session-only doors: a bot cannot mint a PAT, a grant, or rotate.
        for (uri, payload) in [
            (
                "/api/magician/v2/auth/tokens",
                serde_json::json!({"label": "x"}),
            ),
            (
                "/api/magician/v2/auth/session/scope",
                serde_json::json!({"workspace": "default"}),
            ),
        ] {
            let request = test::TestRequest::post()
                .uri(uri)
                .insert_header(("Authorization", auth.clone()))
                .set_json(payload)
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(
                response.status().as_u16(),
                403,
                "{uri} must refuse a bot token"
            );
            let body: serde_json::Value =
                serde_json::from_slice(&test::read_body(response).await).unwrap();
            assert_eq!(body["error"], "session_required");
        }

        // Logout revokes sessions; a bot token is revoked by the runtime.
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/auth/logout")
            .insert_header(("Authorization", auth.clone()))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 400);

        // Revoked by the runtime (the bot stopped): the bearer fails closed.
        magician::magician_v2::auth::bot_token_registry().revoke(&token);
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/session")
            .insert_header(("Authorization", auth))
            .to_request();
        let response = match app.call(request).await {
            Ok(response) => response.into_parts().1,
            Err(error) => error.error_response(),
        };
        assert_eq!(response.status().as_u16(), 401);
    }

    #[actix_web::test]
    async fn reconciler_reports_orphans_and_recognizes_system_roots() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        for name in ["anonymous", "system", "live-eval", "ghost"] {
            std::fs::create_dir_all(scopes.join(name)).unwrap();
        }
        // One identity adopting anonymous — `ghost` stays orphaned, and the
        // system/eval roots are recognized rather than orphaned.
        let store = magician::magician_v2::auth::AuthStore::open(dir.path()).unwrap();
        store
            .create_identity(
                "owner",
                "Alex",
                magician::magician_v2::auth::CredentialKind::Password { hash: "x".into() },
            )
            .unwrap();
        drop(store);

        std::env::set_var("MAGICIAN_ADMIN_SECRET", "test-admin-secret");
        let app = auth_app!(dir.path().to_path_buf(), AuthMode::Open);

        // Without the secret: 401; with it: ghost is the only orphan.
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/admin/orphaned-scopes")
            .insert_header(("Authorization", "Bearer wrong"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 401);

        let request = test::TestRequest::get()
            .uri("/api/magician/v2/auth/admin/orphaned-scopes")
            .insert_header(("Authorization", "Bearer test-admin-secret"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value =
            serde_json::from_slice(&test::read_body(response).await).unwrap();
        assert_eq!(body["orphaned_scope_roots"], serde_json::json!(["ghost"]));
        assert_eq!(
            body["recognized_system_roots"],
            serde_json::json!(["live-eval", "system"])
        );
        assert_eq!(body["owned_scope_roots"], serde_json::json!(["anonymous"]));

        std::env::remove_var("MAGICIAN_ADMIN_SECRET");
    }
}
