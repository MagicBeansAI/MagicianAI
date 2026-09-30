//! The mail door: bounces that came back as mail, turned into receipts.
//!
//! `suppression_api` mounts `POST /delivery/receipts`, which takes a receipt a
//! caller has already read and normalised — a provider, a message id, an
//! identity, a state. That door is the right shape and it needs somebody to
//! walk through it. **For email, nobody does**: no provider wired into this
//! deployment emits a delivery event stream, and no AgentMail skill exposes a
//! bounce, complaint or delivery-status surface.
//!
//! What does exist is the mail itself. A hard bounce arrives in the sending
//! mailbox as an RFC 3464 delivery status notification, and
//! `magician::magician_v2::delivery_receipts` reads it. These two routes are
//! that reader's entry points:
//!
//! - `POST /delivery/sent` — register what left, under the identifier a bounce
//!   will quote back. Without this a bounce cannot be tied to an act, and this
//!   module refuses to guess.
//! - `POST /delivery/receipts/mail` — hand in one raw message. It is parsed,
//!   correlated, and each correlated receipt goes through
//!   `ReceiptIntake::admit` — the **same** door `/delivery/receipts` uses, so
//!   there is one set of rules and not two.
//!
//! # Provider-agnostic
//!
//! Neither route knows what AgentMail is. `provider` is a parameter and
//! `raw_mail` is bytes. An inbox reader, an SMTP adapter's bounce mailbox, a
//! Kapso rail that one day emits RFC 3464, and an owner pasting a forwarded
//! bounce all use the same two calls.
//!
//! # Why a separate file rather than more of `suppression_api`
//!
//! That file is already the register plus two delivery reads plus the receipt
//! door. This is a different subject — reading mail — with its own request
//! shapes, and it is being added while the neighbour is under active edit.
//! Splitting it costs one `pub mod` line and avoids a merge that would be
//! resolved by hand over a security boundary.
//!
//! The **caller-proving** below is deliberately the same rules as
//! `suppression_api::receipt_caller`, restated rather than shared, following the
//! precedent `counterparties_api` already set in this crate. The proof itself is
//! not restated: it comes from `VerifiedRequestIdentity`, which
//! `verify_access_middleware` inserts and which is not deserializable from a
//! payload, so there is exactly one place a caller can be established and it is
//! not here.
//!
//! # Fail closed
//!
//! - An unproved request is **401**. A receipt can suppress a recipient
//!   permanently, and an unattributable suppression is one nobody can audit.
//! - A scope header disagreeing with the proved identity is **403**, never
//!   silently overridden: a client quietly writing to the wrong ledger keeps
//!   doing it, because nothing ever tells it otherwise.
//! - Mail that is **not** a delivery report answers `200` with
//!   `recognised: false`. That is the expected answer for almost everything in
//!   an inbox and must be cheap, not an error.
//! - Mail that **is** a delivery report and cannot be read answers **422**. It
//!   is a bounce nobody could read, which is a finding, not a non-event.
//! - A store fault is **503**. A receipt that could not be recorded must never
//!   come back as one that was.
//!
//! # Counts, never rates
//!
//! Responses carry whole numbers and `recipients_reported` beside them. There
//! is no `correlation_rate`: "80% correlated" over five bounces and over five
//! thousand are different facts, and the first is what a broken send-side
//! registration looks like in its first hour.

