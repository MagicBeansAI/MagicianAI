//! **What did we actually say in that room, and who checked?**
//!
//! Doc: `docs/plans/2026-08-07-opc-outward-assertions.md` §3 and Phase 3. This
//! is the surface for `magician_v2::evidence::transcript_ingestion` — the
//! module that ingests an observed transcript, surfaces what it appeared to
//! claim, and lets a **named person** confirm or reject each candidate.
//!
//! # What this closes
//!
//! Three modules were built, tested, and reachable from nothing:
//! `evidence::observed_statements`, `evidence::transcript_ingestion` and
//! `commitments`. `transcript_ingestion`'s own header said it plainly — nothing
//! in the workspace constructed a `TranscriptIngestion` outside its tests. So
//! *"an unconfirmed extraction is surfaced rather than dropped"* was true the
//! way every statement about the members of an empty set is true, and the
//! commitment register sat behind a door nobody could open.
//!
//! # The rules this surface does NOT get to soften
//!
//! Every one of them belongs to the module and is enforced there. They are
//! listed because a reader of this file should know what an HTTP caller cannot
//! do, not because this file checks them:
//!
//! - **Confirmation names a person, and it may not be the extractor.** An
//!   extractor confirming its own reading of a room is the self-grant the
//!   named-decider rule exists to prevent, so `POST /confirm` requires `by` and
//!   the store refuses it when it matches `extracted_by`.
//! - **Nothing here writes an assertion.** Ingestion produces candidates in
//!   `pending`, always; there is no request field that makes one confirmed.
//! - **Rejection is terminal, and refused once assertion rows exist.**
//! - **The act is recorded whether or not one word was understood.** A room
//!   whose record only exists when extraction succeeded is a room that
//!   disappears exactly when the extractor is worst.
//!
//! # Why an owner route rather than a meeting-sink hook
//!
//! The obvious wiring — have the meeting transcript sink call this on every
//! session — would ingest every internal standup as an outward disclosure.
//! Which rooms are *outward* is a judgement about the relationship, and the
//! runtime cannot see it: the sink knows there was a meeting, not that the
//! people in it were a counterparty. So the caller supplies it. A later
//! automatic feeder is one more caller of the same store, not a change here.

use actix_web::{http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use magician::magician_v2::agents::ConsequenceClass;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{AudienceKind, AudienceRef};
use magician::magician_v2::commitments::{
    CommitmentDirection, CommitmentScope, Commitments, RecordCommitment,
};
use magician::magician_v2::evidence::{
    OutwardChannel, OutwardScope, OwnerDecision, SpeakerAttribution, TranscriptClaimStatus,
    TranscriptIngestion, TranscriptSource, TranscriptUtterance,
};

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;

pub struct TranscriptClaimsApi {
    workspace_layout: ArtifactV2Workspace,
}

impl TranscriptClaimsApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn ingestion(&self) -> TranscriptIngestion {
        TranscriptIngestion::new(self.workspace_layout.clone())
    }

    fn commitments(&self) -> Commitments {
        Commitments::new(self.workspace_layout.clone())
    }
}

// ---------------------------------------------------------------------------
// Shared answers
// ---------------------------------------------------------------------------

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

/// A store that could not be read.
///
/// **Never an empty listing.** An unreadable claim register folded to "no
/// pending claims" tells the owner there is nothing to check, which is the one
/// wrong answer that stops anybody looking.
fn store_unreadable(what: &str, error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "transcript_store_unreadable",
        format!("{what} could not be read, so the answer is unknown — and unknown is not `none`"),
        Some(json!({ "detail": format!("{error:#}") })),
    )
}

/// A write the store refused.
///
/// `409`, not `500`: every refusal here is a rule about what may be recorded —
/// an extractor confirming its own claim, a rejection of something already
/// asserted, a replay under a key whose words changed. All of them are
/// actionable, and a `500` would bury the sentence that says which.
fn refused(code: &str, error: &anyhow::Error) -> HttpResponse {
    api_error_response(StatusCode::CONFLICT, code, format!("{error:#}"), None)
}

fn blocking_store_unavailable(what: &str, error: &actix_web::error::BlockingError) -> HttpResponse {
    api_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "transcript_store_worker_unavailable",
        format!("{what} could not complete on the blocking store worker: {error}"),
        None,
    )
}

fn resolve_scope(
    req: &HttpRequest,
    workspace: Option<String>,
) -> Result<OutwardScope, HttpResponse> {
    let (principal, workspace) = resolve_required_scope(req.headers(), workspace)?;
    for (label, value) in [
        ("the principal", principal.as_str()),
        ("the workspace", workspace.as_str()),
    ] {
        if let Err(message) = crate::work_modules_api::guard_id_component(label, value) {
            return Err(bad_request(message));
        }
    }
    Ok(OutwardScope::new(principal, workspace))
}

