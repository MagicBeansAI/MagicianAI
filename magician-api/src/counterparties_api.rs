//! Owner-facing HTTP surface for the counterparty register.
//!
//! `magician::magician_v2::counterparties` shipped a register that could be
//! **read** and never **written**: the data-room reader asks it who a room
//! admits, `GET /engagements/{id}/audience` asks it what a label covers, and
//! nothing in this workspace could record an organisation, file an address, or
//! make the one owner decision that turns an address into a reachable one. Every
//! one of those reads is correct given an empty register — which is exactly why
//! nothing looked broken. These routes are the missing acts:
//!
//! - `POST /counterparties` — **record an organisation**;
//! - `GET  /counterparties` — the book, with counts;
//! - `GET  /counterparties/resolve` — *whose address is this?*, and separately
//!   whether anybody proved it;
//! - `GET  /counterparties/candidates` — organisations a domain **might**
//!   belong to, for owner review and never for resolution;
//! - `GET  /counterparties/{counterparty_id}` — one organisation and every
//!   address on file for it;
//! - `GET  /counterparties/{counterparty_id}/audience` — who it may be reached
//!   at, as an audience of whatever kind the caller is working in;
//! - `POST /counterparties/{counterparty_id}/identities` — **file an address**;
//! - `POST /counterparties/{counterparty_id}/identities/{identity_id}/promote`
//!   — the owner decision that proves one.
//!
//! # The decider is the boundary's, never the body's
//!
//! `recorded_by`, `created_by` and — the one that matters —
//! `decided_by` are taken from the [`VerifiedRequestIdentity`] the outer
//! boundary attached, and **cannot** be sent in a request body. Every write body
//! here is `deny_unknown_fields`, so a caller that tries to name its own decider
//! gets a refusal rather than a silently ignored field it believes was honoured.
//!
//! That is not decoration. The register refuses to let a
//! [`MintSource::ResearchInferred`] address be promoted by the same actor that
//! recorded it — inferred-and-unverified is the default state of everything
//! research produces, and a path that promoted its own output is how one
//! counterparty's authority reaches another with nobody deciding. That refusal
//! compares `decided_by` against `recorded_by`, so a route that let a caller
//! choose either string would defeat it completely while looking like it
//! enforced it. There is no override, no "trusted service" flag, and no second
//! promotion path.
//!
//! # Promotion needs a signal a server stands behind
//!
//! `request_authenticated` is the presence of the boundary's identity, not a
//! field. The promotion's `channel` is the transport that carried the *proof* —
//! a confirmed click, a delivery receipt, a signed callback — and
//! [`channel_is_verified`](magician::magician_v2::chat::envoy::channel_is_verified)
//! decides whether that transport establishes anything. A channel nobody has
//! classified fails closed, and an unauthenticated request cannot promote at all.
//!
//! # Unreadable is never empty
//!
//! A register that is not installed, or a log that cannot be read, answers
//! `503`. It never answers an empty list, because "we hold nothing for this
//! organisation" and "we could not look" produce the same empty JSON array and
//! mean opposite things — and only the first is something an owner acts on.
//!
//! # Generic
//!
//! A counterparty is a supplier, a customer, a candidate's employer, a
//! regulator, a school, or the other side of a support case. Nothing in these
//! routes or their payloads names a programme: the stage is an open string the
//! owner chooses, the audience kind is a parameter, and the identity kinds are
//! address shapes rather than relationships.

