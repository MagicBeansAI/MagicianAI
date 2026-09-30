//! Enrollment API — HTTP endpoints for consumer channel identity enrollment.
//!
//! Routes:
//! ```text
//! POST   /api/magician/v2/chat/enroll                -> enroll or check identity
//! POST   /api/magician/v2/chat/enroll/approve         -> approve a pending enrollment (admin secret)
//! POST   /api/magician/v2/chat/enroll/revoke          -> un-enroll a channel (holder or owner)
//! POST   /api/magician/v2/chat/enroll/cancel          -> cancel a pending (code capability, or owner by channel)
//! GET    /api/magician/v2/chat/enroll/status           -> check enrollment status
//! ```
//! The workspace is never a request field on these routes: it comes from
//! the caller's engraved session scope (the approve door — a public
//! admin-secret route — is the one exception, taking it in the body
//! because public paths discard scope headers).

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use crate::scope::resolve_required_workspace;
use magician::config::EnrollmentConfig;
use magician::magician_v2::auth::middleware::authenticated;
use magician::magician_v2::chat::enrollment::{
    EnrollResult, EnrollStatus, EnrollmentStoreResolver,
};

/// Serializes cross-tree enrollment mutations (enroll's duplicate pre-scan
/// → write, approve's take → insert move): both are check-then-write over
/// SEPARATE per-principal stores, so without serialization two concurrent
/// calls could each pass their check and both write — recreating the
/// duplicate the pre-scan exists to prevent. Enrollments are human-rare;
/// one process-wide lock costs nothing.
static ENROLLMENT_MUTATION_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

// ========================================================================
// Request/Response Types
// ========================================================================

/// Request body for `POST /chat/enroll`.
///
/// The workspace is deliberately NOT a field: it always comes from the
/// caller's session (the middleware engraves it), like every scoped
/// route. It once existed but was dead — `resolve_required_workspace`
/// never read body values — and a dead field invites clients to believe
/// they can select a workspace they cannot.
#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    pub channel_type: String,
    pub channel_address: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Request body for `POST /chat/enroll/revoke` — same channel identity
/// triple as enroll, workspace session-derived for the same reason.
#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub channel_type: String,
    pub channel_address: String,
}

/// Response for `POST /chat/enroll/revoke`.
#[derive(Debug, Serialize)]
pub struct RevokeResponse {
    pub revoked: bool,
    pub principal: String,
}

/// Request body for `POST /chat/enroll/cancel` — cancel a PENDING
/// enrollment before its TTL sweeps it. Either arm: `code` (the one-time
/// capability from creation — presenting it is the authority, whoever
/// holds it created the pending), or the channel pair, which only the
/// owner may use (the administrative arm for pendings whose code the
/// owner never saw).
#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub channel_type: Option<String>,
    #[serde(default)]
    pub channel_address: Option<String>,
}

/// Response for `POST /chat/enroll/cancel`.
#[derive(Debug, Serialize)]
pub struct CancelResponse {
    pub cancelled: bool,
    pub channel_type: String,
    pub channel_address: String,
}

/// Response for `POST /chat/enroll`.
#[derive(Debug, Serialize)]
pub struct EnrollResponse {
    pub enrolled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Request body for `POST /chat/enroll/approve`.
#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    pub code: String,
    pub principal: String,
    /// The workspace whose pending records to approve. The route is a
    /// public admin door (it authenticates with MAGICIAN_ADMIN_SECRET, not
    /// a bearer), so the middleware discards scope headers before the
    /// handler runs — the workspace must arrive in the body. Absent means
    /// the default workspace.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Response for `POST /chat/enroll/approve`.
#[derive(Debug, Serialize)]
pub struct ApproveResponse {
    pub enrolled: bool,
    pub channel_type: String,
    pub channel_address: String,
    pub principal: String,
}

/// Query parameters for `GET /chat/enroll/status`.
#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub channel_type: String,
    pub channel_address: String,
}

/// Response for `GET /chat/enroll/status`.
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub enrolled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