/// Naming a reviewer is attribution, not authentication. In particular, the
/// channel daemon that acknowledges a send cannot also confirm it as a person.
fn resolve_owner_scope(
    req: &HttpRequest,
    workspace: Option<String>,
) -> Result<OutwardScope, HttpResponse> {
    use magician::magician_v2::{
        auth::{middleware::authenticated, BearerKind},
        cloudflare_access::{VerifiedRequestAuthentication, VerifiedRequestIdentity},
    };
    let scope = if let Some(identity) = authenticated(req) {
        if !matches!(identity.bearer, BearerKind::Session(_)) {
            return Err(api_error_response(
                StatusCode::FORBIDDEN,
                "owner_session_required",
                "A claim or commitment decision requires an interactive owner session.",
                None,
            ));
        }
        let scope = OutwardScope::new(identity.scope.principal(), identity.scope.workspace());
        for (name, expected) in [
            ("X-Principal", scope.principal.as_str()),
            ("X-Workspace", scope.workspace.as_str()),
        ] {
            if req
                .headers()
                .get(name)
                .is_some_and(|value| value.to_str().ok().map(str::trim) != Some(expected))
            {
                return Err(api_error_response(
                    StatusCode::FORBIDDEN,
                    "owner_scope_mismatch",
                    "The requested scope differs from the owner session.",
                    None,
                ));
            }
        }
        scope
    } else {
        // Paired devices and verified local/browser sessions share the existing
        // host authority checks. A bearer stamp without its authenticated kind
        // cannot be promoted to an interactive session through this fallback.
        if req
            .extensions()
            .get::<VerifiedRequestIdentity>()
            .is_some_and(|identity| {
                identity.authentication() == VerifiedRequestAuthentication::MagicianBearer
            })
        {
            return Err(api_error_response(
                StatusCode::UNAUTHORIZED,
                "owner_session_required",
                "An authenticated owner session is required.",
                None,
            ));
        }
        let bound = crate::apps_api::authenticated_app_scope(req, &Utc::now())?;
        OutwardScope::new(
            bound.scope().principal.as_str(),
            bound.scope().workspace.as_str(),
        )
    };
    if workspace
        .as_deref()
        .is_some_and(|requested| requested.trim() != scope.workspace)
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "owner_scope_mismatch",
            "The requested workspace differs from the owner session.",
            None,
        ));
    }
    Ok(scope)
}

// ---------------------------------------------------------------------------
// Ingestion
// ---------------------------------------------------------------------------

