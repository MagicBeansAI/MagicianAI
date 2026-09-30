//! The owner surface for the two reads — the market, and claim health.
//!
//! Doc: `docs/components/magician/outcome-learning.md`.
//!
//! [`super::market_read`] and [`super::claim_health`] are derivations, not
//! writers: they open stores, fold, and return a description. A derivation
//! whose only caller is a background loop is a derivation whose answer nobody
//! ever sees, so these two get a route instead of a tick. The one thing here
//! that writes is nothing.
//!
//! # Registered inside `/api/magician/v2`
//!
//! Both routes answer questions about the owner's own workspace — what the
//! market did with their rooms, and whether their claims have gone stale — so
//! they sit behind the same gate as the rest of the owner surface. There is no
//! counterparty-facing half.
//!
//! # Why the claim report is a POST
//!
//! It reads and changes nothing, and it is still a POST, because a claim
//! reference is opaque caller text: it may hold a comma, a slash or a space,
//! and a query parameter split on any delimiter would silently truncate one —
//! after which the report answers *"nothing is stale"* about a claim it never
//! looked at. A JSON array cannot be truncated by punctuation.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::evidence::outward_assertions::{OutwardAssertionStore, OutwardScope};

use super::super::feeders::{NotAttributed, NotConvertible};
use super::super::retirement::RetirementPolicy;
use super::super::store::OutcomeScope;
use super::{claim_health, market_read, ClaimHealth, MarketRead};

/// §4's window: *"this answer has not been used in six months."*
const DEFAULT_IDLE_AFTER_DAYS: i64 = 180;

/// The two reads, bound to a workspace layout.
#[derive(Clone)]
pub struct OutcomeLearningSurface {
    workspace_layout: ArtifactV2Workspace,
}

impl OutcomeLearningSurface {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }
}

// ── Wire shapes ─────────────────────────────────────────────────────────────

/// One document, on the wire.
///
/// Counts beside their totals, exactly as the fold produces them. There is no
/// `open_rate` field and there will not be one: *"opened by nine of eleven"* is
/// the sentence §6 asks for, and a ratio invites comparison without looking at
/// what produced it.
#[derive(Debug, Serialize)]
struct DocumentReadBody {
    document: String,
    opened_by: usize,
    shared_with: usize,
    audiences: usize,
    /// Whether the read spans enough relationships to be about the market
    /// rather than about one of them. Reported, never used to filter.
    market_backed: bool,
}

/// One `(audience, token)` pair — a share that never appeared, or one that came
/// back. Both are the same shape and neither is a count of clicks.
#[derive(Debug, Serialize)]
struct AudienceTokenBody {
    audience_kind: String,
    audience_id: String,
    token_issued_to: String,
}

#[derive(Debug, Serialize)]
struct UnattributedBody {
    room_id: String,
    token_issued_to: String,
    reason: String,
    /// Present only for an audience disagreement, where knowing both halves is
    /// the whole finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    on_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_room: Option<String>,
}

#[derive(Debug, Serialize)]
struct MarketReadBody {
    rooms_seen: usize,
    shares_seen: usize,
    documents: Vec<DocumentReadBody>,
    never_opened: Vec<AudienceTokenBody>,
    /// Tokens that came back — more than one **visit**, never more than one
    /// click.
    returned: Vec<AudienceTokenBody>,
    unattributed: Vec<UnattributedBody>,
}