// ========================================================================
// Shared API Data
// ========================================================================

/// Shared state for enrollment API handlers.
pub struct EnrollmentApi {
    pub store_resolver: EnrollmentStoreResolver,
    pub config: EnrollmentConfig,
}

impl EnrollmentApi {
    pub fn new(store_resolver: EnrollmentStoreResolver, config: EnrollmentConfig) -> Self {
        Self {
            store_resolver,
            config,
        }
    }
}

// ========================================================================
// Handlers
// ========================================================================

/// POST /api/magician/v2/chat/enroll
///
/// Enroll a consumer channel identity. Idempotent — calling again for an
/// already-enrolled identity returns the existing principal.
pub async fn enroll_handler(
    req: HttpRequest,
    enrollment_api: web::Data<EnrollmentApi>,
    body: web::Json<EnrollRequest>,
) -> impl Responder {
    let workspace = match resolve_required_workspace(req.headers(), None) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    debug!(
        "[ENROLLMENT-API] POST /chat/enroll channel_type={} channel_address={} workspace={}",
        body.channel_type, body.channel_address, workspace
    );

    if body.channel_type.trim().is_empty() || body.channel_address.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "channel_type and channel_address are required"
        }));
    }

    let store = match enrollment_api
        .store_resolver
        .resolve_for_scope(&enrollment_api.config.default_principal, &workspace)
        .await
    {
        Ok(store) => store,
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to resolve enrollment store: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve enrollment store",
                "details": e.to_string()
            }));
        },
    };

    const MAX_CHANNEL_TYPE_LEN: usize = 64;
    const MAX_CHANNEL_ADDRESS_LEN: usize = 256;
    if body.channel_type.len() > MAX_CHANNEL_TYPE_LEN
        || body.channel_address.len() > MAX_CHANNEL_ADDRESS_LEN
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!(
                "channel_type max {} chars, channel_address max {} chars",
                MAX_CHANNEL_TYPE_LEN, MAX_CHANNEL_ADDRESS_LEN
            )
        }));
    }
    // Same bound as member provisioning: display names are stored verbatim
    // in enrollment records; an unbounded one is an unbounded file.
    const MAX_DISPLAY_NAME_LEN: usize = 64;
    if body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| value.chars().count() > MAX_DISPLAY_NAME_LEN)
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!("display_name must be at most {} characters.", MAX_DISPLAY_NAME_LEN)
        }));
    }

    // Clean up expired pending enrollments on every enrollment attempt
    store
        .cleanup_expired(enrollment_api.config.pending_ttl_hours)
        .await;

    // Cross-tree duplicate guard (2026-08-30, review round 3): member
    // records live in per-principal trees, so the store-level
    // AlreadyEnrolled check cannot see a channel another principal already
    // holds — and the inbound resolver scans principals alphabetically, so
    // a member's duplicate would silently WIN the owner's routing. Resolve
    // globally first: an existing mapping is returned as-is (idempotent,
    // no takeover), whoever holds it. Held under the mutation lock so the
    // scan-then-write cannot race a concurrent enroll or approve move.
    let _enrollment_guard = ENROLLMENT_MUTATION_LOCK.lock().await;
    match enrollment_api
        .store_resolver
        .resolve_enrolled_principal(&workspace, &body.channel_type, &body.channel_address)
        .await
    {
        Ok(Some(existing)) => {
            return HttpResponse::Ok().json(EnrollResponse {
                enrolled: true,
                principal: Some(existing),
                code: None,
            });
        },
        Ok(None) => {},
        Err(e) => {
            // Refuse rather than fail open: the only thing this scan guards
            // is a cross-principal takeover, and "temporarily cannot tell"
            // must not become "go ahead".
            error!("[ENROLLMENT-API] Duplicate pre-scan failed: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve existing enrollments",
                "details": e.to_string()
            }));
        },
    }

    // Member-aware enrollment (2026-08-30): an auto-approved record lands
    // in the CALLER's tree — matching the approve flow, which also moves
    // member records into the member's tree — and maps the channel to the
    // caller's principal. Before this, every auto-approval pointed at
    // default_principal, so a family install bled member channels into
    // the owner's scope. Pending records keep their designed home in the
    // default tree: that is the only tree the admin approve flow scans
    // for codes, so a member's pending enrollment must not hide elsewhere.
    // Unauthenticated callers (open-mode bootstrap, before the first
    // identity exists) keep the config default for both.
    let assigned_principal = authenticated(&req)
        .map(|stamped| stamped.scope.principal().to_string())
        .unwrap_or_else(|| enrollment_api.config.default_principal.clone());
    let target_principal = if enrollment_api.config.auto_approve {
        assigned_principal.clone()
    } else {
        enrollment_api.config.default_principal.clone()
    };
    let enrollment_store = match enrollment_api
        .store_resolver
        .resolve_for_scope(&target_principal, &workspace)
        .await
    {
        Ok(enrollment_store) => enrollment_store,
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to resolve enrollment store: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve enrollment store",
                "details": e.to_string()
            }));
        },
    };

    match enrollment_store
        .enroll(
            &body.channel_type,
            &body.channel_address,
            &workspace,
            body.display_name.clone(),
            enrollment_api.config.auto_approve,
            &assigned_principal,
        )
        .await
    {
        Ok(EnrollResult::AlreadyEnrolled { principal }) => {
            HttpResponse::Ok().json(EnrollResponse {
                enrolled: true,
                principal: Some(principal),
                code: None,
            })
        },
        Ok(EnrollResult::AutoApproved { principal }) => HttpResponse::Ok().json(EnrollResponse {
            enrolled: true,
            principal: Some(principal),
            code: None,
        }),
        Ok(EnrollResult::Pending { code }) => HttpResponse::Ok().json(EnrollResponse {
            enrolled: false,
            principal: None,
            code: Some(code),
        }),
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to enroll: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to enroll",
                "details": e.to_string()
            }))
        },
    }
}