use actix_web::{http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approval_envelopes_api::{guard_derivation_component, guard_path_id};
use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::audience::{Audience, AudienceKind};
use magician::magician_v2::cloudflare_access::VerifiedRequestIdentity;
use magician::magician_v2::counterparties::{
    counterparty_id_for, global_counterparty_store, identity_id_for, AddIdentity, Counterparty,
    CounterpartyScope, CounterpartyStore, CounterpartySummary, CreateCounterparty, Identity,
    IdentityKind, MintSource, Promotion, Stage, TrustedSignal, Verification,
};

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// `POST /counterparties`
///
/// `created_by` is deliberately absent: it is the boundary's verified actor.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordCounterpartyRequest {
    /// The organisation's name as the owner writes it. Feeds the derived id
    /// through the register's own name fold, so two spellings of one name
    /// resume on one row.
    pub display_name: String,
    /// A hint for candidate review. **Never a resolution key** — see
    /// `candidates_by_domain`.
    #[serde(default)]
    pub domain: Option<String>,
    /// The stage it starts in, as an open label. Initial only: moving it is a
    /// different act, so a replay carrying a different stage is a refusal
    /// rather than a silent no-op.
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `POST /counterparties/{counterparty_id}/identities`
///
/// `recorded_by` is deliberately absent: it is the boundary's verified actor,
/// and it is half of the pair the register compares to refuse a recorder
/// approving its own row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddIdentityRequest {
    /// `email`, `phone`, `handle` or `domain`. A handle must be written
    /// `platform:handle`; the same name on two platforms is two organisations.
    pub kind: String,
    pub value: String,
    /// **How we learned it**: `owner_stated`, `observed_on_inbound`,
    /// `research_inferred` or `introduced`. None of them implies verification.
    pub source: String,
    /// What to read to check the claim. Required: provenance with no evidence
    /// is a rumour with a category label.
    pub evidence_ref: String,
    /// Required when the source is `introduced`, refused otherwise.
    #[serde(default)]
    pub introduced_by: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `POST /counterparties/{counterparty_id}/identities/{identity_id}/promote`
///
/// `decided_by` is deliberately absent, and `deny_unknown_fields` makes sending
/// one a refusal. See the module note: a caller-chosen decider defeats the one
/// check that stops research approving its own guesses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromoteIdentityRequest {
    /// The transport that carried the **proof** — the confirmed click, the
    /// delivery receipt, the signed callback. Not the channel the address is
    /// used on. A transport that does not establish who sent a message (SMTP,
    /// and anything nobody has classified) is refused.
    pub channel: String,
    /// What the owner looked at. For a `research_inferred` address this must
    /// differ from the evidence that minted it: a guess cannot be its own proof.
    pub evidence_ref: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Scope for the read routes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `GET /counterparties/resolve`
#[derive(Debug, Clone, Deserialize)]
pub struct ResolveQuery {
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `GET /counterparties/candidates`
#[derive(Debug, Clone, Deserialize)]
pub struct CandidatesQuery {
    pub domain: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `GET /counterparties/{counterparty_id}/audience`
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AudienceQuery {
    /// `engagement`, `program`, `account`, `panel` or `person`. Defaults to
    /// `account`: a counterparty on its own surface is an account, and an
    /// unrecognised word is refused rather than defaulted, because filing one
    /// relationship's roster under another's key is how two relationships with
    /// one organisation come to share an audience.
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

fn bad_request(message: impl Into<String>) -> HttpResponse {
    api_error_response(
        StatusCode::BAD_REQUEST,
        "counterparty_request_invalid",
        message,
        None,
    )
}

fn not_found(counterparty_id: &str) -> HttpResponse {
    api_error_response(
        StatusCode::NOT_FOUND,
        "counterparty_not_found",
        format!("no counterparty `{counterparty_id}` in this scope's register"),
        None,
    )
}

/// The register answered with an error, or there is none installed.
///
/// **Never folded into an empty answer.** "The log could not be read" and "this
/// organisation has no addresses" produce the same empty array and mean opposite
/// things; the second is a fact about a counterparty and the first is a broken
/// process reporting a healthy one.
fn register_unreadable(error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "counterparty_register_unreadable",
        "the counterparty register could not be read, so this answer is unknown — and unknown is \
         never `nobody`",
        Some(json!({ "detail": error.to_string() })),
    )
}

fn resolve_register() -> Result<std::sync::Arc<CounterpartyStore>, HttpResponse> {
    global_counterparty_store().ok_or_else(|| {
        api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "counterparty_register_unavailable",
            "no counterparty register is installed in this process, so who we know and how we \
             reach them cannot be read or written — and unreadable is never `nobody`",
            None,
        )
    })
}

/// The actor the outer boundary proved, for the fields that decide things.
///
/// Read from [`VerifiedRequestIdentity`], which the middleware inserts only
/// after Cloudflare Access, a paired device, or a real loopback peer has been
/// verified, and which is deliberately not deserializable from a payload. An
/// unproved request cannot record, file or promote anything here — and for
/// promotion in particular this is also the `request_authenticated` leg of the
/// trusted signal, so an unproved request could not produce one anyway.
fn verified_actor(req: &HttpRequest) -> Result<String, HttpResponse> {
    let actor = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .map(|identity| identity.principal().to_string());
    actor.ok_or_else(|| {
        api_error_response(
            StatusCode::UNAUTHORIZED,
            "counterparty_actor_unproved",
            "this request was not proved by the outer boundary, so there is nobody to record as \
             having done it. Every write to the register names its actor, and an unnamed write is \
             one nobody can audit afterwards",
            None,
        )
    })
}

/// Every caller string that feeds a derived id, guarded at the only door one
/// comes through.
fn guard_scope(principal: &str, workspace: &str) -> Result<(), HttpResponse> {
    for (label, value) in [("the principal", principal), ("the workspace", workspace)] {
        if let Err(message) = guard_derivation_component(label, value) {
            return Err(bad_request(message));
        }
    }
    Ok(())
}

/// Counts, never rates. "3 of 5 addresses proved" is a fact an owner acts on;
/// "identity confidence 0.6" is a number compared against a threshold nobody
/// chose and then read as certainty.
fn counterparty_row(counterparty: &Counterparty, summary: &CounterpartySummary) -> Value {
    json!({
        "counterparty_id": counterparty.counterparty_id,
        "display_name": counterparty.display_name,
        "domain": counterparty.domain,
        "stage": counterparty.stage.as_ref().map(Stage::as_str),
        "created_at": counterparty.created_at.to_rfc3339(),
        "created_by": counterparty.created_by,
        "merged_into": counterparty.merged_into,
        "identity_count": summary.identity_count,
        "verified_identity_count": summary.verified_identity_count,
        "inferred_identity_count": summary.inferred_identity_count,
        "merged_in_count": summary.merged_in_count,
    })
}

/// One address, with the two facts an owner reads before trusting it kept
/// apart: **how we learned it** and **whether anybody proved it**.
fn identity_row(identity: &Identity) -> Value {
    let (verified_channel, verified_evidence_ref, decided_by, verified_at) =
        match &identity.verification {
            Verification::Unverified => (None, None, None, None),
            Verification::Verified {
                channel,
                evidence_ref,
                decided_by,
                at,
            } => (
                Some(channel.clone()),
                Some(evidence_ref.clone()),
                Some(decided_by.clone()),
                Some(at.to_rfc3339()),
            ),
        };
    json!({
        "identity_id": identity.identity_id,
        "counterparty_id": identity.counterparty_id,
        "kind": identity.kind.as_str(),
        "value": identity.value,
        "normalised": identity.normalised,
        "source": identity.minted_by.source.as_str(),
        "evidence_ref": identity.minted_by.evidence_ref,
        "recorded_by": identity.minted_by.recorded_by,
        "minted_at": identity.minted_by.at.to_rfc3339(),
        "introduced_by": identity.introduced_by,
        "is_inferred": identity.is_inferred(),
        "is_verified": identity.is_verified(),
        "verified_channel": verified_channel,
        "verified_evidence_ref": verified_evidence_ref,
        "decided_by": decided_by,
        "verified_at": verified_at,
    })
}

fn audience_row(audience: &Audience) -> Value {
    json!({
        "kind": audience.reference.kind.as_str(),
        "key": audience.reference.as_key(),
        "id": audience.reference.id,
        "size": audience.size(),
        "identities": audience.identities,
    })
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// `POST /api/magician/v2/counterparties`
///
/// Record an organisation. The write path the register shipped without: until
/// this route, `CounterpartyStore::record_counterparty` had no caller outside
/// its own tests, so every audience read in this workspace was answering
/// truthfully about a book with nothing in it.
///
/// Idempotent on the derived id, which comes from the display name's comparison
/// key — so recording the same organisation twice, or twice from a caller that
/// never saw our first response, is one row and answers `200`. A **new** row
/// answers `201`.
///
/// A replay carrying a *different* domain or stage is a `409`, never a silent
/// no-op: the caller would otherwise be left believing the register holds what
/// it just sent while the register holds something else.
pub async fn record_counterparty_handler(
    req: HttpRequest,
    body: web::Json<RecordCounterpartyRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let created_by = match verified_actor(&req) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if let Err(message) = guard_derivation_component("a display name", &body.display_name) {
        return bad_request(message);
    }
    let stage = match body.stage.as_deref() {
        Some(label) => match Stage::new(label) {
            Ok(stage) => Some(stage),
            Err(error) => return bad_request(error.to_string()),
        },
        None => None,
    };

    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let counterparty_id = match counterparty_id_for(&scope, &body.display_name) {
        Ok(id) => id,
        Err(error) => return bad_request(error.to_string()),
    };
    // Read before write, so the answer can say whether this call is the one
    // that recorded the organisation. A surface that always answered `201`
    // would make a retry look like a second organisation.
    let existing = match register.load(&scope, &counterparty_id) {
        Ok(existing) => existing,
        Err(error) => return register_unreadable(&error),
    };

    let request = CreateCounterparty {
        display_name: body.display_name.clone(),
        domain: body.domain.clone(),
        stage,
        created_by,
    };
    match register.record_counterparty(&scope, &request, Utc::now()) {
        Ok(counterparty) => {
            let summary = match register.summary(&scope, &counterparty.counterparty_id) {
                Ok(summary) => summary,
                Err(error) => return register_unreadable(&error),
            };
            let payload = json!({
                "principal": principal,
                "workspace": workspace,
                "as_of_ms": Utc::now().timestamp_millis(),
                "resumed": existing.is_some(),
                "counterparty": counterparty_row(&counterparty, &summary),
            });
            if existing.is_some() {
                HttpResponse::Ok().json(payload)
            } else {
                HttpResponse::Created().json(payload)
            }
        },
        // The row was already there and the store refused: a replay that
        // changed the domain or the stage, or a name that has since been merged
        // away. Both are decisions with their own acts, and both are conflicts
        // rather than validation errors.
        Err(error) if existing.is_some() => api_error_response(
            StatusCode::CONFLICT,
            "counterparty_already_recorded_differently",
            "this organisation is already on file on different terms; nothing was written, \
             because resuming quietly would tell you the register holds what you sent when it \
             holds something else",
            Some(json!({ "detail": error.to_string() })),
        ),
        Err(error) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "counterparty_write_failed",
            "the register could not be written, so nothing was recorded — treat this organisation \
             as unknown until it succeeds",
            Some(json!({ "detail": error.to_string() })),
        ),
    }
}

/// `POST /api/magician/v2/counterparties/{counterparty_id}/identities`
///
/// File an address under an organisation, with the provenance it actually has.
///
/// **Nothing here verifies anything.** Whatever the source says, the address is
/// recorded [`Verification::Unverified`]; `MintSource::implies_verified` is
/// `false` for every variant and exists so that adding one makes somebody answer
/// the question. Proving an address is a separate, owner-made act — the promote
/// route below.
///
/// `recorded_by` is the boundary's verified actor. Filing an address as
/// `research_inferred` therefore records *this actor* as the guesser, and the
/// register will refuse to let that same actor promote it later. That is the
/// intended shape, not a dead end: a guess is promoted by somebody other than
/// whoever guessed, against evidence the guess did not author.
///
/// An identical replay answers `200` with the row already written; a replay that
/// changes the provenance, the evidence or the introducer is a `409`, because
/// those are what an owner reads before trusting an address.
pub async fn add_identity_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddIdentityRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let recorded_by = match verified_actor(&req) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let counterparty_id = path.into_inner();
    if let Err(message) = guard_path_id("a counterparty id", &counterparty_id) {
        return bad_request(message);
    }
    let Some(kind) = IdentityKind::parse(&body.kind) else {
        return bad_request(format!(
            "`{}` is not an address kind. A near miss is refused rather than defaulted: the kind \
             is part of the derived id, so filing an address under the wrong one hides it from \
             every lookup that uses the right one",
            body.kind
        ));
    };
    let Some(source) = parse_mint_source(&body.source) else {
        return bad_request(format!(
            "`{}` is not a provenance. It must be one of `owner_stated`, `observed_on_inbound`, \
             `research_inferred` or `introduced` — how we learned an address is what an owner \
             reads before trusting it, so it is never defaulted",
            body.source
        ));
    };
    if let Err(message) = guard_derivation_component("an address value", &body.value) {
        return bad_request(message);
    }

    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);

    // Index before row: an organisation is recorded before anything is filed
    // under it, so an unknown id is a `404` rather than a row created on the way
    // past.
    match register.load(&scope, &counterparty_id) {
        Ok(Some(counterparty)) if counterparty.merged_into.is_some() => {
            return api_error_response(
                StatusCode::CONFLICT,
                "counterparty_merged",
                "this organisation was folded into another and a merged record never resurrects; \
                 file the address under the record that survived",
                Some(json!({ "merged_into": counterparty.merged_into })),
            );
        },
        Ok(Some(_)) => {},
        Ok(None) => return not_found(&counterparty_id),
        Err(error) => return register_unreadable(&error),
    }

    let identity_id = match identity_id_for(&scope, kind, &body.value) {
        Ok(identity_id) => identity_id,
        Err(error) => return bad_request(error.to_string()),
    };
    let existing = match register.load_identity(&scope, &identity_id) {
        Ok(existing) => existing,
        Err(error) => return register_unreadable(&error),
    };

    let request = AddIdentity {
        counterparty_id: counterparty_id.clone(),
        kind,
        value: body.value.clone(),
        source,
        evidence_ref: body.evidence_ref.clone(),
        recorded_by,
        introduced_by: body.introduced_by.clone(),
    };
    match register.add_identity(&scope, &request, Utc::now()) {
        Ok(identity) => {
            let payload = json!({
                "principal": principal,
                "workspace": workspace,
                "as_of_ms": Utc::now().timestamp_millis(),
                "resumed": existing.is_some(),
                "identity": identity_row(&identity),
            });
            if existing.is_some() {
                HttpResponse::Ok().json(payload)
            } else {
                HttpResponse::Created().json(payload)
            }
        },
        // Either this address is already filed under a different organisation —
        // one address belongs to one organisation, and a second row would make
        // resolution ambiguous — or the provenance changed on a replay.
        Err(error) if existing.is_some() => api_error_response(
            StatusCode::CONFLICT,
            "identity_already_filed_differently",
            "this address is already on file on different terms; nothing was written, because a \
             second row for one address makes resolution ambiguous and an ambiguous resolution \
             hands one counterparty's authority to another",
            Some(json!({ "detail": error.to_string() })),
        ),
        Err(error) => api_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "identity_refused",
            "the register refused this address; nothing was written",
            Some(json!({ "detail": error.to_string() })),
        ),
    }
}

