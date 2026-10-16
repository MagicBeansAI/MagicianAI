//! Verifying Cloudflare Access as an outer gateway without treating it as API scope.
//!
//! Principal/workspace come from the Magician bearer (or the paired-device
//! record). Cloudflare's signed `Cf-Access-Jwt-Assertion` proves the outer edge
//! traversal and actor evidence only; it never selects or rewrites API scope.
//!
//! What this buys, stated precisely, because it is easy to overclaim:
//!
//! - A verified token proves the request traversed Access and that the claims
//!   inside it were minted by Cloudflare for *this* application. Header
//!   spoofing by anything past the tunnel stops working.
//! - For an SSO user it yields their email, which is a real person.
//! - For a **service token** it yields `common_name` — the token's name, shared
//!   by everyone holding it. That is useful edge evidence but never a Magician
//!   principal. Per-device pairing remains the only native-client identity.
//!
//! Absent configuration it is inert for ordinary local development. The one
//! bearer-minting ESP pairing exception is still restricted to an actual
//! loopback peer when no verifier is configured.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

/// The header Cloudflare Access sets on every request it forwards.
pub const ACCESS_JWT_HEADER: &str = "Cf-Access-Jwt-Assertion";
pub const MOBILE_DEVICE_ID_HEADER: &str = "X-Magician-Device-Id";
pub const LEGACY_ANDROID_DEVICE_ID_HEADER: &str = "X-Magdroid-Device";
const ACCESS_CLIENT_ID_HEADER: &str = "CF-Access-Client-Id";
const ACCESS_CLIENT_SECRET_HEADER: &str = "CF-Access-Client-Secret";
const MOBILE_ENROLLMENT_EXCHANGE_PATH: &str = "/api/magician/v2/devices/enrollment/exchange";
const ANDROID_APPS_ENROLLMENT_EXCHANGE_PATH: &str =
    "/api/magician/v2/devices/apps-automation/enrollment/exchange";
const ESP_DEVICE_PAIR_PATH: &str = "/api/magician/v2/devices/pair";

/// Authentication class proved by the outer request boundary.
///
/// This value is inserted into Actix request extensions only after the
/// corresponding credential or actual loopback peer has been verified. It is
/// intentionally not deserializable from an HTTP payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedRequestAuthentication {
    CloudflareAccess,
    PairedDevice,
    MagicianBearer,
    TrustedLoopbackSingleUser,
}

/// Marker installed only after the outer Access JWT on the exact ESP pairing
/// route has been verified. It carries no Magician scope: the handler fixes
/// that bootstrap to `anonymous/default` and mints the device-bound bearer
/// used by every subsequent request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedEspPairBootstrap;

/// Server-owned request identity consumed by boundaries that must not trust
/// the compatibility `X-Principal` / `X-Workspace` headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRequestIdentity {
    principal: String,
    workspace: Option<String>,
    actor_fingerprint: String,
    session_fingerprint: String,
    authentication: VerifiedRequestAuthentication,
    authentication_revision: u64,
    verified_at: chrono::DateTime<chrono::Utc>,
}

impl VerifiedRequestIdentity {
    pub(crate) fn from_magician_bearer(principal: &str, workspace: &str, token: &str) -> Self {
        Self {
            principal: principal.to_owned(),
            workspace: Some(workspace.to_owned()),
            actor_fingerprint: request_identity_fingerprint("bearer-actor", principal),
            session_fingerprint: request_identity_fingerprint("bearer-session", token),
            authentication: VerifiedRequestAuthentication::MagicianBearer,
            authentication_revision: 1,
            verified_at: chrono::Utc::now(),
        }
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Paired devices and the local single-user fallback are bound to one
    /// exact workspace. Interactive Access users select one of their own
    /// workspaces at the route boundary, so this is `None` for that class.
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    pub fn actor_fingerprint(&self) -> &str {
        &self.actor_fingerprint
    }

    pub fn session_fingerprint(&self) -> &str {
        &self.session_fingerprint
    }

    pub fn authentication(&self) -> VerifiedRequestAuthentication {
        self.authentication
    }

    pub fn authentication_revision(&self) -> u64 {
        self.authentication_revision
    }

    pub fn verified_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.verified_at.to_owned()
    }