/// One utterance, as a caller states it.
#[derive(Debug, Deserialize)]
pub struct UtteranceBody {
    pub segment_key: String,
    /// `ours` / `counterparty` / `unresolved`, with `speaker_id` for the first
    /// two. Three states rather than an optional name: *"we could not tell who
    /// said this"* and *"the other side said this"* lead to different records,
    /// and neither may be read as *"we said this"*.
    pub attribution: String,
    #[serde(default)]
    pub speaker_id: Option<String>,
    pub spoken_text: String,
    #[serde(default)]
    pub claim_ref: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

impl UtteranceBody {
    fn into_utterance(self) -> Result<TranscriptUtterance, String> {
        // A `match` with an explicit unknown arm, not a permissive default. An
        // unrecognised attribution falling through to `Unresolved` would be
        // safe; falling through to `Ours` would attribute a claim to us on a
        // typo, and a caller cannot tell which a silent default chose.
        let speaker = match self.attribution.trim().to_ascii_lowercase().as_str() {
            "ours" => SpeakerAttribution::Ours(named(self.speaker_id, "ours")?),
            "counterparty" => {
                SpeakerAttribution::Counterparty(named(self.speaker_id, "counterparty")?)
            },
            "unresolved" => SpeakerAttribution::Unresolved,
            other => {
                return Err(format!(
                    "unknown speaker attribution `{other}`; expected `ours`, `counterparty` or \
                     `unresolved`. There is no default: an unrecognised value read as `ours` \
                     would put a claim on our record that nobody attributed to us"
                ))
            },
        };
        Ok(TranscriptUtterance {
            segment_key: self.segment_key,
            speaker,
            spoken_text: self.spoken_text,
            claim_ref: self.claim_ref,
            evidence_refs: self.evidence_refs,
        })
    }
}

fn named(value: Option<String>, attribution: &str) -> Result<String, String> {
    match value {
        Some(who) if !who.trim().is_empty() => Ok(who),
        _ => Err(format!(
            "attribution `{attribution}` names an identity, so `speaker_id` is required"
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct IngestTranscriptRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Stable key for the whole transcript. The act ref derives from it, so
    /// re-ingesting resumes one act rather than recording the room twice.
    pub transcript_key: String,
    /// Our side's identity in the room, as the runtime resolved it.
    pub effective_speaker: String,
    /// Who was in the room. One assertion row per person on confirmation, so a
    /// later correction can be aimed individually — which is why the store
    /// refuses an empty list rather than writing zero rows and reporting
    /// success.
    pub attendees: Vec<String>,
    /// Who produced the candidates. Recorded so that confirmation can refuse to
    /// come from it.
    pub extracted_by: String,
    /// When the words were SAID, which is not when they were ingested.
    pub occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub engagement_id: Option<String>,
    #[serde(default)]
    pub program_id: Option<String>,
    /// One canonical audience kind (`engagement`, `program`, `account`,
    /// `panel`, or `person`) with `audience_id`. Required by the commitment
    /// bridge, which refuses without it: a term with no relationship is nothing
    /// anybody can look up later.
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// What this room cost if what was said in it was wrong. Supplied, never
    /// invented — this surface cannot see whether a figure read out was
    /// confidential, and understating the class is the direction that misleads
    /// a later reviewer. Defaults to bounded communication, the class of words
    /// said to people already in the room.
    #[serde(default)]
    pub consequence_class: Option<String>,
    pub utterances: Vec<UtteranceBody>,
}

fn audience_ref(kind: Option<&str>, id: Option<&str>) -> Result<Option<AudienceRef>, String> {
    match (kind, id) {
        (None, None) => Ok(None),
        (Some(kind), Some(id)) if !id.trim().is_empty() => {
            let normalized = kind.trim().to_ascii_lowercase();
            let kind = AudienceKind::parse(&normalized)
                .ok_or_else(|| format!("unknown audience kind `{normalized}`"))?;
            Ok(Some(AudienceRef::new(kind, id)))
        },
        _ => Err("an audience needs both `audience_kind` and `audience_id`".to_string()),
    }
}

fn consequence_class(named: Option<&str>) -> Result<ConsequenceClass, String> {
    let Some(named) = named else {
        return Ok(ConsequenceClass::BoundedCommunication);
    };
    match named.trim().to_ascii_lowercase().as_str() {
        "private_local" => Err(
            "a transcript of an observed room is not private/local: the words already reached \
             somebody. Recording it as the one class that needs no gate would put an outward \
             act outside every later review"
                .to_string(),
        ),
        "bounded_communication" => Ok(ConsequenceClass::BoundedCommunication),
        "confidential_disclosure" => Ok(ConsequenceClass::ConfidentialDisclosure),
        "submission_or_publication" => Ok(ConsequenceClass::SubmissionOrPublication),
        "commitment_or_transaction" => Ok(ConsequenceClass::CommitmentOrTransaction),
        other => Err(format!("unknown consequence class `{other}`")),
    }
}

/// `POST /api/magician/v2/transcripts/ingest`
pub async fn ingest_transcript_handler(
    req: HttpRequest,
    body: web::Json<IngestTranscriptRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match audience_ref(body.audience_kind.as_deref(), body.audience_id.as_deref()) {
        Ok(audience) => audience,
        Err(message) => return bad_request(message),
    };
    let class = match consequence_class(body.consequence_class.as_deref()) {
        Ok(class) => class,
        Err(message) => return bad_request(message),
    };

    let mut utterances = Vec::with_capacity(body.utterances.len());
    for utterance in body.utterances {
        match utterance.into_utterance() {
            Ok(utterance) => utterances.push(utterance),
            Err(message) => return bad_request(message),
        }
    }

    let mut source = TranscriptSource::observed(
        body.transcript_key,
        body.effective_speaker,
        body.attendees,
        body.extracted_by,
        body.occurred_at,
    );
    // `Meeting` is what `observed` chooses, and it is the only channel this
    // route offers. The store refuses a CONTROLLED channel — one the runtime
    // could have recorded before the words left — and letting a caller name the
    // channel would let it file a mail we composed as a room we merely
    // overheard, which is the one direction that erases a disclosure.
    source.channel = OutwardChannel::Meeting;
    source.audience = audience;
    source.engagement_id = body.engagement_id;
    source.program_id = body.program_id;
    source.consequence_class = class;

    let ingestion = api.ingestion();
    match web::block(move || ingestion.ingest_transcript(&scope, &source, &utterances, Utc::now()))
        .await
    {
        Ok(Ok(ingested)) => HttpResponse::Ok().json(json!({
            "outward_act_ref": ingested.disclosure.outward_act_ref,
            "utterances_seen": ingested.utterances_seen,
            // Counts, not rates: "eleven of forty" is a fact about this
            // transcript; a percentage hides whether the run saw forty
            // utterances or four.
            "claims": ingested.claims,
            "skipped": ingested
                .skipped
                .iter()
                .map(|skipped| json!({
                    "segment_key": skipped.segment_key,
                    "reason": skipped.reason.as_str(),
                }))
                .collect::<Vec<_>>(),
        })),
        Ok(Err(error)) => refused("transcript_refused", &error),
        Err(error) => blocking_store_unavailable("transcript ingestion", &error),
    }
}

// ---------------------------------------------------------------------------
// The queue, and deciding on it
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClaimListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// `pending` (the default) or `all`. Deciding between them here rather than
    /// filtering client-side keeps the pending queue one answer: a UI that
    /// filtered a full list would show a decided claim as open for one render.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub after_claim_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

/// `GET /api/magician/v2/transcripts/claims`
pub async fn list_claims_handler(
    req: HttpRequest,
    query: web::Query<ClaimListQuery>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let query = query.into_inner();
    let scope = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let ingestion = api.ingestion();
    let wanted = query.state.unwrap_or_else(|| "pending".to_owned());
    if !["pending", "confirmed", "rejected", "all"].contains(&wanted.as_str()) {
        return bad_request("unknown state; expected pending, confirmed, rejected or all");
    }
    match web::block(move || -> anyhow::Result<serde_json::Value> {
        let mut claims = ingestion.claims(&scope)?;
        let pending = claims.iter().filter(|c| c.status == TranscriptClaimStatus::Pending).count();
        let confirmed = claims.iter().filter(|c| c.status == TranscriptClaimStatus::Confirmed).count();
        let rejected = claims.len() - pending - confirmed;
        let search = query.text.unwrap_or_default().to_lowercase();
        claims.retain(|c| (wanted == "all" || c.status.as_str() == wanted)
            && (search.is_empty() || c.stated_text.to_lowercase().contains(&search)
                || c.speaker.to_lowercase().contains(&search) || c.audience.iter().any(|a| a.to_lowercase().contains(&search))));
        let count = claims.len();
        let mut next_cursor = None;
        if let Some(limit) = query.limit {
            let limit = limit.clamp(1, 100);
            claims.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));
            if let Some(after) = query.after_claim_id { claims.retain(|c| c.claim_id > after); }
            if claims.len() > limit {
                claims.truncate(limit);
                next_cursor = claims.last().map(|c| c.claim_id.clone());
            }
        }
        let mut delivery = serde_json::Map::new();
        for claim in &claims {
            if let Some(act) = ingestion.assertions().load_act(&scope, &claim.outward_act_ref)? {
                delivery.insert(claim.claim_id.clone(), json!({"status":act.status,"channel":act.channel,"observed":act.observed}));
            }
        }
        Ok(json!({"state":wanted,"count":count,"pending":pending,"counts":{"pending":pending,"confirmed":confirmed,"rejected":rejected},"claims":claims,"delivery":delivery,"next_cursor":next_cursor}))
    }).await {
        Ok(Ok(page)) => HttpResponse::Ok().json(page),
        Ok(Err(error)) => store_unreadable("the transcript claim register", &error),
        Err(error) => blocking_store_unavailable("the transcript claim register", &error),
    }
}

#[derive(Debug, Deserialize)]
pub struct DecideClaimRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Who decided. Not optional and not defaultable — an unnamed confirmation
    /// is how an automated caller would grant itself the one control this
    /// register has.
    pub by: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Required together for revision-bound app/owner retries. Legacy callers
    /// may omit both and retain the historical idempotent API shape.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub decision_id: Option<String>,
}

impl DecideClaimRequest {
    fn decision(&self) -> OwnerDecision {
        let decision = OwnerDecision::by(self.by.clone());
        match self.note.clone() {
            Some(note) => decision.with_note(note),
            None => decision,
        }
    }
}

/// `GET /api/magician/v2/transcripts/claims/{claim_id}/pending-confirmation`
///
/// Recover an admitted command after navigation or browser-state loss. This
/// owner-only read never applies it; POST /confirm still checks the exact ID,
/// reviewer, note and revision under the destination lock.
pub async fn pending_confirmation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let scope = match resolve_owner_scope(&req, None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let ingestion = api.ingestion();
    let claim_id = path.into_inner();
    match web::block(move || ingestion.pending_confirmation(&scope, &claim_id)).await {
        Ok(Ok(confirmation)) => HttpResponse::Ok().json(json!({ "confirmation": confirmation })),
        Ok(Err(error)) => store_unreadable("the saved confirmation", &error),
        Err(error) => blocking_store_unavailable("saved confirmation recovery", &error),
    }
}

/// `POST /api/magician/v2/transcripts/claims/{claim_id}/confirm`
pub async fn confirm_claim_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideClaimRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let ingestion = api.ingestion();
    let claim_id = path.into_inner();
    let decision = body.decision();
    match (body.expected_revision, body.decision_id.as_deref()) {
        (None, None) => match web::block(move || {
            ingestion.confirm_claim(&scope, &claim_id, &decision, Utc::now())
        })
        .await
        {
            Ok(Ok(claim)) => HttpResponse::Ok().json(claim),
            Ok(Err(error)) => refused("claim_confirmation_refused", &error),
            Err(error) => blocking_store_unavailable("claim confirmation", &error),
        },
        (Some(expected_revision), Some(decision_id)) => {
            let decision_id = decision_id.to_owned();
            match web::block(move || {
                ingestion.confirm_claim_at_revision(
                    &scope,
                    &claim_id,
                    expected_revision,
                    &decision_id,
                    &decision,
                    Utc::now(),
                )
            })
            .await
            {
                Ok(Ok(outcome)) => HttpResponse::Ok().json(outcome),
                Ok(Err(error)) => refused("claim_confirmation_refused", &error),
                Err(error) => blocking_store_unavailable("claim confirmation", &error),
            }
        },
        _ => bad_request("expected_revision and decision_id must be supplied together"),
    }
}