impl From<MarketRead> for MarketReadBody {
    fn from(read: MarketRead) -> Self {
        Self {
            rooms_seen: read.rooms_seen,
            shares_seen: read.shares_seen,
            documents: read
                .documents
                .into_iter()
                .map(|entry| DocumentReadBody {
                    document: entry.read.document,
                    opened_by: entry.read.opened_by,
                    shared_with: entry.read.shared_with,
                    audiences: entry.read.audiences,
                    market_backed: entry.market_backed,
                })
                .collect(),
            never_opened: read
                .never_opened
                .into_iter()
                .map(|(audience, token)| AudienceTokenBody {
                    audience_kind: audience.kind.as_str().to_string(),
                    audience_id: audience.id,
                    token_issued_to: token,
                })
                .collect(),
            returned: read
                .returned
                .into_iter()
                .map(|(audience, token)| AudienceTokenBody {
                    audience_kind: audience.kind.as_str().to_string(),
                    audience_id: audience.id,
                    token_issued_to: token,
                })
                .collect(),
            unattributed: read
                .unattributed
                .into_iter()
                .map(|entry| {
                    let (reason, on_event, on_room) = match entry.reason {
                        NotAttributed::UnknownRoom => ("unknown_room", None, None),
                        NotAttributed::TokenNotShared => ("token_not_shared", None, None),
                        NotAttributed::AudienceMismatch { on_event, on_room } => {
                            ("audience_mismatch", Some(on_event), Some(on_room))
                        },
                    };
                    UnattributedBody {
                        room_id: entry.room_id,
                        token_issued_to: entry.token_issued_to,
                        reason: reason.to_string(),
                        on_event,
                        on_room,
                    }
                })
                .collect(),
        }
    }
}

