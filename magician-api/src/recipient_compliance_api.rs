//! Owner-facing HTTP surface for the four below-the-agent recipient checks.
//!
//! `magician::magician_v2::recipient_compliance` is one gate with four rules —
//! a duplicate contact by another work, a reply already waiting on us, a
//! jurisdiction port, and a recorded erasure request. Three of the four read
//! registers that already have writers. The fourth does not: **nothing anywhere
//! records that somebody asked to be forgotten**, so without this file rule 4
//! would be a guard over a store nothing ever writes to — built, tested, and
//! reachable from nothing, which is the failure the module was written to
//! avoid rather than to repeat.
//!
//! Four acts:
//!
//! - `POST /recipient-compliance/erasure-requests` — record that somebody asked
//!   to be forgotten. **The one writer in this subsystem**, and the reason this
//!   file exists.
//! - `GET  /recipient-compliance/erasure-requests` — every request on file,
//!   with counts.
//! - `GET  /recipient-compliance/erasure-requests/proposal` — what a deletion
//!   for one identity would reach, what is retained and why, and what is out of
//!   reach entirely. **Proposes; deletes nothing.**
//! - `GET  /recipient-compliance/check` — the same four-rule decision the
//!   send-time gate makes, answered by the **same** `screen` call.
//!
//! # There is no DELETE, and no route that lifts a refusal
//!
//! Not an omission. A recorded erasure request refuses contact permanently, and
//! the request row **is** the evidence that refusal rests on — so a route that
//! removed it in the name of honouring it would resurrect the contact, and the
//! person who asked to be forgotten would be the first one reached again. That
//! is the rule `suppression` already enforces about its own rows (*"stop a
//! suppression applying — the only way, and never by deletion"*), and this
//! surface adds nothing that softens it: no DELETE, no `force`, no
//! `mark_fulfilled` that re-permits.
//!
//! The proposal route is where "what does the request actually reach" is
//! answered, and it answers by **listing**, never by purging. Deciding to
//! delete is an owner's act with consequences no route should take on their
//! behalf.
//!
//! # The check answers with the same call the gate makes
//!
//! `GET /recipient-compliance/check` runs
//! `recipient_compliance::screen` — deliberately not a second opinion of its
//! own. A surface that derived the answer another way could say *"clear"* about
//! somebody the gate refuses, or the reverse, and an owner would have no way to
//! tell which of the two was lying.
//!
//! It installs `NoJurisdictionRule`, exactly as the send-time gate does today,
//! so rule 3 comes back `not_assessed` on both surfaces rather than one of them
//! quietly claiming a jurisdiction cleared the send. **Owed, and stated here so
//! nobody rediscovers it:** a deployment that installs a real jurisdiction rule
//! must supply it to both call sites *and* give this route the channel the act
//! would travel on, because `JurisdictionQuery::channel` is `None` here and a
//! rule that turns on the channel would then answer differently on the two
//! surfaces.
//!
//! # Fail closed
//!
//! A store fault never reads as an empty answer — the module's contract is
//! that an unconsultable register is never an empty one, and a surface that
//! flattened that into *"nothing found"* would hand an owner exactly the false
//! confidence the missing store did. It surfaces in **two** shapes, and both
//! are rendered, because only one of them is an HTTP error:
//!
//! * A fault that reaches a handler as an `Err` is **503**. Store faults and
//!   caller faults are told apart by the error chain: an `ArtifactV2Error`
//!   (the file would not open) **or** a `serde_json::Error` (it opened and
//!   would not fold) is the store failing, everything else is the request. See
//!   `compliance_failure` for why the second one has to be named too — a
//!   corrupt register otherwise answers *"your request is invalid"*.
//! * `GET /recipient-compliance/check` is different, and deliberately so:
//!   `screen` does **not** fail on a register one of its rules could not read.
//!   It folds that rule's fault into `RuleVerdict::Unreadable` and returns a
//!   decision, so the route answers **200** with `clear: false`. That is still
//!   fail-closed — `is_clear` is false for `Unreadable` — but it is not a 503,
//!   and a reader expecting one would conclude the stores were fine.
//!
//! The check's verdict is read off each rule's own arm, never off *"no
//! refusals"*: `not_assessed` is rendered as `not_assessed`, and
//! `rules_assessed` is reported beside `clear` at the top level, so a decision
//! where three of four rules could not be applied can never be read as a
//! decision where four rules cleared. On this surface that is the **normal**
//! answer, not an edge case: `NoJurisdictionRule` leaves rule 3 unassessed on
//! every single call, so `clear: true` here always means "three of four".
//!
//! `unreadable_rules` is reported beside `refusing_rules` for the same reason.
//! `refusing_rules` is `RuleVerdict::refuses`, which is true of `Refused` and
//! `Unreadable` alike, so the summary alone cannot tell *"they are off limits"*
//! from *"we could not check"* — the one distinction `RuleVerdict` exists to
//! keep, and the one that decides whether the person reading this has anything
//! to repair.
//!
//! # Authentication, which is the whole risk of the write
//!
//! `POST /recipient-compliance/erasure-requests` is a **remote permanent
//! suppression primitive**: one call silences a real recipient for good, and
//! there is no lift. It therefore requires the `VerifiedRequestIdentity` the
//! access middleware attaches after Cloudflare Access, a paired device or a
//! real loopback peer — the same gate `POST /delivery/receipts` uses, and for
//! the same reason. The **principal** is taken from the proved identity and
//! never from a header: a caller that could pick the principal could pick whose
//! recipients to silence.
//!
//! The **workspace** is the identity's whenever the credential is bound to one
//! — paired devices and the local single-user fallback are. Interactive Access
//! identities are deliberately not bound (see
//! `VerifiedRequestIdentity::workspace`), and for those the workspace is taken
//! from `X-Workspace` and checked only for being present. Stated plainly rather
//! than glossed as *"never from a header"*: such a caller does choose which of
//! **their own** registers this permanent refusal lands in. What they cannot do
//! is reach another principal's, because the scope root is keyed on the
//! principal the proof fixed. A header that **disagrees** with a bound identity
//! is a 403, never a silent override.
//!
//! `evidence_ref` and `recorded_by` are required with no default and no
//! inference, mirroring the `authority` rule on `POST /suppressions/lift`. A
//! request nobody can check is an assertion rather than a record, and this one
//! refuses contact forever.