/// `POST /api/magician/v2/transcripts/claims/{claim_id}/reject`
pub async fn reject_claim_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideClaimRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let ingestion = api.ingestion();
    let claim_id = path.into_inner();
    let decision = body.decision();
    match (body.expected_revision, body.decision_id.as_deref()) {
        (None, None) => match web::block(move || {
            ingestion.reject_claim(&scope, &claim_id, &decision, Utc::now())
        })
        .await
        {
            Ok(Ok(claim)) => HttpResponse::Ok().json(claim),
            Ok(Err(error)) => refused("claim_rejection_refused", &error),
            Err(error) => blocking_store_unavailable("claim rejection", &error),
        },
        (Some(expected_revision), Some(decision_id)) => {
            let decision_id = decision_id.to_owned();
            match web::block(move || {
                ingestion.reject_claim_at_revision(
                    &scope,
                    &claim_id,
                    expected_revision,
                    &decision_id,
                    &decision,
                    Utc::now(),
                )
            })
            .await
            {
                Ok(Ok(outcome)) => HttpResponse::Ok().json(outcome),
                Ok(Err(error)) => refused("claim_rejection_refused", &error),
                Err(error) => blocking_store_unavailable("claim rejection", &error),
            }
        },
        _ => bad_request("expected_revision and decision_id must be supplied together"),
    }
}