use actix_web::{http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::suppression_api::SuppressionApi;
use crate::web_api::api_error_response;
use magician::magician_v2::agents::outward_gate::DispatchLog;
use magician::magician_v2::artifact_v2::service::ArtifactV2Error;
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::delivery::intake::{IntakeAttribution, ReceiptIntake, ReceiptSource};
use magician::magician_v2::delivery::{DeliveryScope, Reconciliation};
use magician::magician_v2::delivery_receipts::{
    admit_mail, open_local_sent_index, read_mail, MailReading, SendIdentifier, SentMessage,
};

/// The largest raw message this surface accepts, mirroring the reader's own
/// bound. Stated here too so an oversized body is refused before it is copied
/// into the parse.
const MAX_RAW_MAIL_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------
// Who is at the door
// ---------------------------------------------------------------------------

/// The caller, as the outer boundary proved them.
struct MailCaller {
    /// Whose ledger this lands in, and who is recorded as having presented it.
    principal: String,
    workspace: String,
    authentication: &'static str,
}

/// How the boundary proved the caller, as a token an auditor can read.
///
/// A match rather than a `Debug` rendering: a new authentication class fails to
/// compile here rather than reaching the attempt log as a string nobody chose.
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

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

/// Establish who is at the door, and refuse everything else.
///
/// The rules, and each one's failure:
///
/// - **No proved identity → 401.** An anonymous caller has nobody to record as
///   having presented a bounce, and a hard bounce becomes a suppression only an
///   explicit owner act can lift.
/// - **A scope header that disagrees with the proof → 403.** Letting the proof
///   silently win would be just as safe and would hide a client that believes
///   it is writing somewhere else.
/// - **No workspace, for an identity not bound to one → 400.** A defaulted
///   workspace files a bounce against a ledger nobody chose.
fn mail_caller(req: &HttpRequest) -> Result<MailCaller, HttpResponse> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            api_error_response(
                StatusCode::UNAUTHORIZED,
                "delivery_mail_actor_unproved",
                "this request was not proved by the outer boundary, so there is nobody to record \
                 as having presented this mail. A bounce read out of it can suppress a recipient \
                 permanently, and an unattributable suppression is one nobody can audit \
                 afterwards",
                None,
            )
        })?;

    if header_value(req, "X-Principal")
        .as_deref()
        .is_some_and(|asserted| asserted != identity.principal())
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "delivery_mail_scope_mismatch",
            "the principal named on this request is not the one the outer boundary proved. The \
             scope is taken from the proved identity and never from a header, because a caller \
             that could choose the principal could choose whose recipients to suppress",
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
                    "delivery_mail_scope_mismatch",
                    "the workspace named on this request is not the one this credential is bound \
                     to. A bounce filed into a workspace the caller was not proved for is a \
                     suppression signal raised in somebody else's register",
                    None,
                ));
                }
                bound.to_owned()
            },
            None => match asserted_workspace {
                Some(workspace) => workspace,
                None => return Err(bad_request(
                    "name the workspace this mail belongs to. This identity is not bound to one, \
                     and defaulting would file a bounce against a ledger nobody chose",
                )),
            },
        };

    Ok(MailCaller {
        principal: identity.principal().to_string(),
        workspace,
        authentication: authentication_label(identity.authentication()),
    })
}

/// Map a failure onto a status without flattening the two kinds.
///
/// An `ArtifactV2Error` in the chain is the store failing: **503**. Everything
/// else is a module refusing the request, which is **400** carrying that
/// module's own sentence, because the sentence says what to do instead.
fn mail_failure(doing: &str, error: anyhow::Error) -> HttpResponse {
    let store_fault = error.chain().any(|cause| cause.is::<ArtifactV2Error>());
    if store_fault {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_mail_store_unavailable",
            format!(
                "the delivery store could not be reached while {doing}, and a receipt that could \
                 not be recorded is never one that was: {error:#}"
            ),
            None,
        );
    }
    api_error_response(
        StatusCode::BAD_REQUEST,
        "delivery_mail_refused",
        format!("{error:#}"),
        None,
    )
}

fn disposition_label(disposition: Reconciliation) -> &'static str {
    match disposition {
        Reconciliation::Opened => "opened",
        Reconciliation::Replayed => "replayed",
        Reconciliation::Advanced { .. } => "advanced",
        Reconciliation::Superseded { .. } => "superseded",
    }
}

// ---------------------------------------------------------------------------
// Registering what left
// ---------------------------------------------------------------------------

/// `POST /api/magician/v2/delivery/sent`
///
/// `principal` and `workspace` are absent on purpose: they are the boundary's,
/// and `deny_unknown_fields` means a caller that sends them is refused rather
/// than silently ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterSentMessageBody {
    /// The outward act this message carried.
    pub act_ref: String,
    /// Which rail sent it — `agentmail`, `kapso`, an SMTP relay. An open
    /// string: a list here would be the thing that had to be edited for the
    /// second rail.
    pub provider: String,
    /// The RFC 5322 `Message-ID` of the message, with or without its angle
    /// brackets. Optional, but one of this and `envelope_id` is required.
    #[serde(default)]
    pub message_id: Option<String>,
    /// The RFC 3461 envelope id set at submission, when the rail sets one.
    #[serde(default)]
    pub envelope_id: Option<String>,
    /// Every address the message was addressed to. Required and non-empty: it
    /// is what a reported bounce address is checked against, and without it a
    /// report about anybody at all would be accepted.
    pub audience: Vec<String>,
    /// When it left.
    pub sent_at: DateTime<Utc>,
}