use std::collections::BTreeSet;

use actix_web::{http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::artifact_v2::service::ArtifactV2Error;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::evidence::OutwardAssertionStore;
use magician::magician_v2::recipient_compliance::{
    erasure_proposal, CompliancePolicy, ComplianceRule, ComplianceScope, ErasureRequestLog,
    NoJurisdictionRule, RecipientComplianceDecision, Refusal, RuleVerdict,
    DUPLICATE_CONTACT_WINDOW_DAYS,
};
use magician::magician_v2::suppression::SuppressionRegister;
use magician::magician_v2::work_context::WorkContextKind;

/// The app-state this surface needs: somewhere to open the owner's records.
#[derive(Clone)]
pub struct RecipientComplianceApi {
    workspace_layout: ArtifactV2Workspace,
}

impl RecipientComplianceApi {
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

/// Map a failure onto a status without flattening the two kinds.
///
/// Two shapes in the chain mean the store itself could not be read or written,
/// and both are **503**, because *"we could not check"* is not a caller fault
/// and must never read like an answer:
///
/// * an `ArtifactV2Error` — the file could not be opened, read or written; and
/// * a `serde_json::Error` — the log opened but did not fold.
///   `jsonl::parse_log_lines` refuses a corrupt or fused line by returning the
///   `serde_json::Error` under a context string, with **no** `ArtifactV2Error`
///   anywhere in the chain. Checking only for the latter would answer a corrupt
///   erasure register with *"your request is invalid"*, which tells an owner to
///   go and fix their client while the register nobody can read keeps holding
///   the refusals.
///
/// A `serde_json::Error` here can only be the store's: the request body is
/// decoded by `web::Json` and the query string by `web::Query`, both of which
/// refuse before a handler runs, so a caller's malformed payload never reaches
/// this function.
///
/// Everything else is the module refusing the request — a malformed identity,
/// blank evidence, a replay whose payload changed, an identity nobody filed a
/// request for — which is **400**, and the module's own sentence is the body,
/// because it says what to do instead.
fn compliance_failure(doing: &str, error: anyhow::Error) -> HttpResponse {
    let store_fault = error
        .chain()
        .any(|cause| cause.is::<ArtifactV2Error>() || cause.is::<serde_json::Error>());
    if store_fault {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "recipient_compliance_unreadable",
            format!(
                "the recipient-compliance records could not be reached while {doing}, and an \
                 unreachable register is never an empty one: {error:#}"
            ),
            None,
        );
    }
    api_error_response(
        StatusCode::BAD_REQUEST,
        "recipient_compliance_refused",
        format!("{error:#}"),
        None,
    )
}

// ---------------------------------------------------------------------------
// Who is at the door
// ---------------------------------------------------------------------------

/// How the boundary proved the caller, as a token an auditor can read.
///
/// A match rather than a `Debug` rendering: a new authentication class will
/// fail to compile here rather than reaching the record as a string nobody
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

/// Whose records this write lands in, and how they were proved.
struct ErasureCaller {
    principal: String,
    workspace: String,
    authentication: &'static str,
}

/// Establish who is at the door, and refuse everything else.
///
/// `VerifiedRequestIdentity` is inserted by the access middleware only after
/// Cloudflare Access, a paired device or a real loopback peer has been
/// verified, and it is deliberately not deserializable from a payload — so it
/// is the one thing on the request a caller cannot write.
///
/// A scope header that **disagrees** with the proved identity is a `403` rather
/// than being quietly overridden: letting the identity silently win would be
/// just as safe and would also hide a client that believes it is writing
/// somewhere else, which keeps doing it because nothing ever says otherwise.
fn erasure_caller(req: &HttpRequest) -> Result<ErasureCaller, HttpResponse> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            api_error_response(
                StatusCode::UNAUTHORIZED,
                "erasure_request_actor_unproved",
                "this request was not proved by the outer boundary, so there is nobody to record \
                 as having filed this erasure request. A recorded request refuses contact \
                 permanently and nothing here lifts it, so an unproved caller is refused rather \
                 than recorded anonymously",
                None,
            )
        })?;

    if header_value(req, "X-Principal")
        .as_deref()
        .is_some_and(|asserted| asserted != identity.principal())
    {
        return Err(api_error_response(
            StatusCode::FORBIDDEN,
            "erasure_request_scope_mismatch",
            "the principal named on this request is not the one the outer boundary proved. The \
             scope an erasure request is filed under is taken from the proved identity and never \
             from a header, because a caller that could choose the principal could choose whose \
             recipients to silence",
            None,
        ));
    }

    let asserted_workspace = header_value(req, "X-Workspace");
    let workspace = match identity.workspace() {
        Some(bound) => {
            if asserted_workspace
                .as_deref()
                .is_some_and(|asserted| asserted != bound)
            {
                return Err(api_error_response(
                    StatusCode::FORBIDDEN,
                    "erasure_request_scope_mismatch",
                    "the workspace named on this request is not the one this credential is bound \
                     to. A request filed into a workspace the caller was not proved for refuses \
                     contact in somebody else's register",
                    None,
                ));
            }
            bound.to_owned()
        },
        None => match asserted_workspace {
            Some(workspace) => workspace,
            None => {
                return Err(bad_request(
                    "name the workspace this erasure request belongs to. This identity is not \
                     bound to one, and defaulting would file a permanent refusal against a \
                     register nobody chose",
                ))
            },
        },
    };

    Ok(ErasureCaller {
        principal: identity.principal().to_string(),
        workspace,
        authentication: authentication_label(identity.authentication()),
    })
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RecipientComplianceScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ErasureProposalQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The address the request was filed for. A query parameter rather than a
    /// path segment because an address carries `@` and `.`, and a path segment
    /// holding them is one encoding mistake away from addressing a different
    /// route.
    pub identity: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckRecipientQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// One or more recipients, comma-separated — one outward act reaches
    /// several people, so the check has to be able to ask about several.
    ///
    /// Comma-separated rather than a repeated parameter because `web::Query`
    /// is backed by `serde_urlencoded`, which cannot deserialise a repeated key
    /// into a sequence: declaring `Vec<String>` here would reject every request
    /// at the boundary. An address cannot contain a comma, so the split is
    /// unambiguous.
    pub to: String,
    /// `program` or `engagement`. With `work_id`, answers as the send-time gate
    /// would for an act bound to that work.
    #[serde(default)]
    pub work_kind: Option<String>,
    #[serde(default)]
    pub work_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordErasureRequestBody {
    pub identity: String,
    /// When they asked — the moment of the act, supplied by the caller, never
    /// this surface's clock. A register imported late must not claim the
    /// request arrived late.
    pub requested_at: DateTime<Utc>,
    /// **Required, no default.** The ticket, message id or signed note an
    /// auditor follows.
    pub evidence_ref: String,
    /// **Required, no default.** Who filed it — the operator, the form, the
    /// support agent who read the mail.
    pub recorded_by: String,
}