#[derive(Debug, Deserialize)]
pub struct RecordCommitmentRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub expected_claim_revision: Option<u64>,
    #[serde(default)]
    pub decision_id: Option<String>,
}

/// `POST /api/magician/v2/transcripts/claims/{claim_id}/commitment`
///
/// Record what the room appeared to commit us to, as an **unconfirmed** term.
///
/// The register's rule is the point: a machine may only write the unconfirmed
/// state, and confirming a commitment is a separate act by a named person. So
/// this route can put a term on the record and cannot make it binding, which is
/// exactly the split that lets a transcript feed the register at all.
pub async fn record_commitment_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordCommitmentRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let commitments = api.commitments();
    let ingestion = api.ingestion();
    let claim_id = path.into_inner();
    match (body.expected_claim_revision, body.decision_id.as_deref()) {
        (None, None) => match web::block(move || {
            ingestion.record_commitment_from_claim(&commitments, &scope, &claim_id, Utc::now())
        })
        .await
        {
            Ok(Ok(commitment)) => HttpResponse::Ok().json(commitment),
            Ok(Err(error)) => refused("commitment_refused", &error),
            Err(error) => blocking_store_unavailable("commitment recording", &error),
        },
        (Some(expected_revision), Some(decision_id)) => {
            let decision_id = decision_id.to_owned();
            match web::block(move || {
                ingestion.record_commitment_from_claim_at_revision(
                    &commitments,
                    &scope,
                    &claim_id,
                    expected_revision,
                    &decision_id,
                    Utc::now(),
                )
            })
            .await
            {
                Ok(Ok(outcome)) => HttpResponse::Ok().json(outcome),
                Ok(Err(error)) => refused("commitment_refused", &error),
                Err(error) => blocking_store_unavailable("commitment recording", &error),
            }
        },
        _ => bad_request("expected_claim_revision and decision_id must be supplied together"),
    }
}