/// `POST /api/magician/v2/delivery/sent`
///
/// Register a message under the identifier a bounce will quote back at us.
///
/// # Why this is a separate act from dispatching
///
/// The dispatch log records *that* an act left. This records *how a report will
/// name it*. They are written by different things at different moments — a rail
/// may not know its own message id until the provider answers — and folding
/// them into one call would force whichever knew less to guess.
///
/// This route does **not** require the act to be in the dispatch log. That check
/// belongs at the moment it matters and is enforced there: `ReceiptIntake::admit`
/// refuses a receipt for an act this scope never recorded as dispatched. Making
/// it a precondition here would break the natural ordering, in which the
/// registration can legitimately be written before or after the dispatch record.
/// Whether the act is on the log today is reported in the response rather than
/// inferred.
///
/// Idempotent: the identical registration again is one row. A different act, a
/// different audience or a different instant under one identifier is refused —
/// two accounts of one message would decide which act a later bounce is
/// attributed to.
pub async fn register_sent_message_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    body: web::Json<RegisterSentMessageBody>,
) -> HttpResponse {
    let caller = match mail_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let body = body.into_inner();

    let mut identifiers = Vec::new();
    if let Some(message_id) = body
        .message_id
        .as_deref()
        .map(magician::magician_v2::delivery_receipts::dsn::unwrap_message_id)
        .filter(|value| !value.is_empty())
    {
        identifiers.push(SendIdentifier::MessageId(message_id));
    }
    if let Some(envelope_id) = body
        .envelope_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        identifiers.push(SendIdentifier::EnvelopeId(envelope_id.to_string()));
    }
    if identifiers.is_empty() {
        return bad_request(
            "name a `message_id`, an `envelope_id`, or both. A send registered under no \
             identifier can never have a bounce attributed to it, and this endpoint would then \
             record nothing while appearing to succeed",
        );
    }

    let workspace_layout = api.workspace().clone();
    let scope = DeliveryScope::new(caller.principal.clone(), caller.workspace.clone());
    let index = open_local_sent_index(workspace_layout.clone());
    let sent = SentMessage {
        act_ref: body.act_ref.clone(),
        audience: body.audience.clone(),
        sent_at: body.sent_at,
    };
    let now = Utc::now();

    for identifier in &identifiers {
        if let Err(error) = index.record(&scope, &body.provider, identifier, &sent, now) {
            return mail_failure(
                &format!("registering {} `{}`", identifier.kind(), identifier.value()),
                error,
            );
        }
    }

    // Reported, never enforced here — see the doc comment. `false` is the
    // finding: a registration for an act nothing dispatched will have every
    // receipt refused by the intake later, and the caller should learn that now
    // rather than when the bounce arrives.
    let dispatch_recorded = match DispatchLog::new(workspace_layout).dispatched(&scope) {
        Ok(dispatched) => dispatched
            .iter()
            .any(|candidate| candidate.act_ref == body.act_ref),
        Err(error) => return mail_failure("reading what this scope dispatched", error),
    };

    HttpResponse::Ok().json(json!({
        "principal": caller.principal,
        "workspace": caller.workspace,
        "act_ref": body.act_ref,
        "provider": body.provider,
        "registered": identifiers
            .iter()
            .map(|identifier| json!({
                "kind": identifier.kind(),
                "value": identifier.value(),
            }))
            .collect::<Vec<_>>(),
        "audience_size": body.audience.len(),
        "dispatch_recorded": dispatch_recorded,
        "recorded_at": now.to_rfc3339(),
    }))
}

// ---------------------------------------------------------------------------
// Reading a bounce
// ---------------------------------------------------------------------------