    /// Test-only constructor for identities the real middleware mints —
    /// for example a Cloudflare Access interactive identity, which
    /// carries no workspace binding. Server-owned evidence with the same
    /// shape verification produces; never compiled into release builds.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn for_test(
        principal: &str,
        workspace: Option<&str>,
        authentication: VerifiedRequestAuthentication,
    ) -> Self {
        Self::for_test_at(principal, workspace, authentication, chrono::Utc::now())
    }

    /// Deterministic-clock variant for tests that exercise the request
    /// freshness boundary at a fixed instant.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn for_test_at(
        principal: &str,
        workspace: Option<&str>,
        authentication: VerifiedRequestAuthentication,
        verified_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            principal: principal.to_owned(),
            workspace: workspace.map(str::to_owned),
            actor_fingerprint: request_identity_fingerprint("test-actor", principal),
            session_fingerprint: request_identity_fingerprint("test-session", principal),
            authentication,
            authentication_revision: 1,
            verified_at,
        }
    }
}

fn request_identity_fingerprint(label: &str, value: &str) -> String {
    let mut material =
        Vec::with_capacity(label.len().saturating_add(value.len()).saturating_add(1));
    material.extend_from_slice(label.as_bytes());
    material.push(0);
    material.extend_from_slice(value.as_bytes());
    blake3::hash(&material).to_hex().to_string()
}

fn local_process_session() -> &'static str {
    static LOCAL_PROCESS_SESSION: OnceLock<String> = OnceLock::new();
    LOCAL_PROCESS_SESSION
        .get_or_init(|| format!("{}:{}", std::process::id(), uuid::Uuid::new_v4()))
        .as_str()
}

fn trusted_loopback_request_identity() -> VerifiedRequestIdentity {
    let process_session = local_process_session();
    VerifiedRequestIdentity {
        principal: DEFAULT_SCOPE_PRINCIPAL.to_owned(),
        workspace: Some(DEFAULT_SCOPE_WORKSPACE.to_owned()),
        actor_fingerprint: request_identity_fingerprint("loopback-actor", process_session),
        session_fingerprint: request_identity_fingerprint("loopback-session", process_session),
        authentication: VerifiedRequestAuthentication::TrustedLoopbackSingleUser,
        authentication_revision: 1,
        verified_at: chrono::Utc::now(),
    }
}

/// How long a fetched key set is reused before refetching.
///
/// Cloudflare rotates signing keys, and a cache that never expires turns a
/// rotation into a total outage. An hour is short enough to follow a rotation
/// without asking on every request.
const JWKS_TTL: Duration = Duration::from_secs(3600);

/// What to do with a request that carries no valid assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Verify when present, allow when absent. The migration setting.
    ///
    /// Weaker than it sounds — an attacker simply omits the header — so this is
    /// a stepping stone, not a destination. It exists because Magician's own
    /// components call its HTTP API, and flipping straight to [`Self::Require`]
    /// would 401 them before anyone noticed.
    Verify,
    /// Reject anything without a valid assertion. The destination.
    Require,
}

/// Identity proved by a verified assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    /// Email for an SSO user; `None` for a service token.
    pub email: Option<String>,
    /// Service token name, when this was a service token rather than a person.
    pub common_name: Option<String>,
    /// Cloudflare's subject id. Empty for service tokens.
    pub subject: String,
}

impl VerifiedIdentity {
    /// The human scope principal this identity maps to.
    ///
    /// A service-token `common_name` is deliberately excluded: it is shared
    /// outer-gateway evidence, not an owner or device identity. Native calls
    /// must already have returned through the paired-device branch.
    pub fn principal(&self) -> Option<String> {
        self.email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    common_name: Option<String>,
    #[serde(default)]
    sub: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("no Cloudflare Access assertion on the request")]
    Missing,
    #[error("the assertion is not a readable JWT: {0}")]
    Malformed(String),
    #[error("the assertion names key `{0}`, which Cloudflare did not publish")]
    UnknownKey(String),
    #[error("the assertion did not verify: {0}")]
    Rejected(String),
    #[error("could not reach Cloudflare for signing keys: {0}")]
    KeysUnavailable(String),
}