#[derive(Debug, Deserialize)]
pub struct CounterpartyOfferRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// One canonical audience kind, with `audience_id`.
    pub audience_kind: String,
    pub audience_id: String,
    /// What they said they would do, in the words they said it in.
    pub terms: String,
    /// The message, transcript or act it was said in. Required by the store:
    /// the owner confirms by reading the words, not a summary.
    pub source_ref: String,
    /// When THEY said it, RFC 3339. Not when it was typed up.
    pub stated_at: DateTime<Utc>,
}

/// `POST /api/magician/v2/transcripts/counterparty-offers`
///
/// Record what the OTHER side said they would do.
///
/// # Why this is a separate route from the claim path
///
/// `CommitmentDirection::OfferedToUs` had no producer anywhere — the only
/// writer was `commitment_request_from_claim`, which hardcodes `StatedByUs`,
/// and correctly so: only utterances resolved to one of OUR identities become
/// transcript claims at all, and a counterparty's words are skipped with
/// `SpokenByCounterparty`. So half of the register's model was unreachable, and
/// *"what did they promise us"* could not be asked.
///
/// # It accepts one direction, and refusing the other is the point
///
/// Our own terms may only reach the register through a **confirmed claim**,
/// which requires a named person who did not extract it. That control exists
/// because recording something as ours puts words in our mouth that a later
/// message will be composed from. Recording what somebody ELSE offered asserts
/// nothing on our behalf, so it needs no such split — and letting this route
/// write `stated_by_us` would be a second door around the one control the
/// claim register has.
///
/// The term lands **unconfirmed** either way. A machine may only write that
/// state; confirming is a separate act by a named person.
pub async fn record_counterparty_offer_handler(
    req: HttpRequest,
    body: web::Json<CounterpartyOfferRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match audience_ref(Some(&body.audience_kind), Some(&body.audience_id)) {
        Ok(Some(audience)) => audience,
        Ok(None) => return bad_request("an audience is required"),
        Err(message) => return bad_request(message),
    };
    let request = RecordCommitment {
        audience,
        source_ref: body.source_ref,
        // Fixed, not taken from the caller. See the note above.
        direction: CommitmentDirection::OfferedToUs,
        terms: body.terms,
        stated_at: body.stated_at,
    };
    let commitment_scope = CommitmentScope::new(scope.principal.clone(), scope.workspace.clone());
    let commitments = api.commitments();
    match web::block(move || commitments.record(&commitment_scope, &request, Utc::now())).await {
        Ok(Ok(commitment)) => HttpResponse::Ok().json(commitment),
        Ok(Err(error)) => refused("counterparty_offer_refused", &error),
        Err(error) => blocking_store_unavailable("counterparty offer recording", &error),
    }
}

#[derive(Debug, Deserialize)]
pub struct CommitmentListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmCommitmentRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    pub by: String,
    pub expected_revision: u64,
    pub decision_id: String,
}

/// `POST /api/magician/v2/transcripts/commitments/{commitment_id}/confirm`
///
/// This closes the API/port parity gap: both surfaces call the same
/// revision-bound transition and the store persists the same atomic receipt.
pub async fn confirm_commitment_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ConfirmCommitmentRequest>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match resolve_owner_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match audience_ref(Some(&body.audience_kind), Some(&body.audience_id)) {
        Ok(Some(audience)) => audience,
        Ok(None) => return bad_request("an audience is required"),
        Err(message) => return bad_request(message),
    };
    let commitment_scope = CommitmentScope::new(scope.principal, scope.workspace);
    let commitments = api.commitments();
    let commitment_id = path.into_inner();
    match web::block(move || {
        commitments.confirm_at_revision(
            &commitment_scope,
            &audience,
            &commitment_id,
            body.expected_revision,
            &body.decision_id,
            &body.by,
            Utc::now(),
        )
    })
    .await
    {
        Ok(Ok(outcome)) => HttpResponse::Ok().json(outcome),
        Ok(Err(error)) => refused("commitment_confirmation_refused", &error),
        Err(error) => blocking_store_unavailable("commitment confirmation", &error),
    }
}

/// `GET /api/magician/v2/transcripts/commitments`
///
/// What we are on the record as having said we would do, for one relationship.
///
/// Here rather than on its own surface because this is the register's only
/// reader, and a second one would be a second answer to *"what did we promise"*.
pub async fn list_commitments_handler(
    req: HttpRequest,
    query: web::Query<CommitmentListQuery>,
    api: web::Data<TranscriptClaimsApi>,
) -> HttpResponse {
    let query = query.into_inner();
    let scope = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match audience_ref(Some(&query.audience_kind), Some(&query.audience_id)) {
        Ok(Some(audience)) => audience,
        Ok(None) => return bad_request("an audience is required"),
        Err(message) => return bad_request(message),
    };
    let commitment_scope = CommitmentScope::new(scope.principal.clone(), scope.workspace.clone());
    let store = api.commitments();
    let read_audience = audience.clone();
    match web::block(move || store.for_audience(&commitment_scope, &read_audience)).await {
        Ok(Ok(commitments)) => {
            let restatable = commitments
                .iter()
                .filter(|held| held.may_be_restated_outward())
                .count();
            HttpResponse::Ok().json(json!({
                "audience": audience,
                "count": commitments.len(),
                // The one predicate a consumer should ask. Reported beside the
                // total so a caller cannot read "twelve commitments" as twelve
                // things it may repeat to somebody.
                "restatable_outward": restatable,
                "commitments": commitments,
            }))
        },
        Ok(Err(error)) => store_unreadable("the commitment register", &error),
        Err(error) => blocking_store_unavailable("the commitment register", &error),
    }
}