/// `POST /api/magician/v2/counterparties/{counterparty_id}/identities/{identity_id}/promote`
///
/// **The one owner decision this module has**: prove that an address reaches
/// whom we think it reaches.
///
/// Four things must hold, and this route supplies none of them from the body:
///
/// 1. **A named decider** — the boundary's verified actor, never a field.
/// 2. **A server-trusted signal** — `request_authenticated` is the presence of
///    that identity, and whether the named channel's transport establishes a
///    sender is decided by `channel_is_verified`, which fails closed on a
///    channel nobody has classified.
/// 3. **A guess may not be its own proof** — for a `research_inferred` address
///    the evidence must differ from the evidence that minted it.
/// 4. **And the guesser may not approve itself** — for that same address the
///    decider must differ from the recorder. Because the decider comes from the
///    boundary and the recorder was written the same way, there is no string a
///    caller can send that gets around this.
///
/// The first promotion is the promotion: an identical replay resumes, and a
/// promotion with different evidence, channel or decider is refused rather than
/// overwriting the one decision an owner can audit.
pub async fn promote_identity_handler(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<PromoteIdentityRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    // Both legs of the same fact: who decided, and that the outer boundary
    // proved the request. Neither is a body field, and neither has an override.
    let decided_by = match verified_actor(&req) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let (counterparty_id, identity_id) = path.into_inner();
    for (label, value) in [
        ("a counterparty id", counterparty_id.as_str()),
        ("an identity id", identity_id.as_str()),
    ] {
        if let Err(message) = guard_path_id(label, value) {
            return bad_request(message);
        }
    }
    if let Err(message) = guard_derivation_component("a promotion channel", &body.channel) {
        return bad_request(message);
    }

    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let existing = match register.load_identity(&scope, &identity_id) {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            return api_error_response(
                StatusCode::NOT_FOUND,
                "identity_not_found",
                format!("no address `{identity_id}` in this scope's register"),
                None,
            );
        },
        Err(error) => return register_unreadable(&error),
    };
    // The address must belong to the organisation the path names. Promoting
    // through another organisation's URL would put an owner's decision on a row
    // they were not looking at.
    if existing.counterparty_id != counterparty_id {
        return api_error_response(
            StatusCode::NOT_FOUND,
            "identity_not_under_this_counterparty",
            "this address is not filed under that organisation, so promoting it here would \
             record a decision about a row the caller was not looking at",
            None,
        );
    }

    let promotion = Promotion {
        decided_by,
        evidence_ref: body.evidence_ref.clone(),
        // `request_authenticated: true` is the presence of the boundary's
        // identity, established above. `caller_claim` is left `None` on
        // purpose: a claim may only de-escalate and never raise, and an
        // owner-facing surface has nothing to de-escalate that the boundary has
        // not already decided.
        signal: TrustedSignal::new(body.channel.trim(), body.evidence_ref.trim())
            .authenticated(true),
    };
    match register.promote_identity(&scope, &identity_id, &promotion, Utc::now()) {
        Ok(identity) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of_ms": Utc::now().timestamp_millis(),
            "identity": identity_row(&identity),
        })),
        // Every refusal here is one of the four checks above, and each one is a
        // sentence an owner can act on. Nothing was written.
        Err(error) => api_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "promotion_refused",
            "the register refused this promotion; the address is still unproved, which is the \
             recoverable direction",
            Some(json!({ "detail": error.to_string() })),
        ),
    }
}

