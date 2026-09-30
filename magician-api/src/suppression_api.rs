//! Owner-facing HTTP surface for the suppression register.
//!
//! `magician::magician_v2::suppression` shipped a complete register — record,
//! time-box, batch-ingest, lift, report — and one reader:
//! `outward_gate::contact_refusal`, which screens every outward recipient
//! before a send leaves. It had **no writer**. `suppress`, `suppress_until`,
//! `ingest` and `lift` had no non-test caller and there was no route, so the
//! store was empty on disk and the screen cleared everybody. A fail-closed
//! guard rendered vacuous by an empty store is not a guard; it is a guard
//! shaped hole, and it would have kept clearing people after they bounced and
//! after they opted out.
//!
//! Four acts close it:
//!
//! - `GET  /suppressions` — every entry on the register, with per-reason counts;
//! - `GET  /suppressions/check` — **may we contact this person?**, answered
//!   through the same `screen` call the send-time gate makes;
//! - `POST /suppressions` — record one, indefinitely or time-boxed;
//! - `POST /suppressions/lift` — stop one applying, by recording a lift beside
//!   it.
//!
//! The sweep that turns provider hard bounces and complaints into entries is
//! the *other* writer and does not live here: see
//! `magician::magician_v2::delivery_hygiene::worker`.
//!
//! # There is no route around the consent rule
//!
//! `SuppressionReason::OptOut` and `SuppressionReason::Complaint` can only be
//! lifted by an explicit `LiftAuthority::OwnerAct` carrying evidence. That rule
//! lives in the module, and this surface is careful to add nothing that softens
//! it:
//!
//! - `authority` is a **required** field with no default and no inference. A
//!   route that supplied `OwnerAct` on the caller's behalf — or that upgraded
//!   an `Operational` request because the reason demanded it — would be exactly
//!   the loophole, wearing the module's own vocabulary.
//! - There is no `force`, no `override`, and no DELETE. Nothing here removes a
//!   row; a lift is a new record beside the original, so the register still
//!   shows that the person was suppressed, when, on what evidence, and who
//!   reversed it.
//! - The refusal comes back as the module wrote it, so the owner reads *why*
//!   rather than a generic 400.
//!
//! # Generic
//!
//! An identity, a reason, evidence, and a clock. Nothing in this file knows
//! what is being sent, on which rail, or on whose behalf. The audience
//! parameters take an [`AudienceKind`] **by name** and an id, so an
//! account's, a programme's, a panel's or a person's register is addressable
//! through the same two query parameters — a second flow does not need this
//! file edited to exist. A kind this build does not know is refused with the
//! list of the ones it does, which is the fail-closed direction: an unnameable
//! kind must never be filed under some other kind's key.
//!
//! # Global by default, and a narrower scope may only add
//!
//! With no audience parameters the register is the global one, which is what
//! almost every caller wants: somebody who opted out of one programme has not
//! consented to another. Naming an audience narrows *writes* to that
//! audience's span — reads still consult the global register too, and a
//! per-audience lift cannot touch a global entry. That is the module's rule and
//! this surface does not restate it; it just passes the audience through.
//!
//! # Reads default to the whole register
//!
//! `GET /suppressions` covers everything ever recorded unless the caller
//! narrows it. A default window would be the comfortable choice and the wrong
//! one: a register holding a year-old opt-out would answer an empty list to an
//! owner asking who is suppressed, and an empty list is indistinguishable from
//! a clean register. The window that was actually applied comes back in the
//! response, so a narrowed read can never be mistaken for the whole thing.
//!
//! # Fail closed
//!
//! An unreadable register answers **503**, never 200 with an empty list. The
//! module's contract is that `Err` means DO NOT SEND, and a surface that
//! flattened that into "nothing found" would hand an owner the same false
//! confidence the empty store did. IO faults and caller faults are told apart
//! by the error chain — an `ArtifactV2Error` anywhere in it is the store
//! failing, and everything else is the request.
//!
//! # The other half of "what happened after we sent"
//!
//! Two further reads mount here, under `/delivery`:
//!
//! - `GET /delivery/unacknowledged` — **what did we send that nothing ever came
//!   back for**, oldest first, with counts by rail;
//! - `GET /delivery/watch` — the health snapshot of the worker that asks the
//!   same question on a cadence, and that pulls provider receipts in on the
//!   same tick: how many acts moved out of `dispatch_unknown`, and how many are
//!   still unacknowledged past the window.
//!
//! They sit in this file rather than a surface of their own, and the reason is
//! not tidiness. The register above is fed by
//! `magician::magician_v2::delivery_hygiene`, whose source is the delivery
//! ledger; these two read the *same* ledger for the acts it has never heard
//! anything about. One operator, one subject — what became of the things we
//! sent — and a new route file for two reads would mean a new `pub mod` line, a
//! new mount, and a page nobody has a habit of opening. Registering them in
//! [`configure_suppression_routes`] makes them reachable through the mount that
//! already exists.
//!
//! **Why this matters at all**: `DeliveryLedger::unreconciled` existed to answer
//! this question and had no caller anywhere in the workspace. So the failure
//! mode of a silently broken provider integration — every send succeeding,
//! nothing ever confirming — produced no signal at all, and nobody would have
//! noticed for weeks. These reads are what make the silence reportable. They do
//! **not** ingest receipts; there is still no producer, which is exactly what
//! the counts will say.
//!
//! Counts, never rates, here as in the ledger: there is no
//! `acknowledged_rate`, because "97% acknowledged" over thirty acts and over
//! thirty thousand are different facts and the first is what a broken
//! integration looks like in its first hour.
//!
//! # The door a receipt comes through
//!
//! Two more acts mount here, and they are the **producer** the reads above kept
//! reporting the absence of:
//!
//! - `POST /delivery/receipts` — record what a provider said about one act;
//! - `GET  /delivery/receipts` — everything on file for one act, and every
//!   attempt to put something there.
//!
//! They feed `magician::magician_v2::delivery::intake::ReceiptIntake::admit`,
//! which hands the receipt to `DeliveryLedger::reconcile` untouched — so every
//! invariant is the ledger's, unchanged. Terminal states never resurrect. A
//! second identity under one provider message id is refused. An identical
//! replay resumes and a changed payload under one id is an error. The severity
//! order decides, not arrival order.
//!
//! One door, deliberately. A provider bridge, a Kapso adapter, an SMTP
//! reconciler and an owner reading a bounce out of a support ticket all POST
//! the same body; `source` records which route the fact travelled and changes
//! nothing about how it is judged. A receipt is a receipt.
//!
//! # Authentication, which is the whole risk of this route
//!
//! A receipt endpoint anybody could POST to is a **remote suppression
//! primitive**. `complained` and a hard bounce each carry a suppression cause
//! that the delivery-hygiene sweep turns into a register entry only an explicit
//! owner act with evidence can lift, so one unauthenticated call could silence
//! a real recipient permanently; `delivered` is the mirror image, silencing a
//! real failure by making an act look acknowledged.
//!
//! **This codebase has no pattern for authenticating an unattended inbound
//! POST, so this route does not pretend to have one.** Every `/api/magician/v2`
//! route sits behind `cloudflare_access::verify_access_middleware`, whose only
//! bypass is the device-enrollment exchange — and that one is still gated by a
//! single-use capability in its handler. Nothing in this crate verifies a
//! signature over a request body. The one webhook receiver that exists is a
//! separate Node process (`skillshub/bots/agentmail`), it accepts unverified
//! requests when its secret is unset, and it ignores delivery events entirely.
//!
//! So this is the **owner-authenticated form only**. Both routes require the
//! `VerifiedRequestIdentity` the middleware attaches after Cloudflare Access, a
//! paired device, or a real loopback peer — the same gate `apps_api` uses — and
//! the scope is taken **from that identity**, never from the caller's headers.
//! An `X-Principal` or `X-Workspace` that disagrees with the proved identity is
//! a `403`, not a merge: a route that let a caller pick the scope would let a
//! caller pick whose recipients to suppress.
//!
//! The read is behind the same gate as the write, which is stricter than its
//! `/delivery/unacknowledged` neighbour on purpose — it names the actors who
//! recorded each receipt, and that is not a list to hand an unproved caller.
//!
//! What a provider-direct door would additionally need, stated so nobody has to
//! rediscover it: a per-provider signing key the deployment does not hold, a
//! path exemption in the access middleware, raw-body capture before JSON
//! parsing (a signature covers the exact bytes), and a replay window keyed on
//! the provider's own event id. None of the four exist today, and inventing a
//! signature scheme without them would be authentication theatre.
//!
//! # An act must have left before a receipt can speak about it
//!
//! `admit` is handed this scope's dispatch log and refuses a receipt whose act
//! is not on it. Without that check the endpoint would accept any string as an
//! act ref, and a complaint recorded against an invented act would suppress a
//! real address on the strength of a send that never happened.