pub fn configure_transcript_claim_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/transcripts/ingest",
        web::post().to(ingest_transcript_handler),
    )
    .route("/transcripts/claims", web::get().to(list_claims_handler))
    .route(
        "/transcripts/claims/{claim_id}/pending-confirmation",
        web::get().to(pending_confirmation_handler),
    )
    .route(
        "/transcripts/claims/{claim_id}/confirm",
        web::post().to(confirm_claim_handler),
    )
    .route(
        "/transcripts/claims/{claim_id}/reject",
        web::post().to(reject_claim_handler),
    )
    .route(
        "/transcripts/claims/{claim_id}/commitment",
        web::post().to(record_commitment_handler),
    )
    .route(
        "/transcripts/commitments",
        web::get().to(list_commitments_handler),
    )
    .route(
        "/transcripts/commitments/{commitment_id}/confirm",
        web::post().to(confirm_commitment_handler),
    )
    .route(
        "/transcripts/counterparty-offers",
        web::post().to(record_counterparty_offer_handler),
    );
}

#[cfg(test)]
mod envoy_claims_tests {
    use super::*;
    use actix_web::{body::to_bytes, test::TestRequest};

    fn request(principal: &str) -> HttpRequest {
        TestRequest::default()
            .insert_header(("X-Principal", principal))
            .insert_header(("X-Workspace", "work"))
            .to_http_request()
    }