fn parse_mint_source(label: &str) -> Option<MintSource> {
    match label.trim().to_ascii_lowercase().as_str() {
        "owner_stated" => Some(MintSource::OwnerStated),
        "observed_on_inbound" => Some(MintSource::ObservedOnInbound),
        "research_inferred" => Some(MintSource::ResearchInferred),
        "introduced" => Some(MintSource::Introduced),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// `GET /api/magician/v2/counterparties`
///
/// The book: every organisation still on its own row, oldest first, each with
/// counts. Records folded away by a merge are names rather than organisations
/// and are omitted, so the list does not double-count.
pub async fn list_counterparties_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let counterparties = match register.list(&scope) {
        Ok(counterparties) => counterparties,
        Err(error) => return register_unreadable(&error),
    };
    let mut rows = Vec::with_capacity(counterparties.len());
    for counterparty in &counterparties {
        let summary = match register.summary(&scope, &counterparty.counterparty_id) {
            Ok(summary) => summary,
            Err(error) => return register_unreadable(&error),
        };
        rows.push(counterparty_row(counterparty, &summary));
    }
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": Utc::now().timestamp_millis(),
        "counterparties": rows,
    }))
}

/// `GET /api/magician/v2/counterparties/resolve`
///
/// **Whose address is this?** Exact match on the normalised value and nothing
/// else — no fuzzy match, no edit distance, no per-provider table, no `www.`
/// stripping, no public-suffix logic, no country-code inference. A near miss
/// answers `on_file: false`.
///
/// # Two answers, kept apart on purpose
///
/// `on_file` says the register holds this address. `proved` says an owner has
/// decided it reaches that organisation. **Only `proved` may grant anything.**
/// They are separate fields rather than one flag beside a counterparty so a
/// caller cannot read the first as the second — an address minted from research
/// is a guess that is very much on file.
pub async fn resolve_identity_handler(
    req: HttpRequest,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let Some(kind) = IdentityKind::parse(&query.kind) else {
        return bad_request(format!(
            "`{}` is not an address kind; a near miss is refused rather than defaulted",
            query.kind
        ));
    };
    if let Err(message) = guard_derivation_component("an address value", &query.value) {
        return bad_request(message);
    }
    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let identity_id = match identity_id_for(&scope, kind, &query.value) {
        Ok(identity_id) => identity_id,
        Err(error) => return bad_request(error.to_string()),
    };
    let identity = match register.load_identity(&scope, &identity_id) {
        Ok(identity) => identity,
        Err(error) => return register_unreadable(&error),
    };
    // `resolve` follows merge edges, so an address filed under a name that has
    // since been folded into another answers with the record that survived.
    let resolved = match register.resolve(&scope, kind, &query.value) {
        Ok(resolved) => resolved,
        Err(error) => return register_unreadable(&error),
    };
    let counterparty = match &resolved {
        Some(reference) => match register.summary(&scope, reference.as_str()) {
            Ok(summary) => Some(json!({
                "counterparty_id": summary.counterparty_id,
                "display_name": summary.display_name,
                "stage": summary.stage.as_ref().map(Stage::as_str),
                "identity_count": summary.identity_count,
                "verified_identity_count": summary.verified_identity_count,
                "inferred_identity_count": summary.inferred_identity_count,
            })),
            Err(error) => return register_unreadable(&error),
        },
        None => None,
    };

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": Utc::now().timestamp_millis(),
        "kind": kind.as_str(),
        "on_file": resolved.is_some(),
        "proved": identity.as_ref().is_some_and(Identity::is_verified),
        "counterparty": counterparty,
        "identity": identity.as_ref().map(identity_row),
    }))
}