/// POST /api/magician/v2/chat/enroll/approve
///
/// Approve a pending enrollment by code. Assigns the given principal to the
/// pending identity.
pub async fn approve_handler(
    req: HttpRequest,
    enrollment_api: web::Data<EnrollmentApi>,
    body: web::Json<ApproveRequest>,
) -> impl Responder {
    debug!(
        "[ENROLLMENT-API] POST /chat/enroll/approve code={} principal={}",
        body.code, body.principal
    );

    // --- Admin secret auth (constant-time) + the same throttle as the
    // password door: a guessable admin secret is a password with more
    // privileges, and it gets the same sliding-window 429 treatment. ---
    let peer = crate::auth_api::peer_of(&req);
    if let Some(retry_after_secs) = crate::auth_api::admin_secret_throttle()
        .check(crate::auth_api::ADMIN_SECRET_THROTTLE_KEY, &peer)
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
    match admin_secret {
        None => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Admin secret not configured. Set MAGICIAN_ADMIN_SECRET env var."
            }));
        },
        Some(secret) => {
            let auth = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let expected = format!("Bearer {}", secret);
            // Constant-time comparison to prevent timing side-channel
            let auth_bytes = auth.as_bytes();
            let expected_bytes = expected.as_bytes();
            let matches = auth_bytes.len() == expected_bytes.len()
                && auth_bytes
                    .iter()
                    .zip(expected_bytes.iter())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0;
            if !matches {
                crate::auth_api::admin_secret_throttle()
                    .record_failure(crate::auth_api::ADMIN_SECRET_THROTTLE_KEY, &peer);
                return HttpResponse::Unauthorized().json(serde_json::json!({
                    "error": "Invalid admin secret"
                }));
            }
        },
    }
    crate::auth_api::admin_secret_throttle()
        .record_success(crate::auth_api::ADMIN_SECRET_THROTTLE_KEY, &peer);

    if body.code.trim().is_empty() || body.principal.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "code and principal are required"
        }));
    }
    // Slugs are directory names; the path layer already neutralizes
    // traversal (`safe_segment`), but a typo like "al ise" would otherwise
    // mint a phantom scope directory the resolver later treats as a known
    // principal. Validate the shape at the door instead.
    if let Err(error) =
        magician::magician_v2::auth::identity::validate_principal_name(body.principal.trim())
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_principal",
            "message": format!("Approve target must be a valid principal slug: {error}")
        }));
    }

    // The route is public (admin-secret door), so the middleware has
    // discarded caller scope headers; the workspace comes from the body
    // and defaults to the default workspace.
    let workspace = body
        .workspace
        .clone()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            magician::magician_v2::auth::workspace_registry::DEFAULT_WORKSPACE_ID.to_string()
        });
    if let Err(error) = magician::magician_v2::auth::identity::validate_principal_name(&workspace) {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_workspace",
            "message": format!("Workspace must be a valid slug: {error}")
        }));
    }
    let store = match enrollment_api
        .store_resolver
        .resolve_for_scope(&enrollment_api.config.default_principal, &workspace)
        .await
    {
        Ok(store) => store,
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to resolve enrollment store: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve enrollment store",
                "details": e.to_string()
            }));
        },
    };
    store
        .cleanup_expired(enrollment_api.config.pending_ttl_hours)
        .await;

    // Same mutation lock as enroll: the approve is a take-then-insert move
    // across two trees and must not race a concurrent enroll's pre-scan.
    let _enrollment_guard = ENROLLMENT_MUTATION_LOCK.lock().await;
    match enrollment_api
        .store_resolver
        .approve(
            &enrollment_api.config.default_principal,
            &workspace,
            &body.code,
            &body.principal,
        )
        .await
    {
        Ok(result) => HttpResponse::Ok().json(ApproveResponse {
            enrolled: true,
            channel_type: result.channel_type,
            channel_address: result.channel_address,
            principal: result.record.principal,
        }),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Pending enrollment not found or expired",
                    "code": body.code
                }))
            } else {
                error!("[ENROLLMENT-API] Failed to approve enrollment: {}", e);
                HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to approve enrollment",
                    "details": msg
                }))
            }
        },
    }
}