    async fn page(
        api: &web::Data<TranscriptClaimsApi>,
        principal: &str,
        query: &str,
    ) -> serde_json::Value {
        let response = list_claims_handler(
            request(principal),
            web::Query::from_query(query).unwrap(),
            api.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body()).await.unwrap()).unwrap()
    }

    #[actix_web::test]
    async fn envoy_claims_queue_paginates_filters_and_keeps_tenants_separate() {
        let dir = tempfile::tempdir().unwrap();
        let api = web::Data::new(TranscriptClaimsApi::new(ArtifactV2Workspace::new(
            dir.path(),
        )));
        for (key, words) in [
            ("one", "Office opens at nine"),
            ("two", "Office closes at five"),
        ] {
            let body: IngestTranscriptRequest = serde_json::from_value(json!({
                "transcript_key":key,"effective_speaker":"Alice","attendees":["Bob"],
                "extracted_by":"manual-import","occurred_at":"2026-09-27T10:00:00Z",
                "utterances":[{"segment_key":"one","attribution":"ours","speaker_id":"Alice","spoken_text":words}]
            })).unwrap();
            assert_eq!(
                ingest_transcript_handler(request("alice"), web::Json(body), api.clone())
                    .await
                    .status(),
                StatusCode::OK
            );
        }
        let first = page(&api, "alice", "state=pending&limit=1").await;
        assert_eq!(first["count"], 2);
        assert_eq!(first["counts"]["pending"], 2);
        assert_eq!(first["claims"].as_array().unwrap().len(), 1);
        let cursor = first["next_cursor"].as_str().unwrap();
        let next = page(
            &api,
            "alice",
            &format!("state=pending&limit=1&after_claim_id={cursor}"),
        )
        .await;
        assert_ne!(
            first["claims"][0]["claim_id"],
            next["claims"][0]["claim_id"]
        );
        assert!(next["next_cursor"].is_null());
        let filtered = page(&api, "alice", "state=all&limit=40&text=OPENS").await;
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["counts"]["pending"], 2);
        assert_eq!(
            filtered["delivery"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["observed"],
            true
        );
        assert_eq!(
            page(&api, "elsewhere", "state=all&limit=40").await["count"],
            0
        );
        assert_eq!(
            page(&api, "alice", "state=rejected&limit=40").await["count"],
            0
        );
    }

    fn decision_caller(bearer: Option<magician::magician_v2::auth::BearerKind>) -> HttpRequest {
        use magician::magician_v2::auth::{middleware::AuthenticatedRequest, ScopeRef};
        let req = request("alice");
        if let Some(bearer) = bearer {
            req.extensions_mut().insert(AuthenticatedRequest {
                scope: ScopeRef::system_internal_unauthenticated("alice", "work"),
                identity_name: Some("Alice".into()),
                bearer,
            });
        }
        req
    }

    #[actix_web::test]
    async fn envoy_claims_bot_or_api_token_cannot_impersonate_a_human_decider() {
        use magician::magician_v2::auth::BearerKind;
        let dir = tempfile::tempdir().unwrap();
        let api = web::Data::new(TranscriptClaimsApi::new(ArtifactV2Workspace::new(
            dir.path(),
        )));
        for (bearer, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (
                Some(BearerKind::Bot {
                    bot_name: "telegram".into(),
                }),
                StatusCode::FORBIDDEN,
            ),
            (Some(BearerKind::ApiToken), StatusCode::FORBIDDEN),
            (
                Some(BearerKind::Grant {
                    workspace: "work".into(),
                }),
                StatusCode::FORBIDDEN,
            ),
        ] {
            assert_eq!(
                pending_confirmation_handler(
                    decision_caller(bearer.clone()),
                    web::Path::from("claim".to_owned()), api.clone(),
                ).await.status(),
                expected,
            );
            for reject in [false, true] {
                let body = web::Json(DecideClaimRequest {
                    workspace: None,
                    by: "Alice".into(),
                    note: None,
                    expected_revision: Some(1),
                    decision_id: Some("attempt-human-impersonation".into()),
                });
                let req = decision_caller(bearer.clone());
                let path = web::Path::from("claim".to_owned());
                let response = if reject {
                    reject_claim_handler(req, path, body, api.clone()).await
                } else {
                    confirm_claim_handler(req, path, body, api.clone()).await
                };
                assert_eq!(response.status(), expected);
            }
            let response = confirm_commitment_handler(
                decision_caller(bearer),
                web::Path::from("term".to_owned()),
                web::Json(ConfirmCommitmentRequest {
                    workspace: None,
                    audience_kind: "person".into(),
                    audience_id: "bob".into(),
                    by: "Alice".into(),
                    expected_revision: 1,
                    decision_id: "attempt-human-impersonation".into(),
                }),
                api.clone(),
            )
            .await;
            assert_eq!(response.status(), expected);
        }
    }

    #[actix_web::test]
    async fn envoy_claims_owner_session_can_decide_only_in_its_bound_workspace() {
        use magician::magician_v2::auth::{AuthMethod, BearerKind};
        let dir = tempfile::tempdir().unwrap();
        let api = web::Data::new(TranscriptClaimsApi::new(ArtifactV2Workspace::new(
            dir.path(),
        )));
        let ingest: IngestTranscriptRequest = serde_json::from_value(json!({
            "transcript_key":"owner-observed","effective_speaker":"Alice","attendees":["Bob"],
            "extracted_by":"manual-import","occurred_at":"2026-09-27T10:00:00Z",
            "utterances":[{"segment_key":"one","attribution":"ours","speaker_id":"Alice","spoken_text":"Exact words"}]
        })).unwrap();
        assert_eq!(
            ingest_transcript_handler(request("alice"), web::Json(ingest), api.clone())
                .await
                .status(),
            StatusCode::OK
        );
        let claim = api
            .ingestion()
            .pending_claims(&OutwardScope::new("alice", "work"))
            .unwrap()
            .remove(0);
        let recovery = pending_confirmation_handler(
            decision_caller(Some(BearerKind::Session(AuthMethod::Password))),
            web::Path::from(claim.claim_id.clone()), api.clone(),
        ).await;
        assert_eq!(recovery.status(), StatusCode::OK);
        let recovery: serde_json::Value = serde_json::from_slice(&to_bytes(recovery.into_body()).await.unwrap()).unwrap();
        assert_eq!(recovery, json!({ "confirmation": null }));
        for (workspace, expected) in [
            (Some("other".into()), StatusCode::FORBIDDEN),
            (None, StatusCode::OK),
        ] {
            let response = confirm_claim_handler(
                decision_caller(Some(BearerKind::Session(AuthMethod::Password))),
                web::Path::from(claim.claim_id.clone()),
                web::Json(DecideClaimRequest {
                    workspace,
                    by: "Alice".into(),
                    note: None,
                    expected_revision: Some(1),
                    decision_id: Some("owner-decision".into()),
                }),
                api.clone(),
            )
            .await;
            assert_eq!(response.status(), expected);
        }
        assert_eq!(
            api.ingestion()
                .claim(&OutwardScope::new("alice", "work"), &claim.claim_id)
                .unwrap()
                .unwrap()
                .status,
            TranscriptClaimStatus::Confirmed
        );
    }
}