/// `GET /api/magician/v2/counterparties/candidates`
///
/// Organisations a domain **might** belong to — **for owner review only.**
///
/// This is not a resolution and must never be used as one. A domain match is
/// affiliation, not authorisation: holding a mailbox at a large company proves
/// somebody works there and nothing more. The job of this read is to make an
/// owner's decision cheap, not to make the decision.
///
/// So: a **single** candidate is still a candidate, and an **empty** list means
/// "nothing to review" — never "cleared", never "resolved", and never
/// permission. The array is named `review_candidates` so a caller cannot mistake
/// it for an answer, and the resolution answer lives on its own route.
pub async fn domain_candidates_handler(
    req: HttpRequest,
    query: web::Query<CandidatesQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    if let Err(message) = guard_derivation_component("a domain", &query.domain) {
        return bad_request(message);
    }
    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let candidates = match register.candidates_by_domain(&scope, &query.domain) {
        Ok(candidates) => candidates,
        Err(error) => return register_unreadable(&error),
    };
    let rows: Vec<Value> = candidates
        .iter()
        .map(|summary| {
            json!({
                "counterparty_id": summary.counterparty_id,
                "display_name": summary.display_name,
                "stage": summary.stage.as_ref().map(Stage::as_str),
                "identity_count": summary.identity_count,
                "verified_identity_count": summary.verified_identity_count,
                "inferred_identity_count": summary.inferred_identity_count,
                "merged_in_count": summary.merged_in_count,
            })
        })
        .collect();
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": Utc::now().timestamp_millis(),
        "domain": query.domain.trim().to_lowercase(),
        "for_review_only": true,
        "review_candidates": rows,
    }))
}