/// GET /api/magician/v2/chat/enroll/status
///
/// Check the enrollment status for a (channel_type, channel_address) pair.
pub async fn enrollment_status_handler(
    req: HttpRequest,
    enrollment_api: web::Data<EnrollmentApi>,
    query: web::Query<StatusQuery>,
) -> impl Responder {
    let workspace = match resolve_required_workspace(req.headers(), None) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    debug!(
        "[ENROLLMENT-API] GET /chat/enroll/status channel_type={} channel_address={} workspace={}",
        query.channel_type, query.channel_address, workspace
    );

    if query.channel_type.trim().is_empty() || query.channel_address.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "channel_type and channel_address query parameters are required"
        }));
    }

    const MAX_CHANNEL_TYPE_LEN: usize = 64;
    const MAX_CHANNEL_ADDRESS_LEN: usize = 256;
    if query.channel_type.len() > MAX_CHANNEL_TYPE_LEN
        || query.channel_address.len() > MAX_CHANNEL_ADDRESS_LEN
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!(
                "channel_type max {} chars, channel_address max {} chars",
                MAX_CHANNEL_TYPE_LEN, MAX_CHANNEL_ADDRESS_LEN
            )
        }));
    }

    match enrollment_api
        .store_resolver
        .status(
            &enrollment_api.config.default_principal,
            &workspace,
            &query.channel_type,
            &query.channel_address,
        )
        .await
    {
        Ok(EnrollStatus::Enrolled { principal }) => HttpResponse::Ok().json(StatusResponse {
            enrolled: true,
            principal: Some(principal),
            code: None,
        }),
        // A pending code is shown exactly once, in the enroll response to
        // its requester — the plane-grant token discipline. Status answers
        // any authenticated caller, so repeating the code here leaked every
        // member's pending code to every other member (inert alone —
        // approval still needs the admin secret — but still a secret with
        // no reason to travel).
        Ok(EnrollStatus::Pending { .. }) => HttpResponse::Ok().json(StatusResponse {
            enrolled: false,
            principal: None,
            code: None,
        }),
        Ok(EnrollStatus::Unknown) => HttpResponse::Ok().json(StatusResponse {
            enrolled: false,
            principal: None,
            code: None,
        }),
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to resolve enrollment store: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve enrollment store",
                "details": e.to_string()
            }))
        },
    }
}