/// `POST /api/magician/v2/delivery/receipts/mail`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryMailBody {
    /// Which rail the original message left on. The bounce is looked up against
    /// that rail's registrations.
    pub provider: String,
    /// A ref an auditor follows back to this exact message — an inbox message
    /// id, a ticket, a file. Required: a receipt nobody can go back and read is
    /// an assertion, and the first person to doubt a suppression raised from it
    /// will remove it.
    pub payload_ref: String,
    /// The raw RFC 5322 message, headers and all. Not a body extract: the
    /// structure *is* the evidence, and a client that pastes only the human
    /// text has thrown away everything this reader uses.
    pub raw_mail: String,
    /// `provider` or `operator`. **Required.** A bridge that fetched this from
    /// the mailbox is `provider`; a person forwarding one out of their own
    /// client is `operator`. Defaulting would file a person's transcription as
    /// a provider's own report.
    pub source: String,
    /// Read and report without recording anything. The form to run before
    /// letting a bounce suppress somebody.
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/magician/v2/delivery/receipts/mail`
///
/// Hand in one message. If it is a delivery status notification, every
/// recipient it reports that can be correlated to a send this scope registered
/// becomes a receipt, and each receipt goes through `ReceiptIntake::admit` — the
/// same door a webhook-shaped receipt would use.
///
/// # What comes back, and what it means
///
/// - `recognised: false` — ordinary mail. Expected, and not an error.
/// - `receipts` — what the ledger decided, receipt by receipt.
/// - `refused` — every reported recipient that produced **no** receipt, with
///   the reason. This list is the point: a bounce that could not be tied to a
///   send is recorded nowhere and reported here, because attributing it by
///   guessing would suppress the wrong person permanently.
///
/// Idempotent: the same message posted twice reconciles to the same records and
/// comes back as `replayed`. The one thing that is **not** idempotent is
/// changing `payload_ref` between two posts of the same message — the ledger
/// treats a changed payload under one id as an error rather than a silent
/// no-op, so an archiver must name the message the same way twice.
pub async fn record_delivery_mail_handler(
    api: web::Data<SuppressionApi>,
    req: HttpRequest,
    body: web::Json<DeliveryMailBody>,
) -> HttpResponse {
    let caller = match mail_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let body = body.into_inner();

    if body.raw_mail.len() > MAX_RAW_MAIL_BYTES {
        return bad_request(format!(
            "`raw_mail` is {} bytes and this door accepts at most {MAX_RAW_MAIL_BYTES}",
            body.raw_mail.len()
        ));
    }
    let Some(source) = ReceiptSource::parse(&body.source) else {
        return bad_request(format!(
            "`{}` is not a receipt source; name {}. There is no default: filing a person's \
             forwarded bounce as a provider's own report would give the wrong answer to the one \
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
    let index = open_local_sent_index(workspace_layout.clone());

    if body.dry_run {
        let reading = match read_mail(
            &index,
            &scope,
            &body.provider,
            &body.payload_ref,
            &body.raw_mail,
        ) {
            Ok(reading) => reading,
            Err(error) => return mail_failure("reading a delivery report", error),
        };
        let harvest = match reading {
            MailReading::NotADeliveryReport { because } => {
                return not_a_report(&caller, &because, true)
            },
            MailReading::Unreadable { because } => return unreadable_report(&because),
            MailReading::Read(harvest) => harvest,
        };
        return HttpResponse::Ok().json(json!({
            "principal": caller.principal,
            "workspace": caller.workspace,
            "recognised": true,
            "dry_run": true,
            "report_message_id": harvest.report_message_id,
            "correlated_on": harvest.correlated_on.as_ref().map(|identifier| json!({
                "kind": identifier.kind(),
                "value": identifier.value(),
            })),
            "recipients_reported": harvest.recipients_reported,
            "would_record": harvest.intents.iter().map(|intent| json!({
                "act_ref": intent.act_ref,
                "identity": intent.receipt.identity,
                "state": intent.receipt.state.as_str(),
                "suppression_cause": intent
                    .receipt
                    .state
                    .suppression_cause()
                    .map(|cause| cause.as_str()),
                "correlation": intent.correlation.as_str(),
                "correlation_confidence": intent.correlation.confidence(),
                "observed_at": intent.receipt.observed_at.to_rfc3339(),
                "diagnostic_code": intent.diagnostic_code,
            })).collect::<Vec<_>>(),
            "refused": harvest.refused,
        }));
    }

    // The candidate list is supplied, never discovered by the ledger. A store
    // fault here is a 503 rather than an empty list, because an empty list
    // would refuse every receipt with the wrong reason.
    let dispatched = match DispatchLog::new(workspace_layout.clone()).dispatched(&scope) {
        Ok(dispatched) => dispatched,
        Err(error) => return mail_failure("reading what this scope dispatched", error),
    };

    let intake = ReceiptIntake::new(workspace_layout);
    let reading = match admit_mail(
        &intake,
        &index,
        &scope,
        &dispatched,
        &body.provider,
        &body.payload_ref,
        &body.raw_mail,
        &IntakeAttribution {
            source,
            actor: caller.principal.clone(),
            authentication: caller.authentication.to_string(),
        },
        Utc::now(),
    ) {
        Ok(reading) => reading,
        Err(error) => return mail_failure("recording the receipts in a delivery report", error),
    };

    let ingest = match reading {
        MailReading::NotADeliveryReport { because } => {
            return not_a_report(&caller, &because, false)
        },
        MailReading::Unreadable { because } => return unreadable_report(&because),
        MailReading::Read(ingest) => ingest,
    };

    HttpResponse::Ok().json(json!({
        "principal": caller.principal,
        "workspace": caller.workspace,
        "recognised": true,
        "dry_run": false,
        "report_message_id": ingest.report_message_id,
        "correlated_on": ingest.correlated_on.as_ref().map(|identifier| json!({
            "kind": identifier.kind(),
            "value": identifier.value(),
        })),
        "recipients_reported": ingest.recipients_reported,
        "receipts": ingest.admitted.iter().map(|admitted| json!({
            "act_ref": admitted.outcome.observation.act_ref,
            "receipt_id": admitted.outcome.observation.receipt_id,
            "attempt_id": admitted.attempt.attempt_id,
            "identity": admitted.outcome.observation.identity,
            "disposition": disposition_label(admitted.outcome.disposition),
            "identity_state": admitted.outcome.identity_state.as_str(),
            "act_state": admitted.outcome.act_state.as_str(),
            // Read off `is_arrival`, never off "no bad news".
            "act_reached": admitted.outcome.act_state.is_arrival(),
            "suppression_cause": admitted
                .outcome
                .identity_state
                .suppression_cause()
                .map(|cause| cause.as_str()),
            "observed_at": admitted.outcome.observation.observed_at.to_rfc3339(),
            "recorded_at": admitted.outcome.observation.recorded_at.to_rfc3339(),
        })).collect::<Vec<_>>(),
        "recorded": ingest.admitted.len(),
        "refused": ingest.refused,
        "recorded_by": caller.principal,
        "authentication": caller.authentication,
    }))
}

/// Ordinary mail. `200`, because a poller feeding a whole inbox through this
/// door must find this cheap, and because "this is not a bounce" is an answer
/// rather than a fault.
fn not_a_report(caller: &MailCaller, because: &str, dry_run: bool) -> HttpResponse {
    HttpResponse::Ok().json(json!({
        "principal": caller.principal,
        "workspace": caller.workspace,
        "recognised": false,
        "dry_run": dry_run,
        "because": because,
        "recipients_reported": 0,
        "receipts": Vec::<serde_json::Value>::new(),
        "recorded": 0,
        "refused": Vec::<serde_json::Value>::new(),
    }))
}

/// A bounce nobody could read. **422**, never 200.
///
/// This is a finding, not a non-event: an unreadable delivery report is a
/// message whose recipient may be dead and whose evidence was thrown away, and
/// answering `200 recognised: false` would file it with the ordinary mail and
/// lose it.
fn unreadable_report(because: &str) -> HttpResponse {
    api_error_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        "delivery_report_unreadable",
        format!(
            "this message announces itself as a delivery report and could not be read, so no \
             receipt was recorded and the bounce it may carry is unaccounted for: {because}"
        ),
        None,
    )
}

/// Mount the two mail routes.
///
/// Registered beside `suppression_api`'s `/delivery/*` routes on the same
/// scope, so they share the mount, the middleware and the app state that
/// already exist. `SuppressionApi` is reused as the app-state handle for
/// exactly that reason — it is a workspace handle, it is already attached to
/// this scope, and a second identical handle would be a second thing to keep in
/// step.
pub fn configure_delivery_mail_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/delivery/sent",
        web::post().to(register_sent_message_handler),
    )
    .route(
        "/delivery/receipts/mail",
        web::post().to(record_delivery_mail_handler),
    );
}