/// The work a caller named, or a refusal.
///
/// Both parameters or neither. One alone is refused rather than half-honoured:
/// a `work_kind` with no id would answer for an act bound to a blank work, and
/// an id with no kind cannot be resolved at all because `program:acme` and
/// `engagement:acme` are different works.
fn work_from(
    kind: Option<&str>,
    id: Option<&str>,
) -> Result<Option<WorkContextKind>, HttpResponse> {
    match (kind, id) {
        (None, None) => Ok(None),
        (Some(kind), Some(id)) => WorkContextKind::from_token(kind, id)
            .map(Some)
            .map_err(bad_request),
        (Some(_), None) => Err(bad_request(
            "`work_kind` was given without `work_id`: the answer would be for an act bound to a \
             blank work, which is not a work any act is ever bound to",
        )),
        (None, Some(_)) => Err(bad_request(format!(
            "`work_id` was given without `work_kind`: the kind is part of the identity, so \
             `acme` alone cannot be told apart from a programme or an engagement of the same \
             name. This build knows {}",
            WorkContextKind::KIND_TOKENS.join(", ")
        ))),
    }
}

/// One rule's answer, rendered so `not_assessed` can never read as `clear`.
fn verdict_json(verdict: &RuleVerdict) -> serde_json::Value {
    match verdict {
        RuleVerdict::Clear => json!({ "verdict": "clear" }),
        RuleVerdict::NotAssessed { why } => json!({ "verdict": "not_assessed", "why": why }),
        RuleVerdict::Unreadable { cause } => json!({ "verdict": "unreadable", "cause": cause }),
        RuleVerdict::Refused(refusal) => json!({
            "verdict": "refused",
            "detail": refusal.detail(),
            "evidence": refusal_evidence_json(refusal),
        }),
    }
}