/// The owner is the install's first login identity. A caller with no login
/// identity (a runtime-minted bot token) is never the owner: the check is
/// spelled out rather than comparing two `Option`s, where an install with no
/// identities yet would let a bot pass as `None == None`.
fn is_owner(owner_name: Option<&str>, identity_name: Option<&str>) -> bool {
    matches!((owner_name, identity_name), (Some(owner), Some(caller)) if owner == caller)
}

/// POST /api/magician/v2/chat/enroll/revoke
///
/// Un-enroll a channel: the holder's mapping is removed and the channel
/// becomes claimable again. A member may revoke their own channel; the
/// owner (the interim admin — the first identity) may revoke anyone's,
/// which is the reassignment flow: revoke, then the new holder enrolls,
/// with the duplicate pre-scan keeping the gap race-free. There is
/// deliberately no in-place reassignment.
pub async fn revoke_handler(
    req: HttpRequest,
    enrollment_api: web::Data<EnrollmentApi>,
    auth: web::Data<magician::magician_v2::auth::middleware::AuthRuntime>,
    body: web::Json<RevokeRequest>,
) -> impl Responder {
    let Some(stamped) = authenticated(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }));
    };
    if body.channel_type.trim().is_empty() || body.channel_address.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "channel_type and channel_address are required"
        }));
    }
    // Same bounds as enroll/status: reject garbage before it becomes scan
    // keys or error messages.
    const MAX_CHANNEL_TYPE_LEN: usize = 64;
    const MAX_CHANNEL_ADDRESS_LEN: usize = 256;
    if body.channel_type.len() > MAX_CHANNEL_TYPE_LEN
        || body.channel_address.len() > MAX_CHANNEL_ADDRESS_LEN
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!(
                "channel_type max {} chars, channel_address max {} chars",
                MAX_CHANNEL_TYPE_LEN, MAX_CHANNEL_ADDRESS_LEN
            )
        }));
    }
    let workspace = match resolve_required_workspace(req.headers(), None) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };

    // Ownership is checked BEFORE removal, and both steps sit under the
    // mutation lock so the holder cannot change between them.
    let _enrollment_guard = ENROLLMENT_MUTATION_LOCK.lock().await;
    let holder = match enrollment_api
        .store_resolver
        .resolve_enrolled_principal(&workspace, &body.channel_type, &body.channel_address)
        .await
    {
        Ok(Some(holder)) => holder,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown_channel",
                "message": "No enrolled channel matches that identity in this workspace."
            }));
        },
        Err(e) => {
            error!("[ENROLLMENT-API] Revoke pre-scan failed: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve existing enrollments",
                "details": e.to_string()
            }));
        },
    };
    if holder != stamped.scope.principal() {
        let owner_name = auth
            .store
            .list_identities()
            .ok()
            .and_then(|identities| identities.first().map(|identity| identity.name.clone()));
        if !is_owner(owner_name.as_deref(), stamped.identity_name.as_deref()) {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "not_your_channel",
                "message": "Only the channel's holder or the owner may revoke it."
            }));
        }
    }
    match enrollment_api
        .store_resolver
        .revoke_enrollment(&workspace, &body.channel_type, &body.channel_address)
        .await
    {
        Ok(Some(principal)) => HttpResponse::Ok().json(RevokeResponse {
            revoked: true,
            principal,
        }),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown_channel",
            "message": "The mapping disappeared before the revoke landed."
        })),
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to revoke enrollment: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to revoke enrollment",
                "details": e.to_string()
            }))
        },
    }
}