/// One stale claim, on the wire.
///
/// `idle_for_days` is derived from the clock this request asked at and is
/// **not** a stored field — the type it comes from deliberately refuses to be
/// serialised for exactly that reason, so persisting this body somewhere would
/// freeze a "how stale" that stops being true a moment later.
#[derive(Debug, Serialize)]
struct RetirementBody {
    claim_ref: String,
    last_used_at: String,
    total_uses: usize,
    idle_for_days: i64,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UnresolvedBody {
    use_ref: String,
    superseded_use_ref: String,
}

#[derive(Debug, Serialize)]
struct RefusedBody {
    use_ref: String,
    reason: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct ClaimHealthBody {
    claims_asked: Vec<String>,
    /// Every claim whose whole use history the report actually holds. A finding
    /// exists for these and no others, so a claim absent from this list was not
    /// examined — which is different from one that was and looked healthy.
    claims_reached: Vec<String>,
    uses_seen: usize,
    /// Whether every supersession resolved. **False means the contradiction
    /// half may be short**: an unresolved pointer is a correction retirement
    /// could not see, so a claim re-asserted after it was corrected raises
    /// nothing. It is reported beside the findings rather than under them,
    /// because a partial report read as a complete one is worse than no report.
    complete: bool,
    retirement: Vec<RetirementBody>,
    contradictions: Vec<super::super::retirement::Contradiction>,
    unresolved: Vec<UnresolvedBody>,
    refused: Vec<RefusedBody>,
    dangling: Vec<String>,
}

impl From<ClaimHealth> for ClaimHealthBody {
    fn from(health: ClaimHealth) -> Self {
        Self {
            claims_asked: health.claims_asked,
            claims_reached: health.claims_reached,
            uses_seen: health.uses_seen,
            complete: health.complete,
            retirement: health
                .retirement
                .into_iter()
                .map(|candidate| RetirementBody {
                    claim_ref: candidate.claim_ref,
                    last_used_at: candidate.last_used_at.to_rfc3339(),
                    total_uses: candidate.total_uses,
                    idle_for_days: candidate.idle_for.num_days(),
                    evidence_refs: candidate.evidence_refs,
                })
                .collect(),
            contradictions: health.contradictions,
            unresolved: health
                .unresolved
                .into_iter()
                .map(|entry| UnresolvedBody {
                    use_ref: entry.use_ref,
                    superseded_use_ref: entry.superseded_use_ref,
                })
                .collect(),
            refused: health
                .refused
                .into_iter()
                .map(|entry| {
                    let NotConvertible::UnreadableTimestamp { value } = entry.reason;
                    RefusedBody {
                        use_ref: entry.use_ref,
                        reason: "unreadable_timestamp".to_string(),
                        value,
                    }
                })
                .collect(),
            dangling: health.dangling,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaimHealthQuery {
    /// The claims to ask about. **Required and non-empty**: see
    /// [`super::claim_health`] for why an empty list is refused rather than
    /// answered.
    pub claims: Vec<String>,
    /// How long a claim may go unused before it is worth a look, in days.
    /// Refused at or below zero: at zero every claim in the index is nominated
    /// the instant it is used, and a list that says everything says nothing.
    pub idle_after_days: Option<i64>,
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /outcome-learning/market-read` — what the market did with the rooms.
pub async fn market_read_handler(
    surface: web::Data<OutcomeLearningSurface>,
    req: HttpRequest,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let layout = surface.workspace_layout.clone();
    let scope = OutcomeScope::new(principal, workspace);
    // The fold reads a log per room and a grant file per room, which is
    // filesystem work and does not belong on the reactor.
    match web::block(move || market_read(&layout, &scope)).await {
        Ok(Ok(read)) => HttpResponse::Ok().json(MarketReadBody::from(read)),
        Ok(Err(error)) => unavailable("market read", &error),
        Err(error) => {
            tracing::error!(error = ?error, "the market read task did not complete");
            HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "outcome_learning_unavailable",
                "message": "This request could not be completed and nothing was changed.",
            }))
        },
    }
}

/// `POST /outcome-learning/claim-health` — stale and contradicted claims.
///
/// Reads and writes nothing. See the module note on why it is a POST.
pub async fn claim_health_handler(
    surface: web::Data<OutcomeLearningSurface>,
    req: HttpRequest,
    body: web::Json<ClaimHealthQuery>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let query = body.into_inner();
    let idle_after_days = query.idle_after_days.unwrap_or(DEFAULT_IDLE_AFTER_DAYS);
    let policy = match RetirementPolicy::new(Duration::days(idle_after_days)) {
        Ok(policy) => policy,
        Err(error) => return refused(&error),
    };

    let layout = surface.workspace_layout.clone();
    let scope = OutwardScope::new(principal, workspace);
    let claims = query.claims;
    let now = Utc::now();
    match web::block(move || {
        let store = OutwardAssertionStore::new(layout);
        claim_health(&store, &scope, &claims, &policy, now)
    })
    .await
    {
        Ok(Ok(health)) => HttpResponse::Ok().json(ClaimHealthBody::from(health)),
        // A refusal here is a caller error — an empty claim list, a claim ref
        // carrying the field separator — and it is answered as one rather than
        // as a fault, so an operator is not sent to look at a disk.
        Ok(Err(error)) => refused(&error),
        Err(error) => {
            tracing::error!(error = ?error, "the claim health task did not complete");
            HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "outcome_learning_unavailable",
                "message": "This request could not be completed and nothing was changed.",
            }))
        },
    }
}

/// A store that could not be read.
///
/// No detail travels outward, and **nothing is reported as partially done**:
/// both derivations write nothing, so a failure left the workspace exactly as
/// it was.
fn unavailable(what: &str, error: &anyhow::Error) -> HttpResponse {
    tracing::error!(error = ?error, what = %what, "the outcome learning surface could not complete a request");
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "outcome_learning_unavailable",
        "message": "This request could not be completed and nothing was changed. Try again \
                    shortly.",
    }))
}

/// A request this surface will not answer, with the reason the caller needs.
///
/// The message is the refusal's own, because every one of them names something
/// the caller can fix — and a generic "bad request" here would leave an owner
/// unable to tell a typo from an empty report.
fn refused(error: &anyhow::Error) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "error": "outcome_learning_refused",
        "message": format!("{error}"),
    }))
}

/// Mounts the two reads.
///
/// **The named entry point** for
/// [`room_attention_from_access`](super::super::feeders::room_attention_from_access)
/// and [`claim_uses_from_index`](super::super::feeders::claim_uses_from_index).
pub fn configure_outcome_learning_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/outcome-learning/market-read",
        web::get().to(market_read_handler),
    )
    .route(
        "/outcome-learning/claim-health",
        web::post().to(claim_health_handler),
    );
}