/// A refusal's typed evidence, or an explicit marker saying it could not be
/// rendered.
///
/// Never `null` on failure. A null evidence field beside a refusal reads as a
/// refusal with no evidence, which is the one thing this whole subsystem is
/// built to make impossible to say by accident.
fn refusal_evidence_json(refusal: &Refusal) -> serde_json::Value {
    match serde_json::to_value(refusal) {
        Ok(value) => value,
        Err(error) => json!({
            "evidence_unavailable": format!(
                "the refusal's evidence could not be rendered ({error}); the refusal itself \
                 stands and its sentence is in `detail`"
            )
        }),
    }
}

fn decision_json(decision: &RecipientComplianceDecision) -> serde_json::Value {
    // Computed once and read twice below — the rendering needs both the list
    // and its length, and `not_assessed_rules` walks every finding to build it.
    let not_assessed = decision.not_assessed_rules();
    // `clear` is `is_clear()`, which is the gate's own reading and must not be
    // second-guessed here. But `is_clear()` is true when a rule was never
    // applied, so the bare boolean read alone says "cleared" about a decision
    // that mostly did not decide — and on this surface that is every decision,
    // because `NoJurisdictionRule` never assesses rule 3. These two counts sit
    // beside it so the caller branching on `clear` sees the cost at the same
    // depth, without having to walk `not_assessed_rules`.
    let rules_total = decision.findings.len();
    let rules_assessed = rules_total.saturating_sub(not_assessed.len());
    json!({
        "identities_screened": decision.identities,
        "clear": decision.is_clear(),
        // Reconciles: `rules_assessed + not_assessed_rules.len() == rules_total`,
        // and `rules_total` is the length of `findings`, so a decision that
        // somehow carried fewer than four findings is visible rather than
        // implied.
        "rules_total": rules_total,
        "rules_assessed": rules_assessed,
        "refusing_rules": decision
            .refusing_rules()
            .into_iter()
            .map(ComplianceRule::as_str)
            .collect::<Vec<_>>(),
        // A **subset** of `refusing_rules`, never a fifth bucket — so the two
        // still reconcile — and named separately because `RuleVerdict` holds
        // that *"they are off limits"* and *"we could not check"* are opposite
        // facts. `refusing_rules` cannot tell them apart: it is
        // `verdict.refuses()`, which is true for `Refused` and `Unreadable`
        // alike. On this route that matters more than at the send-time gate,
        // because `screen` does not fail on a store it could not read — it
        // returns `Unreadable` inside the decision — so an unreadable outward
        // index reaches an owner here as `clear: false` with
        // `refusing_rules: ["duplicate_recipient"]`, which reads as *"another
        // work contacted this person"*. One of those is a fact about a
        // recipient and the other is a disk to repair, and only the second is
        // fixable by the person reading it.
        "unreadable_rules": decision
            .findings
            .iter()
            .filter_map(|finding| match &finding.verdict {
                RuleVerdict::Unreadable { cause } => Some(json!({
                    "rule": finding.rule.as_str(),
                    "cause": cause,
                })),
                _ => None,
            })
            .collect::<Vec<_>>(),
        "not_assessed_rules": not_assessed
            .into_iter()
            .map(|(rule, why)| json!({ "rule": rule.as_str(), "why": why }))
            .collect::<Vec<_>>(),
        "findings": decision
            .findings
            .iter()
            .map(|finding| {
                let mut body = verdict_json(&finding.verdict);
                if let Some(map) = body.as_object_mut() {
                    map.insert("rule".into(), json!(finding.rule.as_str()));
                    map.insert("examined".into(), json!(finding.examined));
                    map.insert("examined_unit".into(), json!(finding.rule.unit_examined()));
                }
                body
            })
            .collect::<Vec<_>>(),
        // Counts, never rates, and counts that reconcile: `accounted_for` is
        // reported beside the total so a reader can check it rather than
        // trusting it.
        "duplicate_scan": decision.duplicate_scan,
        "duplicate_scan_accounted_for": decision.duplicate_scan.accounted_for(),
        "reply_scan": decision.reply_scan,
        "reply_scan_accounted_for": decision.reply_scan.accounted_for(),
        "refusal_message": decision.refusal_message(),
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/magician/v2/recipient-compliance/erasure-requests`
///
/// The one writer. Idempotent per `(identity, evidence_ref)`: replaying a
/// request resumes the row already written and keeps its original
/// `requested_at`. A replay whose payload differs under the same evidence comes
/// back as a refusal, not a quiet no-op, because two accounts of one act have
/// to be reconciled by a person.
pub async fn record_erasure_request_handler(
    api: web::Data<RecipientComplianceApi>,
    req: HttpRequest,
    body: web::Json<RecordErasureRequestBody>,
) -> HttpResponse {
    let caller = match erasure_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let body = body.into_inner();
    let scope = ComplianceScope::new(caller.principal.clone(), caller.workspace.clone());
    let log = ErasureRequestLog::new(api.workspace().clone());

    match log.record(
        &scope,
        &body.identity,
        body.requested_at,
        &body.evidence_ref,
        &body.recorded_by,
    ) {
        Ok(request) => HttpResponse::Ok().json(json!({
            "principal": caller.principal,
            "workspace": caller.workspace,
            "authentication": caller.authentication,
            "request": request,
            // Said plainly on the way out, because the caller is the last
            // person who could still stop it: nothing here lifts this.
            "note": "recorded. This refuses contact with this identity permanently — there is no \
                     route that lifts it, because the row is the evidence the refusal rests on",
        })),
        Err(error) => compliance_failure("recording an erasure request", error),
    }
}

/// `GET /api/magician/v2/recipient-compliance/erasure-requests`
///
/// Every request on file, with counts. No default window, deliberately: a
/// register holding a year-old request would answer an empty list to an owner
/// asking who has asked to be forgotten, and an empty list is indistinguishable
/// from a register with nobody in it.
pub async fn list_erasure_requests_handler(
    api: web::Data<RecipientComplianceApi>,
    req: HttpRequest,
    query: web::Query<RecipientComplianceScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let scope = ComplianceScope::new(principal.clone(), workspace.clone());
    let log = ErasureRequestLog::new(api.workspace().clone());

    let identities = match log.identities(&scope) {
        Ok(identities) => identities,
        Err(error) => return compliance_failure("listing erasure identities", error),
    };
    let requests = match log.all_requests(&scope) {
        Ok(requests) => requests,
        Err(error) => return compliance_failure("listing erasure requests", error),
    };

    // The roster and the rows are two different reads, and `ErasureRequestLog`
    // deliberately writes the roster FIRST ("index before row", so a recorded
    // refusal is always enumerable). That ordering means a crash between the
    // two appends leaves a roster entry whose identity log holds no row — a
    // real state this store is built to be able to be in, not a corruption.
    //
    // Reporting only `identities_asking` and `requests_recorded` renders that
    // state as two unrelated numbers, and an owner reading "3 asked, 2 on file"
    // has nothing telling them the third is a torn write rather than an
    // arithmetic they misread. So the identities the rows actually account for
    // are counted and named. Every row folded here came out of a log the roster
    // named, so equality with `identities_asking` is the whole register
    // accounted for, and a shortfall is the roster entries whose row is not on
    // disk.
    //
    // Exceeding it is reachable and is **not** on its own a sign of corruption,
    // so it is rendered rather than ruled out: `all_requests` reads the roster
    // again for itself, and a `POST` landing between that read and the one
    // above puts a new identity in the rows while `identities_asking` still
    // holds the older count. A persistent excess across two calls is the shape
    // worth chasing — an identity log holding a row filed under a different
    // address than the file it lives in.
    let identities_with_recorded_requests: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.identity.as_str())
        .collect();

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        // Three counts, not one: "three people asked", "three requests were
        // filed" and "the rows speak for three people" are different facts, and
        // one number conflates all of them.
        "identities_asking": identities.len(),
        "identities_with_recorded_requests": identities_with_recorded_requests.len(),
        "requests_recorded": requests.len(),
        "identities": identities,
        "requests": requests,
    }))
}

/// `GET /api/magician/v2/recipient-compliance/erasure-requests/proposal`
///
/// **Proposes; deletes nothing.** What a deletion for one identity would reach,
/// what is retained and why, and what this codebase cannot speak for at all.
///
/// An identity with no recorded request is a `400`, not an empty proposal: a
/// deletion plan for somebody who never asked must not come into existence one
/// click from being executed.
pub async fn erasure_proposal_handler(
    api: web::Data<RecipientComplianceApi>,
    req: HttpRequest,
    query: web::Query<ErasureProposalQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let scope = ComplianceScope::new(principal.clone(), workspace.clone());
    let log = ErasureRequestLog::new(api.workspace().clone());
    let register = SuppressionRegister::global(api.workspace().clone());
    let outward = OutwardAssertionStore::new(api.workspace().clone());

    match erasure_proposal(&log, &register, &outward, &scope, &query.identity) {
        Ok(proposal) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "proposal": proposal,
            "index_entries_accounted_for": proposal.accounted_for(),
            "deletes_nothing": true,
        })),
        Err(error) => compliance_failure("building an erasure proposal", error),
    }
}

/// `GET /api/magician/v2/recipient-compliance/check`
///
/// The owner-facing form of the question the send-time gate asks, answered by
/// the **same** `screen` call — see the header on why it is not a second
/// opinion of its own.
pub async fn check_recipient_compliance_handler(
    api: web::Data<RecipientComplianceApi>,
    req: HttpRequest,
    query: web::Query<CheckRecipientQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    // A blank segment — a trailing comma, two commas in a row — names nobody,
    // so nothing is dropped from the check by skipping it. A segment with any
    // content is kept exactly as written and handed to the module, which
    // normalises it and refuses it if it cannot.
    let recipients: Vec<String> = query
        .to
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if recipients.is_empty() {
        // The module refuses this too. Refused here as well so the caller reads
        // a sentence about their request rather than one about the module's
        // internals: all four rules are per-identity, and a check over nobody
        // is vacuously clear while reading like a clean bill of health.
        return bad_request(
            "name at least one recipient with `to` (comma-separated for several). All four \
             recipient-compliance rules are per-identity, so a check over nobody comes back \
             clear without having decided anything — which reads exactly like a clean bill of \
             health",
        );
    }
    let work = match work_from(query.work_kind.as_deref(), query.work_id.as_deref()) {
        Ok(work) => work,
        Err(response) => return response,
    };

    let scope = ComplianceScope::new(principal.clone(), workspace.clone());
    match magician::magician_v2::recipient_compliance::screen(
        api.workspace(),
        &scope,
        &recipients,
        // No channel: this surface installs `NoJurisdictionRule`, which does
        // not read one. See the header for what a real rule would additionally
        // need here.
        None,
        work.as_ref(),
        // No act is being screened, so there is no own-record to exclude.
        None,
        &CompliancePolicy::standard(),
        &NoJurisdictionRule,
        Utc::now(),
    ) {
        Ok(decision) => {
            let mut body = decision_json(&decision);
            if let Some(map) = body.as_object_mut() {
                map.insert("principal".into(), json!(principal));
                map.insert("workspace".into(), json!(workspace));
                map.insert(
                    "work".into(),
                    match work.as_ref() {
                        Some(work) => json!(work.as_key()),
                        None => serde_json::Value::Null,
                    },
                );
                map.insert(
                    "duplicate_window_days".into(),
                    json!(DUPLICATE_CONTACT_WINDOW_DAYS),
                );
            }
            HttpResponse::Ok().json(body)
        },
        Err(error) => compliance_failure("checking recipient compliance", error),
    }
}

/// Two registrations per literal where a resource carries two methods — see
/// `configure_suppression_routes` on why `ServiceConfig::route` and not
/// `resource`.
pub fn configure_recipient_compliance_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/recipient-compliance/erasure-requests",
        web::post().to(record_erasure_request_handler),
    )
    .route(
        "/recipient-compliance/erasure-requests",
        web::get().to(list_erasure_requests_handler),
    )
    .route(
        "/recipient-compliance/erasure-requests/proposal",
        web::get().to(erasure_proposal_handler),
    )
    .route(
        "/recipient-compliance/check",
        web::get().to(check_recipient_compliance_handler),
    );
}