/// POST /api/magician/v2/chat/enroll/cancel
///
/// Cancel a PENDING enrollment before its TTL sweeps it. Two arms:
/// present the **code** — the one-time capability from creation, whose
/// holder is by construction the pending's creator (or someone they
/// shared it with) — or, as the **owner**, the channel identity, for
/// pendings whose code the owner never saw. Pendings live only in the
/// default tree, so no cross-tree scan is involved.
pub async fn cancel_handler(
    req: HttpRequest,
    enrollment_api: web::Data<EnrollmentApi>,
    auth: web::Data<magician::magician_v2::auth::middleware::AuthRuntime>,
    body: web::Json<CancelRequest>,
) -> impl Responder {
    let Some(stamped) = authenticated(&req) else {
        return HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required."
        }));
    };
    let workspace = match resolve_required_workspace(req.headers(), None) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let code = body
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let channel_type = body
        .channel_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let channel_address = body
        .channel_address
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    // Same bounds as the sibling endpoints.
    const MAX_CHANNEL_TYPE_LEN: usize = 64;
    const MAX_CHANNEL_ADDRESS_LEN: usize = 256;
    if channel_type
        .as_deref()
        .is_some_and(|value| value.len() > MAX_CHANNEL_TYPE_LEN)
        || channel_address
            .as_deref()
            .is_some_and(|value| value.len() > MAX_CHANNEL_ADDRESS_LEN)
    {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!(
                "channel_type max {} chars, channel_address max {} chars",
                MAX_CHANNEL_TYPE_LEN, MAX_CHANNEL_ADDRESS_LEN
            )
        }));
    }

    let default_store = match enrollment_api
        .store_resolver
        .resolve_for_scope(&enrollment_api.config.default_principal, &workspace)
        .await
    {
        Ok(store) => store,
        Err(e) => {
            error!("[ENROLLMENT-API] Failed to resolve enrollment store: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve enrollment store",
                "details": e.to_string()
            }));
        },
    };
    let _enrollment_guard = ENROLLMENT_MUTATION_LOCK.lock().await;

    if let Some(code) = code {
        // Capability arm: the code is its own authority.
        match default_store.cancel_pending(&code).await {
            Ok(Some(pending)) => HttpResponse::Ok().json(CancelResponse {
                cancelled: true,
                channel_type: pending.channel_type,
                channel_address: pending.channel_address,
            }),
            Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown_code",
                "message": "No pending enrollment matches that code."
            })),
            Err(e) => {
                error!("[ENROLLMENT-API] Failed to cancel pending: {}", e);
                HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to cancel pending enrollment",
                    "details": e.to_string()
                }))
            },
        }
    } else if let (Some(channel_type), Some(channel_address)) = (channel_type, channel_address) {
        // Administrative arm: the owner may cancel by channel identity.
        let owner_name = auth
            .store
            .list_identities()
            .ok()
            .and_then(|identities| identities.first().map(|identity| identity.name.clone()));
        if !is_owner(owner_name.as_deref(), stamped.identity_name.as_deref()) {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "owner_required",
                "message": "Cancelling by channel identity is the owner's arm; present the code instead."
            }));
        }
        match default_store
            .cancel_pending_by_channel(&channel_type, &channel_address, &workspace)
            .await
        {
            Ok(Some(_)) => HttpResponse::Ok().json(CancelResponse {
                cancelled: true,
                channel_type,
                channel_address,
            }),
            Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown_channel",
                "message": "No pending enrollment matches that channel in this workspace."
            })),
            Err(e) => {
                error!("[ENROLLMENT-API] Failed to cancel pending: {}", e);
                HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to cancel pending enrollment",
                    "details": e.to_string()
                }))
            },
        }
    } else {
        HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_request",
            "message": "Provide the pending code, or (as the owner) the channel_type and channel_address."
        }))
    }
}