/// Verifies Access assertions against Cloudflare's published signing keys.
pub struct AccessVerifier {
    issuer: String,
    audience: String,
    mode: AccessMode,
    http: reqwest::Client,
    cache: RwLock<Option<(Vec<Jwk>, Instant)>>,
}

impl AccessVerifier {
    /// Build from the environment, or `None` when Access is not configured.
    ///
    /// Both the team domain and the application audience are required. Half a
    /// configuration would verify signatures without checking *which*
    /// application the token was minted for, and a valid token for a different
    /// Access app would sail through — worse than not verifying, because it
    /// looks like it is working.
    pub fn from_env() -> Option<Arc<Self>> {
        let team = std::env::var("MAGICIAN_CF_ACCESS_TEAM_DOMAIN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        let audience = std::env::var("MAGICIAN_CF_ACCESS_AUD")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;

        let mode = match std::env::var("MAGICIAN_CF_ACCESS_MODE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "require" => AccessMode::Require,
            _ => AccessMode::Verify,
        };

        // Accepts either the bare team name or a full hostname, because both
        // are what people have written down.
        let issuer = access_issuer(&team);

        Some(Arc::new(Self {
            issuer,
            audience,
            mode,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            cache: RwLock::new(None),
        }))
    }

    pub fn mode(&self) -> AccessMode {
        self.mode
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    async fn keys(&self) -> Result<Vec<Jwk>, AccessError> {
        if let Some((keys, fetched)) = self.cache.read().await.as_ref() {
            if fetched.elapsed() < JWKS_TTL {
                return Ok(keys.clone());
            }
        }

        let url = format!("{}/cdn-cgi/access/certs", self.issuer);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|error| AccessError::KeysUnavailable(error.to_string()))?;
        let jwks = response
            .json::<Jwks>()
            .await
            .map_err(|error| AccessError::KeysUnavailable(error.to_string()))?;

        debug!(
            "[CF-ACCESS] fetched {} signing key(s) from {}",
            jwks.keys.len(),
            url
        );
        *self.cache.write().await = Some((jwks.keys.clone(), Instant::now()));
        Ok(jwks.keys)
    }

    /// Verify one assertion and return the identity it proves.
    pub async fn verify(&self, token: &str) -> Result<VerifiedIdentity, AccessError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(AccessError::Missing);
        }

        let header = jsonwebtoken::decode_header(token)
            .map_err(|error| AccessError::Malformed(error.to_string()))?;
        let kid = header
            .kid
            .ok_or_else(|| AccessError::Malformed("no `kid` in the JWT header".to_string()))?;

        let mut keys = self.keys().await?;
        if !keys.iter().any(|key| key.kid == kid) {
            // An unknown kid is what a rotation looks like from here, so the
            // cache is dropped and refetched once before giving up. Without
            // this, every rotation is an outage until the TTL happens to lapse.
            *self.cache.write().await = None;
            keys = self.keys().await?;
        }
        let jwk = keys
            .iter()
            .find(|key| key.kid == kid)
            .ok_or_else(|| AccessError::UnknownKey(kid.clone()))?;

        let key = jsonwebtoken::DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .map_err(|error| AccessError::Rejected(error.to_string()))?;

        // RS256 pinned rather than taken from the token's own header: letting a
        // token choose its algorithm is how `alg: none` and HMAC-confusion
        // attacks work.
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.validate_exp = true;
        validation.validate_nbf = true;

        let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|error| AccessError::Rejected(error.to_string()))?;

        Ok(VerifiedIdentity {
            email: data.claims.email,
            common_name: data.claims.common_name,
            subject: data.claims.sub,
        })
    }
}

fn access_issuer(team: &str) -> String {
    let team = team.trim().trim_end_matches('/');
    if team.starts_with("https://") {
        team.to_string()
    } else if team.contains('.') {
        format!("https://{team}")
    } else {
        format!("https://{team}.cloudflareaccess.com")
    }
}