/// `GET /api/magician/v2/counterparties/{counterparty_id}`
///
/// One organisation and every address on file for it, merges followed, ordered
/// stably so the page reads the same on every load. An organisation the register
/// does not hold is a `404` rather than an empty list: "we have never heard of
/// them" and "we know them and have no way to reach them" are different answers.
pub async fn get_counterparty_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let counterparty_id = path.into_inner();
    if let Err(message) = guard_path_id("a counterparty id", &counterparty_id) {
        return bad_request(message);
    }
    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    let counterparty = match register.load(&scope, &counterparty_id) {
        Ok(Some(counterparty)) => counterparty,
        Ok(None) => return not_found(&counterparty_id),
        Err(error) => return register_unreadable(&error),
    };
    let summary = match register.summary(&scope, &counterparty_id) {
        Ok(summary) => summary,
        Err(error) => return register_unreadable(&error),
    };
    let identities = match register.identities_for(&scope, &counterparty_id) {
        Ok(identities) => identities,
        Err(error) => return register_unreadable(&error),
    };
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": Utc::now().timestamp_millis(),
        "counterparty": counterparty_row(&counterparty, &summary),
        "identities": identities.iter().map(identity_row).collect::<Vec<Value>>(),
    }))
}

/// `GET /api/magician/v2/counterparties/{counterparty_id}/audience`
///
/// **Who this organisation may be reached at**, as an
/// [`Audience`] of whatever kind the caller is working in.
///
/// # Verified addresses only, and an empty audience admits nobody
///
/// An address on file is an address on file; nobody proved it reaches whom we
/// think. So the audience carries only addresses an owner has proved, and a
/// caller must never read an empty one as "no restriction applies": a membership
/// check over an empty set answers `false` for everybody, and that is the
/// correct answer.
///
/// # The kind is the caller's, and every kind is available
///
/// The same organisation is an `engagement` to a deal, an `account` to support,
/// a `panel` to a review board and a `person` to somebody's own records.
/// `AudienceRef::as_key` keeps those apart, so a second flow adopts this route by
/// passing a different kind — not by editing anything here or downstream. An
/// unrecognised kind is refused rather than defaulted; filing one relationship's
/// roster under another's key is how two relationships with one organisation
/// come to share an audience.
pub async fn counterparty_audience_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AudienceQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if let Err(response) = guard_scope(&principal, &workspace) {
        return response;
    }
    let counterparty_id = path.into_inner();
    if let Err(message) = guard_path_id("a counterparty id", &counterparty_id) {
        return bad_request(message);
    }
    let requested_kind = query
        .audience_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("account");
    let Some(audience_kind) = AudienceKind::parse(requested_kind) else {
        return bad_request(format!(
            "`{requested_kind}` is not an audience kind. A near miss is refused rather than \
             defaulted: filing one relationship's roster under another's key is how two different \
             relationships with one organisation come to share an audience"
        ));
    };
    let register = match resolve_register() {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = CounterpartyScope::new(&principal, &workspace);
    match register.load(&scope, &counterparty_id) {
        Ok(Some(_)) => {},
        Ok(None) => return not_found(&counterparty_id),
        Err(error) => return register_unreadable(&error),
    }
    let audience = match register.audience_for(&scope, &counterparty_id, audience_kind) {
        Ok(audience) => audience,
        Err(error) => return register_unreadable(&error),
    };
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": Utc::now().timestamp_millis(),
        "verified_only": true,
        "audience": audience_row(&audience),
    }))
}