use actix_web::{http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::agents::outward_gate::DispatchLog;
use magician::magician_v2::artifact_v2::service::ArtifactV2Error;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{AudienceKind, AudienceRef};
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::delivery::intake::{IntakeAttribution, ReceiptIntake, ReceiptSource};
use magician::magician_v2::delivery::{
    DeliveryLedger, DeliveryReceipt, DeliveryScope, DeliveryState, Reconciliation,
};
use magician::magician_v2::delivery_hygiene::silence::{
    attribute, rails_from_disclosures, scan_silence,
};
use magician::magician_v2::delivery_hygiene::worker::SuppressionSweepHealth;
use magician::magician_v2::evidence::{OutwardAssertionStore, OutwardScope};
use magician::magician_v2::suppression::{
    LiftAuthority, Suppression, SuppressionEvidence, SuppressionReason, SuppressionRegister,
    SuppressionScope, SuppressionSpan, SuppressionState,
};

/// A lookback beyond this covers the whole register, which is what it means.
/// Bounded so the subtraction below cannot overflow the clock.
const MAX_LOOKBACK_HOURS: i64 = 24 * 365 * 100;

/// The app-state this surface needs: somewhere to open the owner's register.
#[derive(Clone)]
pub struct SuppressionApi {
    workspace_layout: ArtifactV2Workspace,
}

impl SuppressionApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn workspace(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

/// Map a register failure onto a status, without flattening the two kinds.
///
/// An `ArtifactV2Error` in the chain means the store itself could not be read
/// or written: **503**, because "we could not check" is not a caller fault and
/// must never read like an answer. Everything else is the module refusing the
/// request — a malformed identity, blank evidence, a time-boxed consent
/// decision, an operational attempt on an opt-out — which is **400**, and the
/// module's own sentence is the body, because it says what to do instead.
fn register_failure(doing: &str, error: anyhow::Error) -> HttpResponse {
    let store_fault = error.chain().any(|cause| cause.is::<ArtifactV2Error>());
    if store_fault {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "suppression_register_unreadable",
            format!(
                "the suppression register could not be reached while {doing}, and an \
                 unreachable register is never an empty one: {error:#}"
            ),
            None,
        );
    }
    api_error_response(
        StatusCode::BAD_REQUEST,
        "suppression_refused",
        format!("{error:#}"),
        None,
    )
}

// ---------------------------------------------------------------------------
// Naming things: reasons, authorities, audiences
// ---------------------------------------------------------------------------

/// Every reason this build knows, for an error message that stays true when a
/// new one is added.
fn known_reasons() -> String {
    SuppressionReason::ALL
        .iter()
        .map(|reason| reason.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The reason a caller named, or `None`.
///
/// Read off `SuppressionReason::ALL` rather than a hand-written match, so a
/// reason added to the module is nameable here without this file being edited —
/// and, more importantly, so it cannot be *un*nameable while still being
/// storable. `None` is "that is not a reason", never a default: guessing would
/// file a complaint as an owner block, which lifts under a weaker authority.
fn reason_from(label: &str) -> Option<SuppressionReason> {
    let wanted = label.trim().to_ascii_lowercase();
    SuppressionReason::ALL
        .into_iter()
        .find(|reason| reason.as_str() == wanted.as_str())
}

/// The authority a caller named, or `None`.
///
/// **No default and no inference.** The whole consent rule rests on this: a
/// surface that supplied `owner_act` when the reason demanded one would let any
/// caller of this route reverse an opt-out, which is the failure the register
/// was built to make impossible.
fn authority_from(label: &str) -> Option<LiftAuthority> {
    match label.trim().to_ascii_lowercase().as_str() {
        "operational" => Some(LiftAuthority::Operational),
        "owner_act" => Some(LiftAuthority::OwnerAct),
        _ => None,
    }
}

/// Build the register the caller addressed.
///
/// Both audience parameters or neither. One alone is refused rather than
/// half-honoured: an `audience_kind` with no id would silently fall back to the
/// global register and write a per-audience decision into everybody's, and an
/// id with no kind cannot be filed at all because the kind is part of the key.
fn register_for(
    workspace_layout: &ArtifactV2Workspace,
    audience_kind: Option<&str>,
    audience_id: Option<&str>,
) -> Result<SuppressionRegister, HttpResponse> {
    match (audience_kind, audience_id) {
        (None, None) => Ok(SuppressionRegister::global(workspace_layout.clone())),
        (Some(kind), Some(id)) => {
            let Some(kind) = AudienceKind::parse(kind) else {
                return Err(bad_request(format!(
                    "`{kind}` is not an audience kind; this build knows {}",
                    AudienceKind::ALL
                        .iter()
                        .map(|known| known.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            };
            SuppressionRegister::scoped_to_audience(
                workspace_layout.clone(),
                AudienceRef::new(kind, id.trim()),
            )
            .map_err(|error| bad_request(format!("{error:#}")))
        },
        (Some(_), None) => Err(bad_request(
            "`audience_kind` was given without `audience_id`: a register bound to a blank id \
             would answer for every unnamed caller at once, and defaulting to the global \
             register would write a per-audience decision into everybody's",
        )),
        (None, Some(_)) => Err(bad_request(
            "`audience_id` was given without `audience_kind`: the kind is part of the key, so \
             `acme` alone cannot be told apart from an account, a programme or an engagement \
             of the same name",
        )),
    }
}

fn evidence_from(
    established_at: DateTime<Utc>,
    evidence_ref: &str,
    recorded_by: &str,
) -> SuppressionEvidence {
    SuppressionEvidence::new(established_at, evidence_ref, recorded_by)
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

fn state_label(state: SuppressionState) -> &'static str {
    match state {
        SuppressionState::InForce => "in_force",
        SuppressionState::Lifted => "lifted",
        SuppressionState::Expired => "expired",
    }
}

/// One entry, plus the state derived from the clock at read time.
///
/// The state is **not** a field on the stored row and is not stored here
/// either: it is asked of `Suppression::state` with the current instant on
/// every read, so an expiry that has arrived reads as expired without anything
/// having swept.
fn entry_json(entry: &Suppression, now: DateTime<Utc>) -> serde_json::Value {
    let mut body = serde_json::to_value(entry).unwrap_or_else(|_| json!({}));
    if let Some(map) = body.as_object_mut() {
        map.insert("state".into(), json!(state_label(entry.state(now))));
    }
    body
}

fn span_label(span: &SuppressionSpan) -> String {
    span.as_key()
}

#[derive(Debug, Deserialize)]
pub struct SuppressionScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// One of `AudienceKind`'s names. With `audience_id`, addresses that
    /// audience's register instead of the global one.
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListSuppressionsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// Narrow the report to entries recorded at or after this instant.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// The same, expressed as a lookback. Ignored when `since` is given.
    #[serde(default)]
    pub lookback_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CheckSuppressionQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// The identity to ask about. Required — there is no "check everybody"
    /// form, because a check over nobody is vacuously clear.
    pub identity: String,
}

#[derive(Debug, Deserialize)]
pub struct RecordSuppressionBody {
    pub identity: String,
    /// One of `SuppressionReason`'s names.
    pub reason: String,
    /// When the establishing act happened — the transport's bounce timestamp,
    /// the instant they clicked unsubscribe. Not when we recorded it.
    pub established_at: DateTime<Utc>,
    /// The ref an auditor follows. Never blank; the module refuses it.
    pub evidence_ref: String,
    /// Who or what recorded it.
    pub recorded_by: String,
    /// A time-boxed hold's end. Refused for the consent reasons, and refused
    /// when it is already at or behind now — both by the module.
    #[serde(default)]
    pub in_force_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LiftSuppressionBody {
    pub identity: String,
    /// One of `SuppressionReason`'s names. A lift names the reason it reverses;
    /// it does not clear an identity wholesale.
    pub reason: String,
    /// `operational` or `owner_act`. **Required.** See this module's header for
    /// why there is no default.
    pub authority: String,
    pub established_at: DateTime<Utc>,
    pub evidence_ref: String,
    pub recorded_by: String,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReasonCount {
    reason: &'static str,
    count: usize,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/magician/v2/suppressions`
///
/// Every entry, lifted and expired ones included — the record of a suppression
/// somebody reversed is the single most interesting row in a compliance
/// review, and a report that hid it would hide exactly the act being checked.
/// The per-reason counts come from the module, so a reason with nothing behind
/// it still appears as zero: "none" and "not measured" must not render alike.
pub async fn list_suppressions_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<ListSuppressionsQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let register = match register_for(
        api.workspace(),
        query.audience_kind.as_deref(),
        query.audience_id.as_deref(),
    ) {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = SuppressionScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();

    // The whole register unless the caller narrowed it. See the header: a
    // default window turns an old opt-out into an empty list.
    let since = match query.since {
        Some(at) => at,
        None => match query.lookback_hours {
            // A window wider than the register IS the register, and saying so
            // costs nothing. Subtracting it directly would overflow the clock
            // on a large enough number, and a panicking report is a report an
            // owner reads as an outage rather than as an answer.
            Some(hours) if hours > MAX_LOOKBACK_HOURS => DateTime::<Utc>::MIN_UTC,
            Some(hours) if hours > 0 => now
                .checked_sub_signed(Duration::hours(hours))
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
            // A zero or negative lookback is a caller asking for a window that
            // holds nothing. Answering the whole register instead would be a
            // guess; answering nothing would be a vacuous empty list.
            Some(hours) => {
                return bad_request(format!(
                    "`lookback_hours` must be positive; `{hours}` describes a window that can \
                     hold nothing, and an empty report is indistinguishable from a clean \
                     register"
                ))
            },
            None => DateTime::<Utc>::MIN_UTC,
        },
    };

    let entries = match register.suppressed_since(&scope, since) {
        Ok(entries) => entries,
        Err(error) => return register_failure("listing suppressions", error),
    };
    let counts = match register.counts_since(&scope, since) {
        Ok(counts) => counts,
        Err(error) => return register_failure("counting suppressions", error),
    };

    let in_force = entries
        .iter()
        .filter(|entry| entry.is_in_force(now))
        .count();
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "span": span_label(register.span()),
        "since": if since == DateTime::<Utc>::MIN_UTC {
            json!(null)
        } else {
            json!(since.to_rfc3339())
        },
        "covers_whole_register": since == DateTime::<Utc>::MIN_UTC,
        "in_force": in_force,
        "entries": entries
            .iter()
            .map(|entry| entry_json(entry, now))
            .collect::<Vec<_>>(),
        "counts": counts
            .into_iter()
            .map(|(reason, count)| ReasonCount {
                reason: reason.as_str(),
                count,
            })
            .collect::<Vec<_>>(),
    }))
}

/// `GET /api/magician/v2/suppressions/check`
///
/// The owner-facing form of the question the send-time gate asks, answered by
/// the **same call**: `SuppressionRegister::screen`, over a one-element list.
/// Deliberately not a second opinion of its own — a surface that derived the
/// answer another way could say "clear" about somebody the gate refuses, or the
/// reverse, and an owner would have no way to tell which one was lying.
///
/// The verdict is read off `sendable`, never off `blocked.is_empty()`. A screen
/// that somehow cleared nobody *and* blocked nobody answers 503 rather than
/// implying permission.
pub async fn check_suppression_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<CheckSuppressionQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let register = match register_for(
        api.workspace(),
        query.audience_kind.as_deref(),
        query.audience_id.as_deref(),
    ) {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = SuppressionScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();

    let screened = match register.screen(&scope, &[query.identity.clone()], now) {
        Ok(screened) => screened,
        Err(error) => return register_failure("checking an identity", error),
    };
    let history = match register.history(&scope, &query.identity) {
        Ok(history) => history,
        Err(error) => return register_failure("reading an identity's history", error),
    };

    // `sendable` first, and never `blocked.is_empty()`.
    if let Some(identity) = screened.sendable.first() {
        return HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "span": span_label(register.span()),
            "identity": identity,
            "sendable": true,
            "blocked_by": serde_json::Value::Null,
            "history": history
                .iter()
                .map(|entry| entry_json(entry, now))
                .collect::<Vec<_>>(),
        }));
    }
    if let Some(entry) = screened.blocked.first() {
        return HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "span": span_label(register.span()),
            "identity": entry.identity,
            "sendable": false,
            "blocked_by": entry_json(entry, now),
            "history": history
                .iter()
                .map(|entry| entry_json(entry, now))
                .collect::<Vec<_>>(),
        }));
    }
    api_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "suppression_screen_inconclusive",
        "the screen returned neither a cleared nor a blocked identity, so nothing about this \
         recipient was actually established — and `we could not check` is never permission",
        None,
    )
}

/// `POST /api/magician/v2/suppressions`
///
/// One of the two production writers (the other is the delivery-hygiene sweep).
/// Idempotent per `(identity, reason, evidence)`: replaying the same act
/// resumes the entry already written and keeps its original recording time. A
/// replay whose payload differs under the same evidence comes back as a
/// refusal, not a quiet no-op, because two accounts of one act have to be
/// reconciled by a person.
pub async fn record_suppression_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<SuppressionScopeQuery>,
    body: web::Json<RecordSuppressionBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let body = body.into_inner();
    let Some(reason) = reason_from(&body.reason) else {
        return bad_request(format!(
            "`{}` is not a suppression reason; this build knows {}",
            body.reason,
            known_reasons()
        ));
    };
    // The body may carry the audience too, so a client that POSTs a JSON
    // document does not have to also build a query string.
    let register = match register_for(
        api.workspace(),
        body.audience_kind
            .as_deref()
            .or(query.audience_kind.as_deref()),
        body.audience_id.as_deref().or(query.audience_id.as_deref()),
    ) {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = SuppressionScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();
    let evidence = evidence_from(body.established_at, &body.evidence_ref, &body.recorded_by);

    let recorded = match body.in_force_until {
        Some(until) => {
            register.suppress_until(&scope, &body.identity, reason, evidence, until, now)
        },
        None => register.suppress(&scope, &body.identity, reason, evidence, now),
    };
    match recorded {
        Ok(entry) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "span": span_label(register.span()),
            "suppression": entry_json(&entry, now),
        })),
        Err(error) => register_failure("recording a suppression", error),
    }
}

/// `POST /api/magician/v2/suppressions/lift`
///
/// A lift is a new record beside the original entry; nothing is deleted, and
/// there is no route here that deletes.
///
/// `authority` is required and passed through untouched. An `operational`
/// attempt on an opt-out or a complaint comes back as the module's own refusal,
/// naming the rule — which is the point: the owner should read *"this needs an
/// owner act with evidence"*, not a 400 they might route around.
pub async fn lift_suppression_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<SuppressionScopeQuery>,
    body: web::Json<LiftSuppressionBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let body = body.into_inner();
    let Some(reason) = reason_from(&body.reason) else {
        return bad_request(format!(
            "`{}` is not a suppression reason; this build knows {}",
            body.reason,
            known_reasons()
        ));
    };
    let Some(authority) = authority_from(&body.authority) else {
        return bad_request(format!(
            "`{}` is not a lift authority; name `operational` or `owner_act`. There is no \
             default: `opt_out` and `complaint` can only be lifted by an explicit owner act \
             with evidence, and a route that chose the authority for you would be the way \
             around that rule rather than an application of it",
            body.authority
        ));
    };
    let register = match register_for(
        api.workspace(),
        body.audience_kind
            .as_deref()
            .or(query.audience_kind.as_deref()),
        body.audience_id.as_deref().or(query.audience_id.as_deref()),
    ) {
        Ok(register) => register,
        Err(response) => return response,
    };
    let scope = SuppressionScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();
    let evidence = evidence_from(body.established_at, &body.evidence_ref, &body.recorded_by);

    match register.lift(&scope, &body.identity, reason, authority, evidence, now) {
        Ok(lifted) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "span": span_label(register.span()),
            "lifted": lifted
                .iter()
                .map(|entry| entry_json(entry, now))
                .collect::<Vec<_>>(),
        })),
        Err(error) => register_failure("lifting a suppression", error),
    }
}

// ---------------------------------------------------------------------------
// Delivery silence: what we sent that nothing came back for
// ---------------------------------------------------------------------------

/// The grace an owner gets when they name none.
///
/// Twenty-four hours. Providers acknowledge in seconds and deliver in minutes,
/// so a full day of total silence is already far past any honest in-flight
/// window. The applied value is echoed in the response, so a defaulted read can
/// never be mistaken for one the caller chose.
const DEFAULT_SILENCE_GRACE_HOURS: i64 = 24;

/// A grace beyond this is refused rather than clamped. It is a hundred years of
/// hours; asking for more is a caller error worth naming, and it keeps the
/// duration arithmetic below away from the clock's edges.
const MAX_SILENCE_GRACE_HOURS: i64 = 24 * 365 * 100;

/// How many overdue acts are named when the caller names no limit.
const DEFAULT_SILENCE_LIMIT: usize = 50;

/// The ceiling on the named list. The counts are always complete; only the
/// enumeration is capped, and what it left out comes back as `acts_omitted`.
const MAX_SILENCE_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
pub struct UnacknowledgedQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// How long an act may go unacknowledged before it counts as overdue.
    /// Defaults to a day; the applied value is always echoed back.
    #[serde(default)]
    pub grace_hours: Option<i64>,
    /// How many overdue acts to name. The counts never depend on it.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Map a delivery-read failure onto a status without flattening the two kinds.
///
/// An `ArtifactV2Error` in the chain is the store failing: **503**, because "we
/// could not read the dispatch log" must never come back as "nothing is
/// unacknowledged". That flattening is the exact false confidence this read
/// exists to remove.
fn delivery_failure(doing: &str, error: anyhow::Error) -> HttpResponse {
    let store_fault = error.chain().any(|cause| cause.is::<ArtifactV2Error>());
    if store_fault {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_log_unreadable",
            format!(
                "the delivery logs could not be read while {doing}, and an unreadable log is \
                 never an empty one: {error:#}"
            ),
            None,
        );
    }
    api_error_response(
        StatusCode::BAD_REQUEST,
        "delivery_read_refused",
        format!("{error:#}"),
        None,
    )
}

/// `GET /api/magician/v2/delivery/unacknowledged`
///
/// What this runtime dispatched and no provider ever acknowledged — oldest
/// first, with counts by rail.
///
/// Every number is derived from the clock on this read. Nothing has to have
/// run, nothing is stamped, and asking twice records nothing: a report that
/// depended on a sweep having written a `stale` flag would show silence only
/// where somebody had remembered to look, which is the shape of the bug this
/// read exists to catch.
///
/// # What the zeroes mean
///
/// `any_dispatch_recorded: false` is **not** a clean bill of health, and the
/// response says so rather than leaving an owner to infer it from an empty
/// list. A scope that has never dispatched anything and a scope where every
/// send was confirmed both report `overdue: 0`; only one of them is evidence
/// that sending works. `scanned` is carried beside every count for the same
/// reason — six silences over eight acts and six over eight thousand are
/// different facts.
///
/// An act whose rail could not be named is counted under `unattributed`, never
/// folded into a named rail: a rail that has gone quiet must not be able to hide
/// inside another rail's number.
pub async fn unacknowledged_dispatches_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<UnacknowledgedQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };

    let grace_hours = query.grace_hours.unwrap_or(DEFAULT_SILENCE_GRACE_HOURS);
    if grace_hours <= 0 {
        return bad_request(format!(
            "`grace_hours` must be positive; `{grace_hours}` marks every act overdue the instant \
             it leaves, and a signal that is always on is one nobody reads"
        ));
    }
    if grace_hours > MAX_SILENCE_GRACE_HOURS {
        return bad_request(format!(
            "`grace_hours` must be at most {MAX_SILENCE_GRACE_HOURS}; a longer window is a \
             window in which nothing is ever overdue"
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_SILENCE_LIMIT);
    if limit == 0 || limit > MAX_SILENCE_LIMIT {
        return bad_request(format!(
            "`limit` must be between 1 and {MAX_SILENCE_LIMIT}; a report that names no act is \
             indistinguishable from a scope with nothing to report"
        ));
    }

    let workspace_layout = api.workspace().clone();
    let scope = DeliveryScope::new(principal.clone(), workspace.clone());
    let dispatch_log = DispatchLog::new(workspace_layout.clone());
    let ledger = DeliveryLedger::new(workspace_layout.clone());
    let now = Utc::now();

    let scan = match scan_silence(
        &dispatch_log,
        &ledger,
        &scope,
        Duration::hours(grace_hours),
        now,
    ) {
        Ok(scan) => scan,
        Err(error) => return delivery_failure("reading unacknowledged dispatches", error),
    };

    // Attribution is resolved only for the acts the report names. Reading a
    // disclosure for every act ever dispatched would grow with the log forever
    // to answer a question about the acts that are NOT in it.
    let disclosures = OutwardAssertionStore::new(workspace_layout);
    let outward_scope = OutwardScope::new(principal.clone(), workspace.clone());
    let rails = match rails_from_disclosures(&disclosures, &outward_scope, &scan.act_refs()) {
        Ok(rails) => rails,
        Err(error) => return delivery_failure("attributing unacknowledged acts to a rail", error),
    };

    let report = match attribute(&scan, &rails, limit) {
        Ok(report) => report,
        Err(error) => return delivery_failure("folding the silence report", error),
    };

    let mut body = match serde_json::to_value(&report) {
        Ok(body) => body,
        Err(error) => {
            return delivery_failure("rendering the silence report", anyhow::anyhow!("{error}"))
        },
    };
    if let Some(map) = body.as_object_mut() {
        map.insert("principal".into(), json!(principal));
        map.insert("workspace".into(), json!(workspace));
        map.insert("grace_hours".into(), json!(grace_hours));
        map.insert("observed_at".into(), json!(now.to_rfc3339()));
        // Said outright rather than left to be inferred from `overdue: 0`.
        map.insert("quiet".into(), json!(report.is_quiet()));
    }
    HttpResponse::Ok().json(body)
}

/// What a work-axis re-index is asked for.
#[derive(Debug, serde::Deserialize)]
pub struct ReindexWorkAxesRequest {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `POST /api/magician/v2/delivery/reindex-work-axes`
///
/// Re-file every outward act in a scope under the work axes its own record
/// names, so a programme's or an engagement's history is reachable to the
/// sweeps that read those axes.
///
/// # Read the `unattributed` count, not the 200
///
/// This **re-indexes**; it does not reconstruct. An act whose record names no
/// work at all cannot be repaired here — the work was never written down, and
/// deriving it from the recipient or from the nearest engagement would file
/// real acts under a relationship nobody chose. For every act this runtime
/// dispatched before 2026-08-21 the answer is exactly that: the dispatch path
/// passed `None` for both fields, so they are unattributed and will stay so.
///
/// The route exists anyway, and not only for later acts: it is the difference
/// between *"the sweep finds nothing because the index is stale"* and *"the
/// sweep finds nothing because the history was never attributed"*, which look
/// identical from the sweep's side and have opposite remedies.
pub async fn reindex_work_axes_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    body: web::Json<ReindexWorkAxesRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = OutwardAssertionStore::new(api.workspace().clone());
    let scope = OutwardScope::new(principal.clone(), workspace.clone());
    match store.reindex_work_axes(&scope) {
        Ok(report) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "acts_seen": report.acts_seen,
            "engagement_entries": report.engagement_entries,
            "program_entries": report.program_entries,
            // Almost always the whole answer: acts are indexed at write time,
            // so a healthy store re-indexes to nothing. A run where this is LOW
            // is the one worth looking at.
            "already_indexed": report.already_indexed,
            "unattributed": report.unattributed,
            // Said outright. A caller reading a 200 and a zero would otherwise
            // conclude the index was already correct.
            "unattributed_is_not_repairable": report.unattributed > 0,
        })),
        Err(error) => delivery_failure("re-indexing outward acts by work", error),
    }
}

/// `GET /api/magician/v2/delivery/watch`
///
/// The health snapshot of the worker that asks the same question on a cadence —
/// `delivery_hygiene::worker::SuppressionSweepWorker`, whose tick reads every
/// configured scope's ledger.
///
/// The read above answers for whoever is already looking. This answers *"is
/// anything looking at all"*, which is the question that matters when a rail
/// goes quiet at three in the morning.
///
/// # The three states, and why they are three
///
/// The snapshot is rendered whole, and it carries three independent state
/// fields because collapsing them is exactly how a subsystem stays broken
/// behind a green dashboard:
///
/// - `state` — is the suppression register being written at all?
/// - `silence_state` — with `overdue` beside it: **how many dispatched acts
///   are still unacknowledged past the grace period.** This is the early
///   warning, and it climbs when a rail silently stops answering while every
///   send goes on succeeding.
/// - `receipts_state` — with `acts_left_dispatch_unknown` and
///   `acts_left_dispatch_unknown_total` beside it: **how many acts have
///   actually moved out of `dispatch_unknown`**, on this tick and since the
///   process started. Without it an operator can see that nothing is overdue
///   and have no way to tell whether receipts are arriving or whether nothing
///   ever leaves the unknown state at all.
///
/// `receipts_state: "no_source"` is the honest reading when no receipt source
/// is attached to the process, and it is deliberately **not** `idle`: it means
/// nothing pulls a provider receipt back, so every live send stays at
/// `dispatch_unknown` and the register below stays as empty as whatever anybody
/// posted by hand. `receipts_last_error` carries the sentence that says so.
///
/// Counts, never rates, here as everywhere in this subsystem: `receipts_offered`
/// sits beside `receipts_examined`, and `receipts_uncorrelated` — bounces that
/// arrived and could not be tied to any act this scope sent — is its own number
/// rather than a shortfall somebody has to compute.
///
/// # Not wired is 503, never healthy
///
/// The snapshot arrives as app state the binary attaches at boot. When it is
/// absent this handler refuses rather than answering a cheerful default: "no
/// watcher is running" and "the watcher found nothing" are opposite facts, and
/// rendering the first as the second is the whole failure this subsystem is
/// climbing out of.
pub async fn delivery_watch_health_handler(
    health: Option<web::Data<SuppressionSweepHealth>>,
) -> HttpResponse {
    let Some(health) = health else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_watch_not_wired",
            "no delivery-hygiene worker health is attached to this process, so nothing can be \
             said about whether unacknowledged dispatches are being watched at all. This is not \
             a healthy state and is deliberately not reported as one.",
            None,
        );
    };
    HttpResponse::Ok().json(health.snapshot().await)
}

// ---------------------------------------------------------------------------
// The door: recording what a provider said
// ---------------------------------------------------------------------------

/// The caller, as the outer boundary proved them.
///
/// The scope is the identity's, never the request's. See this module's header:
/// a route that let a caller choose the principal would let a caller choose
/// whose recipients to suppress.
struct ReceiptCaller {
    /// Whose ledger this receipt lands in, and — following
    /// `counterparties_api`'s precedent — who is recorded as having presented
    /// it. One value, not two: an actor that could differ from the scope it
    /// writes into would be a second thing to keep in step.
    principal: String,
    workspace: String,
    authentication: &'static str,
}

/// How the boundary proved the caller, as a token an auditor can read.
///
/// A match rather than a `Debug` rendering: a new authentication class will
/// fail to compile here rather than reaching the attempt log as a string nobody
/// chose.
fn authentication_label(authentication: VerifiedRequestAuthentication) -> &'static str {
    match authentication {
        VerifiedRequestAuthentication::CloudflareAccess => "cloudflare_access",
        VerifiedRequestAuthentication::PairedDevice => "paired_device",
        VerifiedRequestAuthentication::MagicianBearer => "magician_bearer",
        VerifiedRequestAuthentication::TrustedLoopbackSingleUser => "trusted_loopback_single_user",
    }
}

fn header_value(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Establish who is at the door, and refuse everything else.
///
/// `VerifiedRequestIdentity` is inserted by `verify_access_middleware` only
/// after Cloudflare Access, a paired device or a real loopback peer has been
/// verified, and it is deliberately not deserializable from a payload — so it
/// is the one thing on the request a caller cannot write. Its absence is a
/// `401`: an unproved request has nobody to record as having marked an address
/// complained, and an unattributable suppression is the one nobody can audit.
///
/// A scope header that **disagrees** with the proved identity is a `403` rather
/// than being quietly overridden. Letting the identity silently win would be
/// just as safe and would also hide a client that believes it is writing
/// somewhere else — and a client that quietly writes to the wrong ledger keeps
/// doing it, because nothing ever tells it otherwise.
///
/// A paired device and the loopback single user are bound to one exact
/// workspace, so theirs is used. An interactive Access owner has many, so one
/// must be named — and the absence of a name is a `400`, never a default: a
/// defaulted workspace would file a bounce against a ledger nobody chose.
fn receipt_caller(req: &HttpRequest) -> Result<ReceiptCaller, HttpResponse> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            api_error_response(
                StatusCode::UNAUTHORIZED,
                "delivery_receipt_actor_unproved",
                "this request was not proved by the outer boundary, so there is nobody to record \
                 as having presented this receipt. A receipt can suppress a recipient \
                 permanently, and an unattributable suppression is one nobody can audit \
                 afterwards — so an unproved request is refused rather than recorded anonymously",
                None,
            )
        })?;

    if header_value(req, "X-Principal")
        .as_deref()
        .is_some_and(|asserted| asserted != identity.principal())
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "delivery_receipt_scope_mismatch",
            "the principal named on this request is not the one the outer boundary proved. The \
             scope a receipt is filed under is taken from the proved identity and never from a \
             header, because a caller that could choose the principal could choose whose \
             recipients to suppress",
            None,
        ));
    }

    let asserted_workspace = header_value(req, "X-Workspace");
    let workspace =
        match identity.workspace() {
            Some(bound) => {
                if asserted_workspace
                    .as_deref()
                    .is_some_and(|asserted| asserted != bound)
                {
                    return Err(api_error_response(
                    StatusCode::FORBIDDEN,
                    "delivery_receipt_scope_mismatch",
                    "the workspace named on this request is not the one this credential is bound \
                     to. A receipt filed into a workspace the caller was not proved for is a \
                     suppression signal raised in somebody else's register",
                    None,
                ));
                }
                bound.to_owned()
            },
            None => match asserted_workspace {
                Some(workspace) => workspace,
                None => return Err(bad_request(
                    "name the workspace this receipt belongs to. This identity is not bound to \
                     one, and defaulting would file a bounce or a complaint against a ledger \
                     nobody chose",
                )),
            },
        };

    Ok(ReceiptCaller {
        principal: identity.principal().to_string(),
        workspace,
        authentication: authentication_label(identity.authentication()),
    })
}

/// Every state this build knows, for an error message that stays true when a
/// new one is added.
fn known_states() -> String {
    DeliveryState::ALL
        .iter()
        .map(|state| state.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The state a caller named, or `None`.
///
/// Read off `DeliveryState::ALL` rather than a hand-written match, so a state
/// added to the ledger is nameable here without this file being edited — and so
/// it cannot be *un*nameable while still being storable. `None` is "that is not
/// a state", never a default: guessing would be the difference between a soft
/// bounce that suppresses nobody and a hard one that suppresses forever.
fn state_from(label: &str) -> Option<DeliveryState> {
    let wanted = label.trim().to_ascii_lowercase();
    DeliveryState::ALL
        .into_iter()
        .find(|state| state.as_str() == wanted.as_str())
}

fn disposition_label(disposition: Reconciliation) -> &'static str {
    match disposition {
        Reconciliation::Opened => "opened",
        Reconciliation::Replayed => "replayed",
        Reconciliation::Advanced { .. } => "advanced",
        Reconciliation::Superseded { .. } => "superseded",
    }
}

/// Map an intake failure onto a status without flattening the two kinds.
///
/// An `ArtifactV2Error` in the chain is the store failing: **503**, because a
/// receipt that could not be written must never come back as one that was.
/// Everything else is the door or the ledger refusing the request — an act that
/// never left, a second identity under one provider message id, a changed
/// payload under one id — which is **400**, carrying the module's own sentence,
/// because that sentence says what is wrong and a generic refusal would invite
/// the caller to retry until something stuck.
fn receipt_failure(doing: &str, error: anyhow::Error) -> HttpResponse {
    let store_fault = error.chain().any(|cause| cause.is::<ArtifactV2Error>());
    if store_fault {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_receipt_store_unavailable",
            format!(
                "the delivery store could not be reached while {doing}, and a receipt that could \
                 not be recorded is never one that was: {error:#}"
            ),
            None,
        );
    }
    api_error_response(
        StatusCode::BAD_REQUEST,
        "delivery_receipt_refused",
        format!("{error:#}"),
        None,
    )
}

/// `POST /api/magician/v2/delivery/receipts`
///
/// `principal` and `workspace` are deliberately absent: they are the boundary's,
/// and `deny_unknown_fields` means a caller that tries to send them gets a
/// refusal rather than a silently ignored field it believes was honoured. The
/// same applies to any attempt to name the recording actor.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordDeliveryReceiptBody {
    /// The outward act this receipt is about. Must be an act this scope
    /// recorded as dispatched.
    pub act_ref: String,
    /// Which provider is speaking — `agentmail`, `kapso`, an SMTP relay, a push
    /// service. An open string: this door does not hold a list of providers,
    /// because a list is the thing that would have to be edited for the second
    /// rail.
    pub provider: String,
    /// The provider's own id for the message. The idempotency key, and the key
    /// the ledger binds to one identity.
    pub provider_message_id: String,
    /// Who the receipt is about. Normalised by the ledger, so a provider's
    /// capitalisation and angle-wrapping fold onto one person.
    pub identity: String,
    /// One of `DeliveryState`'s names. No default: see `state_from`.
    pub state: String,
    /// When the **provider** observed it. Required and never defaulted to now —
    /// the ledger keeps the provider's clock apart from ours precisely so a
    /// backdated event is not lost by a sweep that had already passed.
    pub observed_at: DateTime<Utc>,
    /// A ref an auditor follows back to the exact body, ticket or console
    /// screen this receipt was read from. Required: a receipt nobody can go back
    /// and read is an assertion, and the first person to doubt a suppression
    /// raised from it will remove it.
    pub payload_ref: String,
    /// `provider` or `operator`. **Required.** See `ReceiptSource`: defaulting
    /// would file a person's transcription as a provider's own report.
    pub source: String,
}

/// `POST /api/magician/v2/delivery/receipts`
///
/// The producer the whole subsystem was missing. It holds no store logic of its
/// own: it proves the caller, resolves the scope from that proof, hands this
/// scope's dispatch log and the receipt to `ReceiptIntake::admit`, and returns
/// what the ledger decided. Every rule it appears to enforce is enforced one
/// layer down, which is what stops this surface and the ledger drifting apart.
///
/// Idempotent because the ledger is. The identical receipt again is one record
/// and comes back as `replayed`; a stronger observation is `advanced`; one the
/// order refuses is `superseded` and is recorded for audit without moving the
/// state. A changed payload under an id already used is a refusal, not a quiet
/// no-op.
pub async fn record_delivery_receipt_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    body: web::Json<RecordDeliveryReceiptBody>,
) -> HttpResponse {
    let caller = match receipt_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let body = body.into_inner();

    let Some(state) = state_from(&body.state) else {
        return bad_request(format!(
            "`{}` is not a delivery state; this build knows {}. There is no default: a soft \
             bounce suppresses nobody and a hard one suppresses forever, so a state nobody named \
             cannot be guessed",
            body.state,
            known_states()
        ));
    };
    let Some(source) = ReceiptSource::parse(&body.source) else {
        return bad_request(format!(
            "`{}` is not a receipt source; name {}. There is no default: filing a person's \
             transcription as a provider's own report would give the wrong answer to the one \
             question a review asks, which is whether a provider really said this",
            body.source,
            ReceiptSource::ALL
                .iter()
                .map(|source| source.as_str())
                .collect::<Vec<_>>()
                .join(" or ")
        ));
    };

    let workspace_layout = api.workspace().clone();
    let scope = DeliveryScope::new(caller.principal.clone(), caller.workspace.clone());
    let now = Utc::now();

    // The candidate list is supplied, never discovered by the ledger. A store
    // fault here is a 503 rather than an empty list, because an empty list
    // would refuse every receipt with the wrong reason.
    let dispatched = match DispatchLog::new(workspace_layout.clone()).dispatched(&scope) {
        Ok(dispatched) => dispatched,
        Err(error) => return receipt_failure("reading what this scope dispatched", error),
    };

    let intake = ReceiptIntake::new(workspace_layout);
    let admitted = match intake.admit(
        &scope,
        &dispatched,
        &body.act_ref,
        &DeliveryReceipt {
            provider: body.provider.clone(),
            provider_message_id: body.provider_message_id.clone(),
            identity: body.identity.clone(),
            state,
            observed_at: body.observed_at,
            payload_ref: body.payload_ref.clone(),
        },
        &IntakeAttribution {
            source,
            actor: caller.principal.clone(),
            authentication: caller.authentication.to_string(),
        },
        now,
    ) {
        Ok(admitted) => admitted,
        Err(error) => return receipt_failure("recording a delivery receipt", error),
    };

    let observation = &admitted.outcome.observation;
    // Named separately rather than folded into `disposition`, so an owner
    // reading "superseded" can see WHAT held instead of the receipt they sent.
    let advanced_from = match admitted.outcome.disposition {
        Reconciliation::Advanced { from } => json!(from.as_str()),
        _ => serde_json::Value::Null,
    };
    let held = match admitted.outcome.disposition {
        Reconciliation::Superseded { held } => json!(held.as_str()),
        _ => serde_json::Value::Null,
    };
    // What the delivery-hygiene sweep will do with this, said outright: a
    // caller should not have to infer that `complained` suppresses forever.
    let suppression_cause = match admitted.outcome.identity_state.suppression_cause() {
        Some(cause) => json!(cause.as_str()),
        None => serde_json::Value::Null,
    };
    HttpResponse::Ok().json(json!({
        "principal": caller.principal,
        "workspace": caller.workspace,
        "act_ref": observation.act_ref,
        "receipt_id": observation.receipt_id,
        "attempt_id": admitted.attempt.attempt_id,
        "disposition": disposition_label(admitted.outcome.disposition),
        "advanced_from": advanced_from,
        "held": held,
        "identity": observation.identity,
        "identity_state": admitted.outcome.identity_state.as_str(),
        "act_state": admitted.outcome.act_state.as_str(),
        // Read off `is_arrival`, never off "no bad news". A bare acceptance is
        // not an arrival and this field says so.
        "act_reached": admitted.outcome.act_state.is_arrival(),
        "suppression_cause": suppression_cause,
        "source": admitted.attempt.source.as_str(),
        "recorded_by": admitted.attempt.actor,
        "authentication": admitted.attempt.authentication,
        "observed_at": observation.observed_at.to_rfc3339(),
        "recorded_at": observation.recorded_at.to_rfc3339(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct DeliveryReceiptsQuery {
    /// The act to report on. Required — there is no "every act" form, because a
    /// report over everything is a report nobody reads and a scan that grows
    /// with the log forever.
    pub act_ref: String,
}

/// `GET /api/magician/v2/delivery/receipts`
///
/// Everything on file for one act: what the provider said, and every attempt to
/// put something there — including the attempts the ledger refused, which are
/// the rows a review of "who tried to mark this address complained" is made of.
///
/// Behind the same gate as the write. It names actors, and that is not a list to
/// hand a caller the boundary never proved.
///
/// `dispatch_recorded: false` is reported rather than refused. A read is not a
/// write, and an act missing from the dispatch log is itself the finding.
pub async fn delivery_receipts_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    query: web::Query<DeliveryReceiptsQuery>,
) -> HttpResponse {
    let caller = match receipt_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let workspace_layout = api.workspace().clone();
    let scope = DeliveryScope::new(caller.principal.clone(), caller.workspace.clone());
    let act_ref = query.act_ref.trim().to_string();

    let intake = ReceiptIntake::new(workspace_layout.clone());
    let knowledge = match intake.ledger().state_of(&scope, &act_ref) {
        Ok(knowledge) => knowledge,
        Err(error) => return receipt_failure("reading an act's delivery state", error),
    };
    let observations = match intake.ledger().observations(&scope, &act_ref) {
        Ok(observations) => observations,
        Err(error) => return receipt_failure("reading an act's receipts", error),
    };
    let attempts = match intake.attempts(&scope, &act_ref) {
        Ok(attempts) => attempts,
        Err(error) => return receipt_failure("reading an act's intake attempts", error),
    };
    let dispatched = match DispatchLog::new(workspace_layout).dispatched(&scope) {
        Ok(dispatched) => dispatched,
        Err(error) => return receipt_failure("reading what this scope dispatched", error),
    };
    let dispatched_at = dispatched
        .iter()
        .find(|candidate| candidate.act_ref == act_ref)
        .map(|candidate| candidate.dispatched_at);

    let dispatched_at_json = match dispatched_at {
        Some(at) => json!(at.to_rfc3339()),
        None => serde_json::Value::Null,
    };
    HttpResponse::Ok().json(json!({
        "principal": caller.principal,
        "workspace": caller.workspace,
        "act_ref": act_ref,
        "dispatch_recorded": dispatched_at.is_some(),
        "dispatched_at": dispatched_at_json,
        // `dispatch_unknown` reads the same here as it does on the outward act,
        // and `reconciled`/`reached` are the two questions it is NOT an answer
        // to. See `DeliveryKnowledge`.
        "knowledge": knowledge.as_str(),
        "reconciled": knowledge.is_reconciled(),
        "reached": knowledge.reached(),
        // Counts beside the lists, never rates: two refused attempts over two
        // and over two hundred are different facts.
        "observation_count": observations.len(),
        "attempt_count": attempts.len(),
        "observations": observations
            .iter()
            .map(|row| serde_json::to_value(row).unwrap_or_else(|_| json!({})))
            .collect::<Vec<_>>(),
        "attempts": attempts
            .iter()
            .map(|row| serde_json::to_value(row).unwrap_or_else(|_| json!({})))
            .collect::<Vec<_>>(),
    }))
}

/// Mount the four register acts, the two delivery reads, and the receipt door.
///
/// **This is the named entry point.** `magician-bin/src/main.rs` calls it once,
/// inside the `/api/magician/v2` scope, so every route below is reachable the
/// moment this file compiles — no new `pub mod`, no second mount, and nothing
/// waiting on a registration somebody has to remember.
///
/// Two `.route` registrations on `/suppressions` is how this crate registers a
/// second method on one literal: `ServiceConfig::route` moves the method guard
/// onto the resource, so a non-matching method falls through to the next
/// registration rather than answering 405 from the first. `/delivery/receipts`
/// carries a `GET` and a `POST` the same way.
///
/// `/delivery/...` is registered here on purpose — see the header. One operator,
/// one subject: what became of the things we sent.
pub fn configure_suppression_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/suppressions", web::get().to(list_suppressions_handler))
        .route("/suppressions", web::post().to(record_suppression_handler))
        .route(
            "/suppressions/check",
            web::get().to(check_suppression_handler),
        )
        .route(
            "/suppressions/lift",
            web::post().to(lift_suppression_handler),
        )
        .route(
            "/delivery/unacknowledged",
            web::get().to(unacknowledged_dispatches_handler),
        )
        .route(
            "/delivery/watch",
            web::get().to(delivery_watch_health_handler),
        )
        .route(
            "/delivery/reindex-work-axes",
            web::post().to(reindex_work_axes_handler),
        )
        .route(
            "/delivery/receipts",
            web::post().to(record_delivery_receipt_handler),
        )
        .route(
            "/delivery/receipts",
            web::get().to(delivery_receipts_handler),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;

    fn api(dir: &tempfile::TempDir) -> web::Data<SuppressionApi> {
        web::Data::new(SuppressionApi::new(ArtifactV2Workspace::new(dir.path())))
    }

    fn scope() -> SuppressionScope {
        SuppressionScope::new("alpha", "prod")
    }

    /// Seed one real entry, so nothing below can pass against an empty store.
    fn seed_opt_out(api: &web::Data<SuppressionApi>) -> Suppression {
        let register = SuppressionRegister::global(api.workspace().clone());
        register
            .suppress(
                &scope(),
                "Quiet@Example.Test",
                SuppressionReason::OptOut,
                SuppressionEvidence::new(Utc::now() - Duration::hours(3), "unsub-form-1", "owner"),
                Utc::now() - Duration::hours(2),
            )
            .expect("the seed records")
    }

    async fn call(
        api: web::Data<SuppressionApi>,
        request: actix_test::TestRequest,
    ) -> (u16, serde_json::Value) {
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api)
                .configure(configure_suppression_routes),
        )
        .await;
        let response = actix_test::call_service(&app, request.to_request()).await;
        let status = response.status().as_u16();
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        (status, body)
    }

    fn scoped(request: actix_test::TestRequest) -> actix_test::TestRequest {
        request
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
    }

    /// The write path exists and the read path sees it.
    ///
    /// Pins the failure this file was written for: `suppress` had no non-test
    /// caller, so the register was empty on disk and every read — and every
    /// send-time screen — answered "nobody". The entry is asserted to EXIST
    /// with its exact reason and identity, not merely that a list came back.
    #[actix_web::test]
    async fn a_recorded_suppression_is_listed_and_counted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            scoped(actix_test::TestRequest::post().uri("/suppressions")).set_json(json!({
                "identity": "Dead@Example.Test",
                "reason": "hard_bounce",
                "established_at": "2026-08-20T09:00:00Z",
                "evidence_ref": "bounce-webhook-7",
                "recorded_by": "owner",
            })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["suppression"]["identity"], "dead@example.test");
        assert_eq!(body["suppression"]["reason"], "hard_bounce");
        assert_eq!(body["suppression"]["state"], "in_force");
        assert_eq!(body["span"], "global");

        let (status, listed) = call(
            api(&dir),
            scoped(actix_test::TestRequest::get().uri("/suppressions")),
        )
        .await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(listed["entries"].as_array().expect("entries").len(), 1);
        assert_eq!(listed["entries"][0]["identity"], "dead@example.test");
        assert_eq!(listed["in_force"], 1);
        assert_eq!(listed["covers_whole_register"], true);
        // Every reason is present, zeros included: "none" must not render like
        // "not measured".
        let counts = listed["counts"].as_array().expect("counts");
        assert_eq!(counts.len(), 5);
        assert_eq!(counts[0]["reason"], "opt_out");
        assert_eq!(counts[0]["count"], 0);
        assert_eq!(counts[1]["reason"], "hard_bounce");
        assert_eq!(counts[1]["count"], 1);
    }

    /// The check answers the send-time question, and answers it about a real
    /// entry that the same store really holds.
    #[actix_web::test]
    async fn a_check_blocks_a_suppressed_identity_and_clears_an_unknown_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let seeded = seed_opt_out(&data);
        assert_eq!(seeded.identity, "quiet@example.test");

        let (status, blocked) = call(
            data.clone(),
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=%3CQUIET%40example.test%3E"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{blocked}");
        assert_eq!(blocked["sendable"], false);
        assert_eq!(blocked["identity"], "quiet@example.test");
        assert_eq!(blocked["blocked_by"]["reason"], "opt_out");
        assert_eq!(blocked["blocked_by"]["state"], "in_force");
        assert_eq!(blocked["history"].as_array().expect("history").len(), 1);

        let (status, clear) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=someone.else%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{clear}");
        assert_eq!(clear["sendable"], true);
        assert_eq!(clear["blocked_by"], serde_json::Value::Null);
        assert_eq!(clear["history"].as_array().expect("history").len(), 0);
    }

    /// **The rule this surface must not offer a route around.**
    ///
    /// An opt-out is seeded, asserted present, and then an `operational` lift
    /// is refused — and, critically, the identity is still blocked afterwards.
    /// A test that only asserted the 400 would pass against a route that
    /// returned an error *and* lifted the entry anyway.
    #[actix_web::test]
    async fn an_operational_lift_cannot_clear_an_opt_out() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, before) = call(
            data.clone(),
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=quiet%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{before}");
        assert_eq!(
            before["sendable"], false,
            "the seed must really be in force"
        );

        let (status, refused) = call(
            data.clone(),
            scoped(actix_test::TestRequest::post().uri("/suppressions/lift")).set_json(json!({
                "identity": "quiet@example.test",
                "reason": "opt_out",
                "authority": "operational",
                "established_at": "2026-08-20T12:00:00Z",
                "evidence_ref": "tidy-up-ticket-4",
                "recorded_by": "some-sweep",
            })),
        )
        .await;
        assert_eq!(status, 400, "{refused}");
        assert_eq!(refused["code"], "suppression_refused");
        assert!(
            refused["error"]
                .as_str()
                .expect("an error sentence")
                .contains("owner act"),
            "{refused}"
        );

        let (status, after) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=quiet%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{after}");
        assert_eq!(
            after["sendable"], false,
            "the refusal must not have lifted the entry on its way out"
        );
        assert_eq!(after["blocked_by"]["reason"], "opt_out");
    }

    /// An owner act with evidence does lift it — otherwise the rule above would
    /// be indistinguishable from "this route never works".
    #[actix_web::test]
    async fn an_owner_act_with_evidence_lifts_an_opt_out() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, lifted) = call(
            data.clone(),
            scoped(actix_test::TestRequest::post().uri("/suppressions/lift")).set_json(json!({
                "identity": "quiet@example.test",
                "reason": "opt_out",
                "authority": "owner_act",
                "established_at": "2026-08-20T12:00:00Z",
                "evidence_ref": "signed-note-9",
                "recorded_by": "owner",
            })),
        )
        .await;
        assert_eq!(status, 200, "{lifted}");
        let rows = lifted["lifted"].as_array().expect("lifted rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "lifted");
        assert_eq!(rows[0]["lift"]["authority"], "owner_act");

        let (status, after) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=quiet%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{after}");
        assert_eq!(after["sendable"], true);
        // Forward-only: the lifted row is still on the register, which is what
        // a compliance review reads.
        assert_eq!(after["history"].as_array().expect("history").len(), 1);
        assert_eq!(after["history"][0]["state"], "lifted");
    }

    /// A missing or unknown authority is refused rather than defaulted.
    ///
    /// The default that would be convenient here — `owner_act` — is precisely
    /// the loophole, so its absence is pinned.
    #[actix_web::test]
    async fn an_unnamed_authority_is_refused_never_defaulted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, unknown) = call(
            data.clone(),
            scoped(actix_test::TestRequest::post().uri("/suppressions/lift")).set_json(json!({
                "identity": "quiet@example.test",
                "reason": "opt_out",
                "authority": "owner",
                "established_at": "2026-08-20T12:00:00Z",
                "evidence_ref": "signed-note-9",
                "recorded_by": "owner",
            })),
        )
        .await;
        assert_eq!(status, 400, "{unknown}");
        assert!(
            unknown["error"]
                .as_str()
                .expect("an error sentence")
                .contains("no default"),
            "{unknown}"
        );

        // Omitting the field entirely is a deserialisation failure, not a
        // silently-defaulted `owner_act`.
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(data.clone())
                .configure(configure_suppression_routes),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            scoped(actix_test::TestRequest::post().uri("/suppressions/lift"))
                .set_json(json!({
                    "identity": "quiet@example.test",
                    "reason": "opt_out",
                    "established_at": "2026-08-20T12:00:00Z",
                    "evidence_ref": "signed-note-9",
                    "recorded_by": "owner",
                }))
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 400);

        // And the entry is still in force after both attempts.
        let (status, after) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=quiet%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{after}");
        assert_eq!(after["sendable"], false);
    }

    /// A consent decision may not be given an end date, through this route or
    /// any other. An opt-out with a timer expires quietly while nobody looks.
    #[actix_web::test]
    async fn a_time_boxed_opt_out_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, refused) = call(
            api(&dir),
            scoped(actix_test::TestRequest::post().uri("/suppressions")).set_json(json!({
                "identity": "quiet@example.test",
                "reason": "opt_out",
                "established_at": "2026-08-20T09:00:00Z",
                "evidence_ref": "unsub-1",
                "recorded_by": "owner",
                "in_force_until": "2099-01-01T00:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, 400, "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .expect("an error sentence")
                .contains("may not be time-boxed"),
            "{refused}"
        );
    }

    /// A regulatory hold may be time-boxed, and its end is derived from the
    /// clock rather than swept: an entry whose window has closed reads
    /// `expired` on the next read with nothing having run.
    #[actix_web::test]
    async fn a_time_boxed_hold_expires_on_the_clock() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let register = SuppressionRegister::global(data.workspace().clone());
        register
            .suppress_until(
                &scope(),
                "held@example.test",
                SuppressionReason::RegulatoryHold,
                SuppressionEvidence::new(Utc::now() - Duration::hours(4), "dispute-2", "owner"),
                Utc::now() - Duration::minutes(1),
                Utc::now() - Duration::hours(3),
            )
            .expect("a hold that has since lapsed");

        let (status, body) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=held%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["sendable"], true, "an inclusive expiry has arrived");
        assert_eq!(body["history"][0]["state"], "expired");
    }

    /// An audience-scoped write lands in that audience's span and does not
    /// pretend to be global; the kind is part of the key.
    #[actix_web::test]
    async fn an_audience_scoped_suppression_is_filed_under_its_kind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let (status, body) = call(
            data.clone(),
            scoped(actix_test::TestRequest::post().uri("/suppressions")).set_json(json!({
                "identity": "drop.me@example.test",
                "reason": "owner_blocked",
                "established_at": "2026-08-20T09:00:00Z",
                "evidence_ref": "cohort-note-3",
                "recorded_by": "owner",
                "audience_kind": "account",
                "audience_id": "acme",
            })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["span"], "audience:account:acme");

        // The same id under a different kind is a different audience, so the
        // entry must not be visible there.
        let (status, other) = call(
            data.clone(),
            scoped(actix_test::TestRequest::get().uri(
                "/suppressions/check?identity=drop.me%40example.test&audience_kind=engagement&audience_id=acme",
            )),
        )
        .await;
        assert_eq!(status, 200, "{other}");
        assert_eq!(other["sendable"], true);
        assert_eq!(other["span"], "audience:engagement:acme");

        // And the global register is untouched by a per-audience decision.
        let (status, global) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=drop.me%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{global}");
        assert_eq!(global["sendable"], true);
        assert_eq!(global["span"], "global");
    }

    /// Half an audience is refused. A kind with no id would fall back to the
    /// global register and write a per-audience decision into everybody's.
    #[actix_web::test]
    async fn half_an_audience_is_refused_rather_than_falling_back_to_global() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            scoped(actix_test::TestRequest::get().uri("/suppressions?audience_kind=account")),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]
                .as_str()
                .expect("an error sentence")
                .contains("without `audience_id`"),
            "{body}"
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            scoped(actix_test::TestRequest::get().uri("/suppressions?audience_id=acme")),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]
                .as_str()
                .expect("an error sentence")
                .contains("without `audience_kind`"),
            "{body}"
        );
    }

    /// An unknown audience kind is refused with the list of the known ones,
    /// never filed under a default kind's key.
    #[actix_web::test]
    async fn an_unknown_audience_kind_is_refused_with_the_known_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions?audience_kind=cohort&audience_id=acme"),
            ),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        let sentence = body["error"].as_str().expect("an error sentence");
        assert!(sentence.contains("not an audience kind"), "{sentence}");
        for known in ["engagement", "program", "account", "panel", "person"] {
            assert!(sentence.contains(known), "{sentence} is missing {known}");
        }
    }

    /// An identity carrying the id separator is refused, not stored.
    #[actix_web::test]
    async fn an_identity_holding_the_unit_separator_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            scoped(actix_test::TestRequest::post().uri("/suppressions")).set_json(json!({
                "identity": "a\u{1f}b@example.test",
                "reason": "owner_blocked",
                "established_at": "2026-08-20T09:00:00Z",
                "evidence_ref": "note-1",
                "recorded_by": "owner",
            })),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]
                .as_str()
                .expect("an error sentence")
                .contains("U+001F"),
            "{body}"
        );
    }

    /// An unreadable register answers 503, never 200 with an empty list.
    ///
    /// The seeded entry is asserted visible first, so this cannot pass against
    /// a store that was empty all along.
    #[actix_web::test]
    async fn an_unreadable_register_answers_503_not_an_empty_list() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, before) = call(
            data.clone(),
            scoped(actix_test::TestRequest::get().uri("/suppressions")),
        )
        .await;
        assert_eq!(status, 200, "{before}");
        assert_eq!(before["entries"].as_array().expect("entries").len(), 1);

        // A directory where the span's index belongs reproduces the whole class
        // of non-NotFound read faults portably.
        let index = data
            .workspace()
            .scope_root("alpha", "prod")
            .join("suppression")
            .join("global")
            .join("index.jsonl");
        std::fs::remove_file(&index).expect("remove the index");
        std::fs::create_dir_all(&index).expect("a directory in its place");

        let (status, body) = call(
            data.clone(),
            scoped(actix_test::TestRequest::get().uri("/suppressions")),
        )
        .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["code"], "suppression_register_unreadable");
    }

    /// Scope is required before anything else is considered; a read without one
    /// must not default to somebody's register.
    #[actix_web::test]
    async fn a_read_without_scope_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api(&dir))
                .configure(configure_suppression_routes),
        )
        .await;
        for uri in [
            "/suppressions",
            "/suppressions/check?identity=someone@example.test",
        ] {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::get().uri(uri).to_request(),
            )
            .await;
            assert_ne!(
                response.status().as_u16(),
                200,
                "{uri} answered without a scope"
            );
        }
    }

    /// A lookback that describes an empty window is refused rather than
    /// answering an empty list an owner would read as "nobody is suppressed".
    #[actix_web::test]
    async fn a_non_positive_lookback_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, body) = call(
            data,
            scoped(actix_test::TestRequest::get().uri("/suppressions?lookback_hours=0")),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]
                .as_str()
                .expect("an error sentence")
                .contains("must be positive"),
            "{body}"
        );
    }

    /// A narrowed read says so, so an owner cannot mistake a window for the
    /// whole register.
    #[actix_web::test]
    async fn a_narrowed_read_reports_its_window() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        // The seed was recorded two hours ago, so a one-hour window excludes it.
        let (status, narrow) = call(
            data.clone(),
            scoped(actix_test::TestRequest::get().uri("/suppressions?lookback_hours=1")),
        )
        .await;
        assert_eq!(status, 200, "{narrow}");
        assert_eq!(narrow["entries"].as_array().expect("entries").len(), 0);
        assert_eq!(narrow["covers_whole_register"], false);
        assert!(narrow["since"].is_string());

        // And the same read over the whole register finds it, which is what
        // makes the zero above a window rather than an empty store.
        let (status, wide) = call(
            data,
            scoped(actix_test::TestRequest::get().uri("/suppressions?lookback_hours=24")),
        )
        .await;
        assert_eq!(status, 200, "{wide}");
        assert_eq!(wide["entries"].as_array().expect("entries").len(), 1);
    }

    /// A lookback wider than any clock answers the whole register instead of
    /// panicking the handler.
    ///
    /// Pins a real hazard rather than a hypothetical one: subtracting an
    /// unbounded `Duration::hours` from the clock overflows, and a panicking
    /// report reads to an owner as an outage rather than as an answer — which
    /// is the one reading that would send them looking somewhere other than the
    /// register.
    #[actix_web::test]
    async fn an_absurd_lookback_covers_the_whole_register_rather_than_overflowing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_opt_out(&data);

        let (status, body) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions?lookback_hours=9223372036854775807"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["covers_whole_register"], true);
        assert_eq!(body["entries"].as_array().expect("entries").len(), 1);
    }

    // ── Delivery silence ────────────────────────────────────────────────────

    /// Prepare a real disclosure and record its dispatch, so the act ref the
    /// two logs share is the one the store derived rather than one this test
    /// invented.
    fn seed_dispatch(
        data: &web::Data<SuppressionApi>,
        key: &str,
        channel: magician::magician_v2::evidence::OutwardChannel,
        at: DateTime<Utc>,
    ) -> String {
        use magician::magician_v2::evidence::PrepareOutwardAct;

        let act = OutwardAssertionStore::new(data.workspace().clone())
            .prepare(
                &OutwardScope::new("alpha", "prod"),
                &PrepareOutwardAct {
                    idempotency_key: key.to_string(),
                    program_id: None,
                    engagement_id: None,
                    exact_payload_artifact_ref: format!("artifact-{key}"),
                    effective_sender: "owner@example.test".to_string(),
                    intended_audience: vec!["them@example.test".to_string()],
                    channel,
                    consequence_class: "routine".to_string(),
                },
                &at.to_rfc3339(),
            )
            .expect("the disclosure prepares");
        DispatchLog::new(data.workspace().clone())
            .record(
                &DeliveryScope::new("alpha", "prod"),
                &act.outward_act_ref,
                at,
            )
            .expect("the dispatch records");
        act.outward_act_ref
    }

    /// The read names what was sent and never acknowledged, oldest first, and
    /// counts it by rail.
    ///
    /// Pins the whole of tier 3 at the surface. `DeliveryLedger::unreconciled`
    /// existed and had no caller, so a live send that no provider ever
    /// confirmed produced no signal anywhere — the failure mode of a silently
    /// broken integration was literally unobservable. The store is seeded with
    /// three real dispatches first, so nothing here can pass against an empty
    /// one.
    #[actix_web::test]
    async fn the_read_names_unacknowledged_acts_oldest_first_and_counts_them_by_rail() {
        use magician::magician_v2::evidence::OutwardChannel;

        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let oldest = seed_dispatch(
            &data,
            "mail-1",
            OutwardChannel::Email,
            Utc::now() - Duration::hours(9),
        );
        seed_dispatch(
            &data,
            "mail-2",
            OutwardChannel::Email,
            Utc::now() - Duration::hours(5),
        );
        seed_dispatch(
            &data,
            "wa-1",
            OutwardChannel::WhatsApp,
            Utc::now() - Duration::hours(4),
        );

        let (status, body) = call(
            data,
            scoped(
                actix_test::TestRequest::get()
                    .uri("/delivery/unacknowledged?grace_hours=1&limit=10"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["any_dispatch_recorded"], true);
        assert_eq!(body["grace_hours"], 1);
        assert_eq!(body["scanned"], 3);
        assert_eq!(body["acknowledged"], 0);
        assert_eq!(body["unacknowledged"], 3);
        assert_eq!(body["overdue"], 3);
        assert_eq!(body["acts_omitted"], 0);
        assert_eq!(body["quiet"], false);

        let acts = body["acts"].as_array().expect("named acts");
        assert_eq!(acts.len(), 3);
        assert_eq!(acts[0]["act_ref"], oldest, "oldest silence first");
        assert_eq!(acts[0]["rail"], "email");
        assert!(
            acts[0]["silent_for_secs"].as_i64().expect("a silence") >= 9 * 3600,
            "{body}"
        );

        // Counts by rail, worst first — the axis that says WHICH integration
        // went quiet rather than merely that something did.
        let by_rail = body["by_rail"].as_array().expect("rails");
        assert_eq!(by_rail.len(), 2);
        assert_eq!(by_rail[0]["rail"], "email");
        assert_eq!(by_rail[0]["unacknowledged"], 2);
        assert_eq!(by_rail[0]["overdue"], 2);
        assert_eq!(by_rail[1]["rail"], "whatsapp");
        assert_eq!(by_rail[1]["overdue"], 1);
    }

    /// A scope that has never dispatched anything says so and is NOT reported
    /// as quiet.
    ///
    /// The vacuous pass, at the surface. `overdue: 0` over no dispatches and
    /// `overdue: 0` over four hundred confirmed sends are opposite facts, and
    /// an owner reading the first as the second is exactly how a subsystem runs
    /// for a year behind a green dashboard.
    #[actix_web::test]
    async fn a_scope_with_no_dispatches_is_never_reported_as_quiet() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);

        let (status, body) = call(
            data,
            scoped(actix_test::TestRequest::get().uri("/delivery/unacknowledged")),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["any_dispatch_recorded"], false);
        assert_eq!(body["scanned"], 0);
        assert_eq!(body["overdue"], 0);
        assert_eq!(
            body["quiet"], false,
            "no dispatches is not a clean bill of health"
        );
        // The default is applied and echoed, so a defaulted window can never be
        // mistaken for one the caller chose.
        assert_eq!(body["grace_hours"], DEFAULT_SILENCE_GRACE_HOURS);
    }

    /// A grace of zero is refused rather than answering that everything is
    /// overdue.
    #[actix_web::test]
    async fn a_non_positive_grace_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);

        let (status, body) = call(
            data,
            scoped(actix_test::TestRequest::get().uri("/delivery/unacknowledged?grace_hours=0")),
        )
        .await;
        assert_eq!(status, 400, "{body}");
        assert!(
            body["error"]
                .as_str()
                .expect("an error sentence")
                .contains("nobody reads"),
            "{body}"
        );
    }

    /// An unreadable dispatch log answers 503, never a 200 saying nothing is
    /// unacknowledged.
    ///
    /// This is the fail-open that would matter most: the one moment an owner
    /// checks whether delivery is working is the moment a disk fault would tell
    /// them it is.
    #[actix_web::test]
    async fn an_unreadable_dispatch_log_answers_503_not_an_empty_report() {
        use magician::magician_v2::evidence::OutwardChannel;

        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_dispatch(
            &data,
            "mail-1",
            OutwardChannel::Email,
            Utc::now() - Duration::hours(9),
        );

        // A directory where the log belongs reproduces a non-NotFound read
        // fault portably — the same trick the register's own test uses.
        let log = data
            .workspace()
            .scope_root("alpha", "prod")
            .join("delivery")
            .join("dispatched.jsonl");
        std::fs::remove_file(&log).expect("remove the dispatch log");
        std::fs::create_dir_all(&log).expect("a directory in its place");

        let (status, body) = call(
            data,
            scoped(actix_test::TestRequest::get().uri("/delivery/unacknowledged")),
        )
        .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["code"], "delivery_log_unreadable");
    }

    /// With no watcher attached, the health read refuses rather than answering
    /// a cheerful default.
    ///
    /// "No watcher is running" and "the watcher found nothing" are opposite
    /// facts. Rendering the first as the second is the failure this whole
    /// subsystem is climbing out of, so the absence of the snapshot is a 503
    /// with a named code an operator can alert on.
    #[actix_web::test]
    async fn the_watch_read_refuses_when_nothing_is_watching() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (status, body) = call(
            api(&dir),
            actix_test::TestRequest::get().uri("/delivery/watch"),
        )
        .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["code"], "delivery_watch_not_wired");
    }

    /// With a watcher attached, the health read hands back its counts.
    ///
    /// Without this the test above would also pass for a route that refused
    /// unconditionally, which would be a health endpoint that can never report
    /// health.
    #[actix_web::test]
    async fn the_watch_read_reports_the_workers_counts_when_one_is_attached() {
        use magician::config::DeliveryHygieneConfig;

        let dir = tempfile::tempdir().expect("temp dir");
        let health = SuppressionSweepHealth::new(&DeliveryHygieneConfig::default());
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api(&dir))
                .app_data(web::Data::new(health))
                .configure(configure_suppression_routes),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/delivery/watch")
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 200);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["silence_state"], "idle");
        assert_eq!(body["silence_grace_hours"], 24);
        assert_eq!(body["silence_alert_overdue"], 1);
        assert_eq!(body["overdue"], 0);
    }

    // ── The receipt door ────────────────────────────────────────────────────

    /// Run the request through the **real** access middleware.
    ///
    /// Not a convenience. `VerifiedRequestIdentity` has no public constructor —
    /// deliberately, so nothing can fabricate one — which means the only honest
    /// way to test a route that requires it is to put the middleware that mints
    /// it in front. A test that reached past the boundary would be pinning a
    /// handler nobody can reach the same way.
    async fn call_at_boundary(
        api: web::Data<SuppressionApi>,
        request: actix_test::TestRequest,
    ) -> (u16, serde_json::Value) {
        use actix_web::middleware::from_fn;
        use magician::magician_v2::cloudflare_access::{verify_access_middleware, AccessVerifier};
        use std::sync::Arc;

        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api)
                // Unconfigured, exactly as local development runs. The loopback
                // branch below is then the identity under test.
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_suppression_routes),
        )
        .await;
        let response = actix_test::call_service(&app, request.to_request()).await;
        let status = response.status().as_u16();
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        (status, body)
    }

    /// The status alone, for the cases actix refuses **before** the handler.
    ///
    /// A body that fails to deserialise is rejected by the `web::Json`
    /// extractor, whose error body is plain text rather than JSON — so reading
    /// it as JSON would panic and hide the very refusal being pinned.
    async fn status_at_boundary(
        api: web::Data<SuppressionApi>,
        request: actix_test::TestRequest,
    ) -> u16 {
        use actix_web::middleware::from_fn;
        use magician::magician_v2::cloudflare_access::{verify_access_middleware, AccessVerifier};
        use std::sync::Arc;

        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api)
                .app_data(web::Data::new(None::<Arc<AccessVerifier>>))
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_suppression_routes),
        )
        .await;
        actix_test::call_service(&app, request.to_request())
            .await
            .status()
            .as_u16()
    }

    /// A request from a real loopback peer — what the middleware turns into a
    /// `TrustedLoopbackSingleUser` identity bound to the default scope.
    fn from_loopback(request: actix_test::TestRequest) -> actix_test::TestRequest {
        request.peer_addr("127.0.0.1:52020".parse().expect("a loopback peer"))
    }

    /// The scope that identity is bound to. Read off the constants rather than
    /// written out, so the binding is named rather than coincidental.
    fn loopback_scope() -> (&'static str, &'static str) {
        use magician::magician_v2::artifact_v2::workspace::{
            DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
        };
        (DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
    }

    fn loopback_scoped(request: actix_test::TestRequest) -> actix_test::TestRequest {
        let (principal, workspace) = loopback_scope();
        from_loopback(
            request
                .insert_header(("X-Principal", principal))
                .insert_header(("X-Workspace", workspace)),
        )
    }

    /// Prepare a real disclosure and record its dispatch in the loopback scope,
    /// so the act ref is the one the store derived rather than one this test
    /// invented.
    fn seed_loopback_dispatch(
        data: &web::Data<SuppressionApi>,
        key: &str,
        at: DateTime<Utc>,
    ) -> String {
        use magician::magician_v2::evidence::{OutwardChannel, PrepareOutwardAct};

        let (principal, workspace) = loopback_scope();
        let act = OutwardAssertionStore::new(data.workspace().clone())
            .prepare(
                &OutwardScope::new(principal, workspace),
                &PrepareOutwardAct {
                    idempotency_key: key.to_string(),
                    program_id: None,
                    engagement_id: None,
                    exact_payload_artifact_ref: format!("artifact-{key}"),
                    effective_sender: "owner@example.test".to_string(),
                    intended_audience: vec!["them@example.test".to_string()],
                    channel: OutwardChannel::Email,
                    consequence_class: "routine".to_string(),
                },
                &at.to_rfc3339(),
            )
            .expect("the disclosure prepares");
        DispatchLog::new(data.workspace().clone())
            .record(
                &DeliveryScope::new(principal, workspace),
                &act.outward_act_ref,
                at,
            )
            .expect("the dispatch records");
        act.outward_act_ref
    }

    fn receipt_body(
        act_ref: &str,
        state: &str,
        message_id: &str,
        identity: &str,
    ) -> serde_json::Value {
        json!({
            "act_ref": act_ref,
            "provider": "agentmail",
            "provider_message_id": message_id,
            "identity": identity,
            "state": state,
            "observed_at": "2026-08-21T10:00:00Z",
            "payload_ref": format!("payload://{message_id}"),
            "source": "provider",
        })
    }

    /// **The whole of tier 4, end to end.**
    ///
    /// Pins the failure this door was written for: `DeliveryLedger::reconcile`
    /// had no production caller, so every live send was parked at
    /// `dispatch_unknown` for good and an unacknowledged send was
    /// indistinguishable from a successful one. The act is asserted overdue on
    /// the tier-3 read FIRST — so nothing here can pass against an empty store —
    /// then a receipt is recorded through the door, and the same read is
    /// asserted to have gone quiet. `quiet: true` is a value this subsystem
    /// could not previously produce at all.
    #[actix_web::test]
    async fn a_recorded_receipt_reconciles_the_act_and_clears_the_silence() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, before) = call_at_boundary(
            data.clone(),
            loopback_scoped(
                actix_test::TestRequest::get()
                    .uri("/delivery/unacknowledged?grace_hours=1&limit=10"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{before}");
        assert_eq!(before["scanned"], 1);
        assert_eq!(before["overdue"], 1);
        assert_eq!(before["acts"][0]["act_ref"], act_ref);
        assert_eq!(before["quiet"], false);

        let (status, recorded) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "delivered", "pm-1", "Them@Example.Test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{recorded}");
        assert_eq!(recorded["disposition"], "opened");
        assert_eq!(recorded["identity"], "them@example.test");
        assert_eq!(recorded["identity_state"], "delivered");
        assert_eq!(recorded["act_state"], "delivered");
        assert_eq!(recorded["act_reached"], true);
        assert_eq!(recorded["suppression_cause"], serde_json::Value::Null);
        assert_eq!(recorded["source"], "provider");
        assert_eq!(recorded["authentication"], "trusted_loopback_single_user");
        let (principal, _) = loopback_scope();
        assert_eq!(recorded["recorded_by"], principal);
        assert!(
            recorded["receipt_id"]
                .as_str()
                .expect("a receipt id")
                .starts_with("rcpt-"),
            "{recorded}"
        );

        let (status, after) = call_at_boundary(
            data,
            loopback_scoped(
                actix_test::TestRequest::get()
                    .uri("/delivery/unacknowledged?grace_hours=1&limit=10"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{after}");
        assert_eq!(after["scanned"], 1);
        assert_eq!(after["acknowledged"], 1);
        assert_eq!(after["unacknowledged"], 0);
        assert_eq!(after["overdue"], 0);
        assert_eq!(
            after["quiet"], true,
            "an acknowledged act leaves the silence"
        );
    }

    /// **An unproved request cannot record a receipt.**
    ///
    /// This is the whole risk of the route: `complained` suppresses a real
    /// recipient permanently and `delivered` silences a real failure, so a door
    /// anybody could POST to is a remote suppression primitive. The request is
    /// made from a non-loopback peer with no Access assertion, which is exactly
    /// what an attacker past the tunnel looks like, and the act is asserted
    /// still unreconciled afterwards — a test that only checked the 401 would
    /// pass against a handler that refused and wrote anyway.
    #[actix_web::test]
    async fn an_unproved_request_cannot_record_a_receipt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, refused) = call_at_boundary(
            data.clone(),
            actix_test::TestRequest::post()
                .uri("/delivery/receipts")
                .peer_addr("203.0.113.5:41000".parse().expect("a remote peer"))
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .set_json(receipt_body(
                    &act_ref,
                    "complained",
                    "pm-1",
                    "victim@example.test",
                )),
        )
        .await;
        assert_eq!(status, 401, "{refused}");
        assert_eq!(refused["code"], "delivery_receipt_actor_unproved");

        let (status, state) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(state["knowledge"], "dispatch_unknown");
        assert_eq!(state["reconciled"], false);
        assert_eq!(state["reached"], false);
        assert_eq!(state["observation_count"], 0);
        assert_eq!(
            state["attempt_count"], 0,
            "a refused request never reached the door at all"
        );
    }

    /// A proved caller may not file a receipt into a scope it was not proved
    /// for.
    ///
    /// The loopback identity is bound to one exact workspace. A header naming
    /// another is a `403`, never a silent override: a receipt filed into
    /// somebody else's workspace is a suppression signal raised in somebody
    /// else's register.
    #[actix_web::test]
    async fn a_receipt_cannot_name_a_scope_the_caller_was_not_proved_for() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));
        let body = receipt_body(&act_ref, "complained", "pm-1", "victim@example.test");

        for (header, value) in [
            ("X-Principal", "somebody-else"),
            ("X-Workspace", "elsewhere"),
        ] {
            let (status, refused) = call_at_boundary(
                data.clone(),
                from_loopback(actix_test::TestRequest::post().uri("/delivery/receipts"))
                    .insert_header((header, value))
                    .set_json(body.clone()),
            )
            .await;
            assert_eq!(status, 403, "{header} was accepted: {refused}");
            assert_eq!(refused["code"], "delivery_receipt_scope_mismatch");
        }

        // And nothing was written on the way out of either refusal.
        let (status, state) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(state["knowledge"], "dispatch_unknown");
        assert_eq!(state["attempt_count"], 0);
    }

    /// A receipt for an act this scope never dispatched is refused, and
    /// suppresses nobody.
    ///
    /// The second half of the remote-suppression risk: even a proved caller must
    /// not be able to mint a complaint against an arbitrary string. The
    /// suppression register is checked afterwards through the same `screen` the
    /// send-time gate calls, because that — not the 400 — is what would actually
    /// have gone wrong.
    #[actix_web::test]
    async fn a_receipt_for_an_act_that_never_left_is_refused_and_suppresses_nobody() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, refused) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(
                    "act-nobody-sent",
                    "complained",
                    "pm-9",
                    "victim@example.test",
                ),
            ),
        )
        .await;
        assert_eq!(status, 400, "{refused}");
        assert_eq!(refused["code"], "delivery_receipt_refused");
        assert!(
            refused["error"]
                .as_str()
                .expect("an error sentence")
                .contains("not among the 1 acts"),
            "{refused}"
        );

        // The ledger raised no signal, so the hygiene sweep has nothing to
        // ingest and the address is still sendable.
        let signals = DeliveryLedger::new(data.workspace().clone())
            .suppression_signals(
                &DeliveryScope::new(loopback_scope().0, loopback_scope().1),
                DateTime::<Utc>::MIN_UTC,
            )
            .expect("signals read");
        assert_eq!(signals, Vec::new());

        let (status, screened) = call_at_boundary(
            data,
            loopback_scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=victim%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{screened}");
        assert_eq!(screened["sendable"], true);
    }

    /// A complaint carries its suppression cause, and a later delivery cannot
    /// undo it.
    ///
    /// The severity order, reached through the door rather than restated by it.
    /// `Complained` outranks everything, so a `delivered` arriving afterwards is
    /// recorded for audit and comes back as `superseded` with the complaint
    /// named — and the act's state has not moved.
    #[actix_web::test]
    async fn a_complaint_carries_its_cause_and_a_later_delivery_cannot_undo_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, complaint) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "complained", "pm-1", "cross@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{complaint}");
        assert_eq!(complaint["identity_state"], "complained");
        assert_eq!(complaint["suppression_cause"], "complaint");
        assert_eq!(
            complaint["act_reached"], true,
            "somebody who reports a message had to receive it first"
        );

        let (status, later) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "delivered", "pm-2", "cross@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{later}");
        assert_eq!(later["disposition"], "superseded");
        assert_eq!(later["held"], "complained");
        assert_eq!(later["identity_state"], "complained");
        assert_eq!(later["act_state"], "complained");

        // Nothing was dropped: the refused observation is on the act's log.
        let (status, state) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(state["knowledge"], "complained");
        assert_eq!(state["observation_count"], 2);
        assert_eq!(state["attempt_count"], 2);
        assert_eq!(state["dispatch_recorded"], true);
    }

    /// The operator path is the same door, and the log says which route the
    /// fact travelled.
    ///
    /// An owner recording a bounce read out of a support ticket reconciles
    /// exactly as a provider bridge does — same call, same invariants — and the
    /// attempt log carries `operator` so a review can tell the two apart. The
    /// suppression cause is asserted, because that is the consequence an owner
    /// is taking on when they record it by hand.
    #[actix_web::test]
    async fn an_operator_can_record_a_receipt_by_hand_through_the_same_door() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, recorded) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                json!({
                    "act_ref": act_ref,
                    "provider": "kapso",
                    "provider_message_id": "wa-77",
                    "identity": "<Dead@Example.Test>",
                    "state": "hard_bounce",
                    "observed_at": "2026-08-21T10:00:00Z",
                    "payload_ref": "ticket://support/4182",
                    "source": "operator",
                }),
            ),
        )
        .await;
        assert_eq!(status, 200, "{recorded}");
        assert_eq!(recorded["source"], "operator");
        assert_eq!(recorded["identity"], "dead@example.test");
        assert_eq!(recorded["identity_state"], "hard_bounce");
        assert_eq!(recorded["suppression_cause"], "hard_bounce");
        assert_eq!(
            recorded["act_reached"], false,
            "a hard bounce is not an arrival"
        );

        let (status, state) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(state["attempts"][0]["source"], "operator");
        assert_eq!(state["attempts"][0]["actor"], loopback_scope().0);
        assert_eq!(state["attempts"][0]["payload_ref"], "ticket://support/4182");
        assert_eq!(state["attempts"][0]["provider"], "kapso");
    }

    /// A refused attempt is still on the log, naming who made it.
    ///
    /// The door's failures have to be reviewable. A second identity presented
    /// under a provider message id already bound to somebody else is the shape
    /// of an attempt to suppress the wrong person: it is refused, the victim is
    /// untouched, and the attempt is visible.
    #[actix_web::test]
    async fn a_refused_attempt_is_recorded_and_the_wrong_identity_is_untouched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, first) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "delivered", "pm-1", "real@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{first}");

        let (status, refused) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "complained", "pm-1", "victim@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 400, "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .expect("an error sentence")
                .contains("one identity's story"),
            "{refused}"
        );

        let (status, state) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(
            state["observation_count"], 1,
            "the refusal recorded no receipt"
        );
        assert_eq!(state["attempt_count"], 2, "but the attempt is on the log");
        assert_eq!(state["attempts"][1]["identity"], "victim@example.test");
        assert_eq!(state["attempts"][1]["state"], "complained");

        let (status, screened) = call_at_boundary(
            data,
            loopback_scoped(
                actix_test::TestRequest::get()
                    .uri("/suppressions/check?identity=victim%40example.test"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{screened}");
        assert_eq!(
            screened["sendable"], true,
            "the victim was never suppressed"
        );
    }

    /// An unnamed state or source is refused rather than defaulted.
    ///
    /// Both defaults that would be convenient here are wrong in the same
    /// direction: `accepted` would let a broken integration look acknowledged,
    /// and `provider` would file a person's transcription as a provider's own
    /// report. Their absence is pinned, including the omitted-field case, which
    /// must be a deserialisation failure rather than a silent default.
    #[actix_web::test]
    async fn an_unnamed_state_or_source_is_refused_never_defaulted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (status, unknown_state) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "bounced", "pm-1", "them@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 400, "{unknown_state}");
        let sentence = unknown_state["error"].as_str().expect("an error sentence");
        assert!(sentence.contains("not a delivery state"), "{sentence}");
        for known in ["soft_bounce", "hard_bounce", "complained", "delivered"] {
            assert!(sentence.contains(known), "{sentence} is missing {known}");
        }

        let mut wrong_source = receipt_body(&act_ref, "delivered", "pm-1", "them@example.test");
        wrong_source["source"] = json!("webhook");
        let (status, unknown_source) = call_at_boundary(
            data.clone(),
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts"))
                .set_json(wrong_source),
        )
        .await;
        assert_eq!(status, 400, "{unknown_source}");
        assert!(
            unknown_source["error"]
                .as_str()
                .expect("an error sentence")
                .contains("not a receipt source"),
            "{unknown_source}"
        );

        // Omitting a field entirely, and naming the scope in the body, are both
        // deserialisation failures rather than silently honoured requests.
        for body in [
            json!({
                "act_ref": act_ref,
                "provider": "agentmail",
                "provider_message_id": "pm-1",
                "identity": "them@example.test",
                "state": "delivered",
                "observed_at": "2026-08-21T10:00:00Z",
                "payload_ref": "payload://pm-1",
            }),
            json!({
                "act_ref": act_ref,
                "provider": "agentmail",
                "provider_message_id": "pm-1",
                "identity": "them@example.test",
                "state": "delivered",
                "observed_at": "2026-08-21T10:00:00Z",
                "payload_ref": "payload://pm-1",
                "source": "provider",
                "principal": "somebody-else",
            }),
        ] {
            let status = status_at_boundary(
                data.clone(),
                loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts"))
                    .set_json(body),
            )
            .await;
            assert_eq!(
                status, 400,
                "an omitted field and a body-named scope are both refused at the extractor"
            );
        }

        // Nothing landed through any of the four attempts.
        let (status, state) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::get().uri(&format!(
                "/delivery/receipts?act_ref={}",
                urlencoding::encode(&act_ref)
            ))),
        )
        .await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(state["knowledge"], "dispatch_unknown");
        assert_eq!(state["attempt_count"], 0);
    }

    /// An unreadable delivery store answers 503, never a 200 saying the receipt
    /// was recorded.
    ///
    /// The fail-open that would matter most on a write: a receipt reported as
    /// recorded and never written means a hard bounce nobody suppresses, and the
    /// caller has no reason to send it again.
    #[actix_web::test]
    async fn an_unreadable_delivery_store_answers_503_not_a_recorded_receipt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = api(&dir);
        let act_ref = seed_loopback_dispatch(&data, "mail-1", Utc::now() - Duration::hours(9));

        let (principal, workspace) = loopback_scope();
        let log = data
            .workspace()
            .scope_root(principal, workspace)
            .join("delivery")
            .join("dispatched.jsonl");
        std::fs::remove_file(&log).expect("remove the dispatch log");
        std::fs::create_dir_all(&log).expect("a directory in its place");

        let (status, body) = call_at_boundary(
            data,
            loopback_scoped(actix_test::TestRequest::post().uri("/delivery/receipts")).set_json(
                receipt_body(&act_ref, "hard_bounce", "pm-1", "dead@example.test"),
            ),
        )
        .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["code"], "delivery_receipt_store_unavailable");
    }

    /// The watch read tells an operator whether reconciliation is HAPPENING,
    /// not merely whether anything is overdue.
    ///
    /// Pins the gap tier 4 closes at the surface. `overdue` alone cannot
    /// distinguish *"receipts are arriving and acts are settling"* from
    /// *"nothing has ever pulled a receipt, and these acts simply have not aged
    /// past the grace period yet"* — and the second is the state this whole
    /// deployment was in. So the snapshot carries `receipts_state` beside the
    /// silence numbers, and `no_source` — not `idle` — is what an unwired
    /// process reports, with the sentence that says what it costs.
    #[actix_web::test]
    async fn the_watch_read_says_whether_anything_is_pulling_receipts_at_all() {
        use magician::config::DeliveryHygieneConfig;

        let dir = tempfile::tempdir().expect("temp dir");
        let health = SuppressionSweepHealth::new(&DeliveryHygieneConfig::default());
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(api(&dir))
                .app_data(web::Data::new(health))
                .configure(configure_suppression_routes),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/delivery/watch")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = actix_test::read_body_json(response).await;

        // The three states are three fields, and an operator can read each
        // without the others.
        assert_eq!(body["receipts_state"], "no_source", "{body}");
        assert_eq!(body["receipts_source"], serde_json::Value::Null, "{body}");
        assert!(
            body["receipts_last_error"]
                .as_str()
                .expect("a stated reason")
                .contains("dispatch_unknown"),
            "an unwired intake must say what it costs, not report a cheerful zero: {body}"
        );

        // The number that answers "is reconciliation actually happening", on
        // this tick and over the life of the process.
        assert_eq!(body["acts_left_dispatch_unknown"], 0, "{body}");
        assert_eq!(body["acts_left_dispatch_unknown_total"], 0, "{body}");
        assert_eq!(body["disclosures_advanced"], 0, "{body}");

        // And the other half of the same question, unchanged: how many
        // dispatched acts are still unacknowledged past the window.
        assert_eq!(body["overdue"], 0, "{body}");
        assert!(
            body["silence_grace_hours"]
                .as_i64()
                .expect("a grace window")
                > 0,
            "the overdue count is unreadable without the window it was measured against: {body}"
        );

        // Counts, never rates: the denominators are carried, and there is no
        // reconciliation rate anywhere in the snapshot.
        assert_eq!(body["receipts_examined"], 0, "{body}");
        assert_eq!(body["receipts_offered"], 0, "{body}");
        assert_eq!(body["receipts_uncorrelated"], 0, "{body}");
        assert!(
            body.as_object()
                .expect("an object")
                .keys()
                .all(|key| !key.ends_with("_rate")),
            "no rate may appear in this snapshot: {body}"
        );
    }
}