/// Whether a TCP peer address is loopback.
///
/// Magician's own components call its HTTP API — transcript sinks, voice
/// responders — and those requests never pass through Cloudflare, so they carry
/// no assertion and would be refused the moment enforcement is switched on.
/// The middleware combines this predicate with a forwarded-header check before
/// granting the exemption; loopback alone is insufficient behind a local proxy.
pub fn is_loopback_peer(peer: Option<std::net::IpAddr>) -> bool {
    matches!(peer, Some(address) if address.is_loopback())
}

/// A direct same-machine request, as opposed to a remote request delivered by
/// a loopback reverse proxy such as cloudflared. The TCP peer of both is
/// loopback, but the proxy adds original-client forwarding metadata. Treating
/// that second shape as local would turn every tunnel request into an Access
/// bypass at the origin.
fn is_direct_loopback_request(req: &actix_web::dev::ServiceRequest) -> bool {
    is_loopback_peer(req.peer_addr().map(|address| address.ip()))
        && [
            "cf-connecting-ip",
            "forwarded",
            "x-forwarded-for",
            "x-real-ip",
        ]
        .iter()
        .all(|header| !req.headers().contains_key(*header))
}

/// Verify the Access assertion after bearer scope has been authenticated.
///
/// A *present but invalid* assertion is refused in both modes. Absence is a
/// question of policy — [`AccessMode`] decides — but a token that fails to
/// verify is either an attack or a misconfiguration, and neither should be
/// waved through on the grounds that it could have been left out entirely.
pub async fn verify_access_middleware(
    mut req: actix_web::dev::ServiceRequest,
    next: actix_web::middleware::Next<impl actix_web::body::MessageBody + 'static>,
) -> Result<actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>, actix_web::Error> {
    use actix_web::http::header::{HeaderName, HeaderValue};
    use actix_web::HttpMessage;

    let header = |primary: &'static str, legacy: &'static str| {
        req.headers()
            .get(primary)
            .or_else(|| req.headers().get(legacy))
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let is_esp_pair =
        req.method() == actix_web::http::Method::POST && req.path() == ESP_DEVICE_PAIR_PATH;
    // This proof was validated by the outer bearer gate for exactly one asset
    // GET. Keep Access verification intact, but do not manufacture a loopback
    // or Access API identity for a request that holds only page-file authority.
    let scripted_asset = req
        .extensions()
        .get::<crate::magician_v2::auth::middleware::AuthenticatedScriptedSurfaceAsset>()
        .is_some();

    // A paired native client is its own identity boundary. Resolve the owner
    // scope from the credential instead of preserving phone-controlled scope
    // headers, and do this before Cloudflare verification so a deployment may
    // use a narrowly bypassed mobile API path without weakening authorization.
    let mobile_device_id = header(MOBILE_DEVICE_ID_HEADER, LEGACY_ANDROID_DEVICE_ID_HEADER);
    // The durable device credential is a standard bearer. Scope is always
    // resolved from the pairing record; dedicated token headers are rejected
    // rather than kept as a second credential transport.
    let mobile_device_token = req
        .headers()
        .get(actix_web::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    // A bearer is a device credential only when the device-id header names
    // the pairing it belongs to. Without that header the bearer is the
    // caller's Magician session or API token, already resolved (or refused)
    // by the auth gate that runs ahead of this layer, and this layer has no
    // say over it. Treating every bare bearer as an incomplete device
    // credential rejected each browser session the moment it was minted.
    if mobile_device_id.is_some() && mobile_device_token.is_none() {
        return Err(actix_web::error::ErrorUnauthorized(
            "mobile device credential is incomplete",
        ));
    }
    if let (Some(device_id), Some(device_token)) = (mobile_device_id, mobile_device_token) {
        let pairing = req
            .app_data::<actix_web::web::Data<
                Arc<crate::magician_v2::device_pairing::DevicePairingStore>,
            >>()
            .map(|data| data.get_ref().clone())
            .ok_or_else(|| {
                actix_web::error::ErrorInternalServerError(
                    "mobile credential verifier is unavailable",
                )
            })?;
        let device = pairing
            .authenticate_mobile(
                &device_id,
                &device_token,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .map_err(|_| {
                warn!("[MOBILE-ACCESS] refused device on {}", req.path());
                actix_web::error::ErrorUnauthorized("mobile device credential rejected")
            })?;
        let principal = HeaderValue::from_str(&device.principal)
            .map_err(|_| actix_web::error::ErrorUnauthorized("mobile device scope is invalid"))?;
        let workspace = HeaderValue::from_str(&device.workspace)
            .map_err(|_| actix_web::error::ErrorUnauthorized("mobile device scope is invalid"))?;
        req.headers_mut()
            .insert(HeaderName::from_static("x-principal"), principal);
        req.headers_mut()
            .insert(HeaderName::from_static("x-workspace"), workspace);
        req.extensions_mut().insert(VerifiedRequestIdentity {
            principal: device.principal,
            workspace: Some(device.workspace),
            actor_fingerprint: request_identity_fingerprint("device-actor", &device_id),
            session_fingerprint: request_identity_fingerprint("device-session", &device_token),
            authentication: VerifiedRequestAuthentication::PairedDevice,
            authentication_revision: 1,
            verified_at: chrono::Utc::now(),
        });
        return next.call(req).await;
    }

    // The Cloudflare service credential distributed at enrollment is an outer
    // edge key shared by native installs; it is never sufficient identity for
    // a Magician API call. If it reaches the origin without a per-device token,
    // reject it even when JWT verification is not configured locally. Browser
    // SSO requests carry the Access assertion, not these client-secret headers.
    let outer_id_present = req.headers().contains_key(ACCESS_CLIENT_ID_HEADER);
    let outer_secret_present = req.headers().contains_key(ACCESS_CLIENT_SECRET_HEADER);
    if (outer_id_present || outer_secret_present) && !is_esp_pair {
        warn!(
            "[MOBILE-ACCESS] refused outer credential without device identity on {}",
            req.path()
        );
        return Err(actix_web::error::ErrorUnauthorized(
            "mobile device credential required",
        ));
    }

    // The phone cannot have an outer Access or Magician credential before its
    // first exchange. Cloudflare provisioning bypasses only these exact paths;
    // the high-entropy, five-minute, single-use capability remains mandatory
    // in the handler and every other route stays behind normal authentication.
    if req.method() == actix_web::http::Method::POST
        && matches!(
            req.path(),
            MOBILE_ENROLLMENT_EXCHANGE_PATH | ANDROID_APPS_ENROLLMENT_EXCHANGE_PATH
        )
    {
        return next.call(req).await;
    }

    // Registered as an `Option` so the value exists whether or not Access is
    // configured — actix's builder chain has no clean way to add app data
    // conditionally, and a missing entry would be indistinguishable from a
    // wiring mistake.
    let verifier = req
        .app_data::<actix_web::web::Data<Option<Arc<AccessVerifier>>>>()
        .and_then(|data| data.get_ref().clone());

    // Unconfigured: ordinary local development remains permissive. The ESP
    // pairing door is stronger because it mints a durable device bearer:
    // without a configured verifier it is available only to an actual
    // loopback peer, never to an arbitrary remote caller.
    let Some(verifier) = verifier else {
        let loopback = is_direct_loopback_request(&req);
        if is_esp_pair && !loopback {
            return Err(actix_web::error::ErrorUnauthorized(
                "ESP pairing requires loopback or verified Cloudflare Access",
            ));
        }
        if !scripted_asset
            && req.extensions().get::<VerifiedRequestIdentity>().is_none()
            && loopback
        {
            req.extensions_mut()
                .insert(trusted_loopback_request_identity());
        }
        return next.call(req).await;
    };

    let presented = req
        .headers()
        .get(ACCESS_JWT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let Some(token) = presented else {
        let loopback = is_direct_loopback_request(&req);
        if (is_esp_pair || verifier.mode() == AccessMode::Require) && !loopback {
            warn!("[CF-ACCESS] refused {} — no assertion", req.path());
            return Err(actix_web::error::ErrorUnauthorized(
                "cloudflare access assertion required",
            ));
        }
        if !scripted_asset
            && loopback
            && req.extensions().get::<VerifiedRequestIdentity>().is_none()
        {
            req.extensions_mut()
                .insert(trusted_loopback_request_identity());
        }
        return next.call(req).await;
    };

    match verifier.verify(&token).await {
        Ok(identity) => {
            if is_esp_pair {
                // Service tokens deliberately have no principal. Their signed
                // assertion is sufficient only for this exact bootstrap; the
                // handler owns the fixed scope and subsequent device bearer.
                req.extensions_mut().insert(VerifiedEspPairBootstrap);
                return next.call(req).await;
            }
            // Access is an outer edge gate, not an API scope selector. The
            // auth middleware has already engraved principal/workspace from
            // the Magician bearer (or anonymous/default in local open mode),
            // and this layer must never create a hybrid Access-principal plus
            // bearer-workspace identity.
            let principal = identity.principal().ok_or_else(|| {
                warn!(
                    "[CF-ACCESS] refused non-human identity without paired device on {}",
                    req.path()
                );
                actix_web::error::ErrorUnauthorized("mobile device credential required")
            })?;
            let bearer_identity = req.extensions().get::<VerifiedRequestIdentity>().is_some();
            let public_auth_path =
                crate::magician_v2::auth::middleware::is_public_auth_path(req.path());
            if !bearer_identity && !public_auth_path && !scripted_asset {
                return Err(actix_web::error::ErrorUnauthorized(
                    "a scoped Magician bearer token is required",
                ));
            }
            let actor_material = if identity.subject.trim().is_empty() {
                principal.as_str()
            } else {
                identity.subject.as_str()
            };
            let actor_fingerprint = request_identity_fingerprint("access-actor", actor_material);
            let session_fingerprint = request_identity_fingerprint("access-session", &token);
            if !bearer_identity && !scripted_asset {
                req.extensions_mut().insert(VerifiedRequestIdentity {
                    principal,
                    workspace: None,
                    actor_fingerprint,
                    session_fingerprint,
                    authentication: VerifiedRequestAuthentication::CloudflareAccess,
                    authentication_revision: 1,
                    verified_at: chrono::Utc::now(),
                });
            }
            next.call(req).await
        },
        Err(error) => {
            warn!("[CF-ACCESS] refused {}: {}", req.path(), error);
            Err(actix_web::error::ErrorUnauthorized(
                "cloudflare access assertion rejected",
            ))
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    async fn echo_scope(req: actix_web::HttpRequest) -> actix_web::HttpResponse {
        let principal = req
            .headers()
            .get("X-Principal")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let workspace = req
            .headers()
            .get("X-Workspace")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        actix_web::HttpResponse::Ok().json(serde_json::json!({
            "principal": principal,
            "workspace": workspace,
        }))
    }

    #[test]
    fn email_is_preferred_over_a_service_token_name() {
        let identity = VerifiedIdentity {
            email: Some("someone@example.com".to_string()),
            common_name: Some("android-companion".to_string()),
            subject: "abc".to_string(),
        };
        assert_eq!(identity.principal().as_deref(), Some("someone@example.com"));
    }

    #[test]
    fn a_service_token_never_becomes_a_scope_principal() {
        let identity = VerifiedIdentity {
            email: None,
            common_name: Some("android-companion".to_string()),
            subject: String::new(),
        };
        assert!(identity.principal().is_none());
    }

    /// A token proving nothing about who sent it must not become a principal.
    #[test]
    fn an_identity_with_neither_claim_yields_no_principal() {
        let identity = VerifiedIdentity {
            email: None,
            common_name: Some("   ".to_string()),
            subject: "abc".to_string(),
        };
        assert!(identity.principal().is_none());
    }

    #[test]
    fn loopback_is_recognised_on_both_families() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        assert!(is_loopback_peer(Some(IpAddr::V4(Ipv4Addr::LOCALHOST))));
        assert!(is_loopback_peer(Some(IpAddr::V6(Ipv6Addr::LOCALHOST))));
        assert!(!is_loopback_peer(Some(IpAddr::V4(Ipv4Addr::new(
            203, 0, 113, 5
        )))));
        // No peer address at all is not loopback: unknown must not mean trusted.
        assert!(!is_loopback_peer(None));
    }

    #[test]
    fn access_issuer_accepts_team_name_hostname_or_full_origin() {
        assert_eq!(
            access_issuer("example-team"),
            "https://example-team.cloudflareaccess.com"
        );
        assert_eq!(
            access_issuer("example-team.cloudflareaccess.com"),
            "https://example-team.cloudflareaccess.com"
        );
        assert_eq!(
            access_issuer("https://example-team.cloudflareaccess.com/"),
            "https://example-team.cloudflareaccess.com"
        );
    }

    #[actix_web::test]
    async fn paired_mobile_identity_replaces_phone_controlled_scope() {
        use actix_web::{middleware::from_fn, test, web, App};

        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::magician_v2::device_pairing::DevicePairingStore::open(temp.path())
                .await
                .unwrap(),
        );
        let key = crate::magician_v2::device_bridge::DeviceKey::new(
            "owner@example.com",
            "private",
            "phone-1",
        );
        let token = store.pair(key, "Phone", 1_000).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(store))
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .route("/ordinary", web::get().to(echo_scope)),
        )
        .await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/ordinary")
                .insert_header((MOBILE_DEVICE_ID_HEADER, "phone-1"))
                .insert_header(("Authorization", format!("Bearer {token}")))
                .insert_header(("X-Principal", "attacker"))
                .insert_header(("X-Workspace", "elsewhere"))
                .to_request(),
        )
        .await;
        assert!(response.status().is_success());
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["principal"], "owner@example.com");
        assert_eq!(body["workspace"], "private");
    }

    #[actix_web::test]
    async fn shared_outer_credential_is_not_a_mobile_identity() {
        use actix_web::{middleware::from_fn, test, web, App};

        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::magician_v2::device_pairing::DevicePairingStore::open(temp.path())
                .await
                .unwrap(),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(store))
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .route("/ordinary", web::get().to(echo_scope)),
        )
        .await;

        let result = test::try_call_service(
            &app,
            test::TestRequest::get()
                .uri("/ordinary")
                .insert_header((ACCESS_CLIENT_ID_HEADER, "shared-id"))
                .insert_header((ACCESS_CLIENT_SECRET_HEADER, "shared-secret"))
                .to_request(),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("the shared outer credential must be rejected"),
        };
        assert_eq!(
            error.as_response_error().status_code(),
            actix_web::http::StatusCode::UNAUTHORIZED
        );
    }

    #[actix_web::test]
    async fn unconfigured_remote_esp_pairing_cannot_mint_a_device_bearer() {
        use actix_web::{middleware::from_fn, test, web, App};

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .route(ESP_DEVICE_PAIR_PATH, web::post().to(echo_scope)),
        )
        .await;

        let result = test::try_call_service(
            &app,
            test::TestRequest::post()
                .uri(ESP_DEVICE_PAIR_PATH)
                .to_request(),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("a non-loopback pairing request must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.as_response_error().status_code(),
            actix_web::http::StatusCode::UNAUTHORIZED
        );
    }

    #[actix_web::test]
    async fn unconfigured_loopback_esp_pairing_keeps_local_bootstrap_working() {
        use actix_web::{middleware::from_fn, test, web, App};

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .route(ESP_DEVICE_PAIR_PATH, web::post().to(echo_scope)),
        )
        .await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri(ESP_DEVICE_PAIR_PATH)
                .peer_addr("127.0.0.1:43123".parse().unwrap())
                .to_request(),
        )
        .await;
        assert!(response.status().is_success());
    }

    #[actix_web::test]
    async fn proxied_loopback_esp_pairing_still_requires_an_access_assertion() {
        use actix_web::{middleware::from_fn, test, web, App};

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .route(ESP_DEVICE_PAIR_PATH, web::post().to(echo_scope)),
        )
        .await;

        let result = test::try_call_service(
            &app,
            test::TestRequest::post()
                .uri(ESP_DEVICE_PAIR_PATH)
                .peer_addr("127.0.0.1:43123".parse().unwrap())
                .insert_header(("CF-Connecting-IP", "203.0.113.41"))
                .to_request(),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("a proxied request must not inherit the loopback bypass"),
            Err(error) => error,
        };
        assert_eq!(
            error.as_response_error().status_code(),
            actix_web::http::StatusCode::UNAUTHORIZED
        );
    }
}