/// Route registration. Mounted under `web::scope("/api/magician/v2")`.
///
/// `/counterparties/resolve` and `/counterparties/candidates` are registered
/// **before** `/counterparties/{counterparty_id}`: actix matches in registration
/// order, and a dynamic segment registered first would swallow both literals and
/// answer "no counterparty `resolve`" forever.
pub fn configure_counterparty_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/counterparties",
        web::get().to(list_counterparties_handler),
    )
    // A second `.route` on the same literal is how this crate registers a
    // second method: `ServiceConfig::route` moves the method guard onto the
    // resource, so a non-matching method falls through to the next
    // registration rather than answering 405 from the first.
    .route(
        "/counterparties",
        web::post().to(record_counterparty_handler),
    )
    .route(
        "/counterparties/resolve",
        web::get().to(resolve_identity_handler),
    )
    .route(
        "/counterparties/candidates",
        web::get().to(domain_candidates_handler),
    )
    .route(
        "/counterparties/{counterparty_id}/identities",
        web::post().to(add_identity_handler),
    )
    .route(
        "/counterparties/{counterparty_id}/identities/{identity_id}/promote",
        web::post().to(promote_identity_handler),
    )
    .route(
        "/counterparties/{counterparty_id}/audience",
        web::get().to(counterparty_audience_handler),
    )
    .route(
        "/counterparties/{counterparty_id}",
        web::get().to(get_counterparty_handler),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;

    /// Every write route refuses a request the outer boundary did not prove.
    ///
    /// Pins the hole that would make the promotion guard cosmetic: if an
    /// unproved request could write, the actor recorded against a row would be
    /// whatever the caller felt like, and the register's "the guesser may not
    /// approve its own output" check — which compares the recorder against the
    /// decider — would be comparing two strings the same caller chose.
    ///
    /// These requests carry a full scope and a valid body, so the only reason
    /// they can fail is the missing identity. `VerifiedRequestIdentity` cannot
    /// be deserialized from a payload, so a test client cannot forge one.
    #[actix_web::test]
    async fn every_write_refuses_an_unproved_request() {
        let app = actix_test::init_service(
            actix_web::App::new().configure(configure_counterparty_routes),
        )
        .await;

        let cases: Vec<(&str, Value)> = vec![
            ("/counterparties", json!({ "display_name": "Acme Ltd" })),
            (
                "/counterparties/cp-abc/identities",
                json!({
                    "kind": "email",
                    "value": "ops@acme.com",
                    "source": "owner_stated",
                    "evidence_ref": "intro-1"
                }),
            ),
            (
                "/counterparties/cp-abc/identities/idy-abc/promote",
                json!({ "channel": "web", "evidence_ref": "receipt-1" }),
            ),
        ];

        for (uri, body) in cases {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::post()
                    .uri(uri)
                    .insert_header(("X-Principal", "alpha"))
                    .insert_header(("X-Workspace", "prod"))
                    .set_json(&body)
                    .to_request(),
            )
            .await;
            assert_eq!(
                response.status().as_u16(),
                401,
                "{uri} accepted a write from a request nobody proved"
            );
        }
    }

    /// **A caller may not name its own decider, recorder or creator.**
    ///
    /// Pins the path around the store's one real control. The register refuses
    /// to let a `research_inferred` address be promoted by the actor that
    /// recorded it, and that refusal is a string comparison — so a body field
    /// called `decided_by` (or `recorded_by`, or `created_by`) would let a
    /// research agent send any two different strings and promote its own guess.
    /// `deny_unknown_fields` makes sending one a visible `400` rather than a
    /// silently dropped field the caller believes was honoured.
    #[actix_web::test]
    async fn a_body_naming_its_own_actor_is_refused_rather_than_ignored() {
        let app = actix_test::init_service(
            actix_web::App::new().configure(configure_counterparty_routes),
        )
        .await;

        let cases: Vec<(&str, Value)> = vec![
            (
                "/counterparties",
                json!({ "display_name": "Acme Ltd", "created_by": "owner" }),
            ),
            (
                "/counterparties/cp-abc/identities",
                json!({
                    "kind": "email",
                    "value": "ops@acme.com",
                    "source": "research_inferred",
                    "evidence_ref": "note-7",
                    "recorded_by": "somebody-else"
                }),
            ),
            (
                "/counterparties/cp-abc/identities/idy-abc/promote",
                json!({
                    "channel": "web",
                    "evidence_ref": "receipt-1",
                    "decided_by": "owner"
                }),
            ),
        ];

        for (uri, body) in cases {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::post()
                    .uri(uri)
                    .insert_header(("X-Principal", "alpha"))
                    .insert_header(("X-Workspace", "prod"))
                    .set_json(&body)
                    .to_request(),
            )
            .await;
            assert_eq!(
                response.status().as_u16(),
                400,
                "{uri} accepted a caller-named actor instead of refusing it"
            );
        }
    }

    /// A register nobody installed answers `503`, never an empty book.
    ///
    /// Pins the failure that reads as a healthy system: an empty
    /// `counterparties: []` from a process with no register looks exactly like a
    /// scope with nobody in it, and an owner would conclude their book was empty
    /// rather than that the binary never installed one.
    #[actix_web::test]
    async fn the_reads_refuse_rather_than_answer_an_empty_book() {
        assert!(
            global_counterparty_store().is_none(),
            "this test asserts the uninstalled state; something installed a register"
        );
        let app = actix_test::init_service(
            actix_web::App::new().configure(configure_counterparty_routes),
        )
        .await;

        for uri in [
            "/counterparties",
            "/counterparties/resolve?kind=email&value=ops@acme.com",
            "/counterparties/candidates?domain=acme.com",
            "/counterparties/cp-abc",
            "/counterparties/cp-abc/audience?audience_kind=account",
        ] {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::get()
                    .uri(uri)
                    .insert_header(("X-Principal", "alpha"))
                    .insert_header(("X-Workspace", "prod"))
                    .to_request(),
            )
            .await;
            assert_eq!(
                response.status().as_u16(),
                503,
                "{uri} answered as though the register were readable and empty"
            );
        }
    }

    /// The scope is required before anything else is considered.
    ///
    /// Pins a surface that fell back to a default scope: one owner's book would
    /// be served to another, and every answer would look correct.
    #[actix_web::test]
    async fn a_request_with_no_scope_is_refused() {
        let app = actix_test::init_service(
            actix_web::App::new().configure(configure_counterparty_routes),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/counterparties")
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 400);
    }

    /// Address kinds and provenances are parsed exactly, and a near miss is a
    /// refusal rather than a default.
    ///
    /// Pins the silent misfile: the kind is part of every derived id, so an
    /// address filed under a defaulted kind is invisible to every lookup that
    /// uses the right one — and a defaulted provenance would record a guess as
    /// an owner statement.
    #[test]
    fn a_near_miss_kind_or_provenance_is_refused() {
        assert_eq!(IdentityKind::parse("Email"), Some(IdentityKind::Email));
        assert_eq!(IdentityKind::parse("  phone "), Some(IdentityKind::Phone));
        assert_eq!(IdentityKind::parse("handle"), Some(IdentityKind::Handle));
        assert_eq!(IdentityKind::parse("domain"), Some(IdentityKind::Domain));
        for near_miss in ["emails", "e-mail", "mail", "phone_number", "", "kind"] {
            assert_eq!(
                IdentityKind::parse(near_miss),
                None,
                "`{near_miss}` was accepted as an address kind"
            );
        }

        assert_eq!(
            parse_mint_source("owner_stated"),
            Some(MintSource::OwnerStated)
        );
        assert_eq!(
            parse_mint_source("RESEARCH_INFERRED"),
            Some(MintSource::ResearchInferred)
        );
        for near_miss in ["owner", "inferred", "observed", "", "research"] {
            assert!(
                parse_mint_source(near_miss).is_none(),
                "`{near_miss}` was accepted as a provenance"
            );
        }
        // The provenance that must never imply proof, whichever way it is
        // spelled on the wire.
        for source in [
            MintSource::OwnerStated,
            MintSource::ObservedOnInbound,
            MintSource::ResearchInferred,
            MintSource::Introduced,
        ] {
            assert!(
                !source.implies_verified(),
                "`{}` implied verification",
                source.as_str()
            );
        }
    }
}
