//! Owner-facing HTTP surface for approval envelopes.
//!
//! Plan phase 4 of `docs/plans/2026-08-07-opc-approval-envelopes.md`, mounted.
//! [`magician::magician_v2::approval_envelopes::owner_view`] is a transport-free
//! projection built precisely so a route could sit on it; this module is that
//! route and deliberately nothing more.
//!
//! # The one rule that shapes every handler here
//!
//! **Standing is derived in the projection, and this module never re-derives
//! it.** `owner_view::summarise` decides `active` / `revoked` / `expired` /
//! `exhausted` from the same limits and the same clock the resolver reads. A
//! handler that recomputed any of that — or cached it — would be a second
//! opinion, and the owner surface could then say `active` about an envelope the
//! resolver refuses. That divergence is the exact failure `owner_view.rs` was
//! shaped to prevent, so what these handlers do with a summary is serialise it
//! and count it. Counting standings is not re-deriving them.
//!
//! # A primitive, not a programme feature
//!
//! An envelope is scoped to a goal, a program or an engagement, and this surface
//! knows nothing else about what the work is. Nothing in the vocabulary here
//! names a domain: the same routes serve an outreach programme, a hiring loop, a
//! procurement run, or anything else that wants bounded consent to a class of
//! future acts.
//!
//! # Where this surface is stricter than the store, and why that is safe
//!
//! The store refuses an expiry-less **standing** envelope. This surface refuses
//! an expiry-less envelope of *any* kind, and refuses a standing envelope with
//! no count cap. Both are narrowings: every request this module rejects is one
//! the store would have been free to reject too, and no request it accepts
//! bypasses a store check. The store stays the authority on what may be granted
//! — it is asked, every time, after these refusals have run.

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::agents::ConsequenceClass;
use magician::magician_v2::approval_envelopes::{
    preview_approval_waiver, reason_label, ApprovalContext, ApprovalEnvelope,
    ApprovalEnvelopeStore, ApprovalWaiver, BoundaryPredicate, EnvelopeGate, EnvelopeKind,
    EnvelopeLimits, EnvelopeScope, EnvelopeStanding, EnvelopeStoreScope, EnvelopeSummary,
    GrantEnvelope, OwnerView,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// The separator every derived id in this system is built from.
///
/// A caller string carrying it can fuse two id components into one, which for
/// envelopes means one scope's grant answering under another scope's key. The
/// counterparty store guards its own components against exactly this character;
/// the envelope store does not, so the guard lives here, at the only door a
/// caller-supplied string comes through.
const FIELD_SEP: char = '\u{1f}';

/// Upper bound on an id taken from a URL path.
///
/// The store's own ids are `env-` plus 32 hex characters. The cap is generous
/// rather than exact so a future id shape does not silently 400, while still
/// refusing the pathological lengths a path component should never carry.
const MAX_PATH_ID_LEN: usize = 128;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Shared state for the envelope routes.
///
/// Holds the store rather than an [`OwnerView`], because a view is bound to one
/// `(principal, workspace)` and the scope is resolved per request. Building the
/// view per request is what keeps a handler physically unable to read another
/// tenant's envelopes: the store derives every path from the scope it is handed.
#[derive(Clone)]
pub struct ApprovalEnvelopeApi {
    store: ApprovalEnvelopeStore,
}

impl ApprovalEnvelopeApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            store: ApprovalEnvelopeStore::new(workspace_layout),
        }
    }

    /// The projection for one authenticated scope.
    fn owner_view(&self, principal: &str, workspace: &str) -> OwnerView {
        OwnerView::new(
            self.store.clone(),
            EnvelopeStoreScope::new(principal, workspace),
        )
    }
}

// ---------------------------------------------------------------------------
// Guards — pure, and the reason a caller string can never reach a derivation
// ---------------------------------------------------------------------------

/// A caller string that feeds the envelope id derivation.
///
/// `derive_envelope_id` joins the principal, the workspace, the scope key, the
/// outcome and the grant instant with [`FIELD_SEP`] and hashes the result. A
/// component carrying that separator can shift the boundary between two
/// components, so `("program:a", "b\u{1f}c")` and `("program:a\u{1f}b", "c")`
/// derive one id for two different grants. The store does not check, so nothing
/// downstream will.
pub fn guard_derivation_component(label: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{label} must not be blank; a blank component derives an id nobody meant"
        ));
    }
    if trimmed.contains(FIELD_SEP) {
        return Err(format!(
            "{label} must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other, and a value carrying it could fuse two \
             grants into one id"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!("{label} must not contain control characters"));
    }
    Ok(())
}

/// An id arriving in a URL path and used to build a file path.
///
/// `ApprovalEnvelopeStore::envelope_path` interpolates the id straight into a
/// filename under the scope root, with no normalisation of its own — so an id
/// carrying a separator or a parent reference reads a file outside the scope
/// that was authenticated. Scope isolation on this surface *is* the path, and
/// this guard is what makes the path trustworthy.
pub fn guard_path_id(label: &str, value: &str) -> Result<(), String> {
    guard_derivation_component(label, value)?;
    let trimmed = value.trim();
    if trimmed.len() > MAX_PATH_ID_LEN {
        return Err(format!(
            "{label} is longer than {MAX_PATH_ID_LEN} characters and cannot be an id this store \
             ever minted"
        ));
    }
    if trimmed
        .chars()
        .any(|character| !character.is_ascii_alphanumeric() && character != '-' && character != '_')
    {
        return Err(format!(
            "{label} must be ASCII letters, digits, `-` or `_`: the id becomes a filename under \
             the authenticated scope, and a separator or a parent reference in it would read \
             outside that scope"
        ));
    }
    Ok(())
}

/// Read an envelope scope out of two query components.
///
/// An unrecognised kind is refused rather than defaulted. There is no scope this
/// surface may pick on a caller's behalf: a wrong guess lists a different piece
/// of work's authority, and the caller cannot tell because the answer looks like
/// a correct one.
pub fn parse_envelope_scope(kind: Option<&str>, id: Option<&str>) -> Result<EnvelopeScope, String> {
    let kind = kind.map(str::trim).filter(|value| !value.is_empty());
    let id = id.map(str::trim).filter(|value| !value.is_empty());
    let (Some(kind), Some(id)) = (kind, id) else {
        return Err(
            "both `scope_kind` (goal | program | engagement) and `scope_id` are required; an \
             envelope listing has no default scope, and defaulting one would show a different \
             piece of work's authority"
                .to_string(),
        );
    };
    guard_derivation_component("a scope id", id)?;
    match kind.to_ascii_lowercase().as_str() {
        "goal" => Ok(EnvelopeScope::Goal(id.to_string())),
        "program" => Ok(EnvelopeScope::Program(id.to_string())),
        "engagement" => Ok(EnvelopeScope::Engagement(id.to_string())),
        other => Err(format!(
            "`{other}` names no envelope scope; expected `goal`, `program` or `engagement`"
        )),
    }
}

/// The id inside a scope, whatever kind it is.
fn scope_id_of(scope: &EnvelopeScope) -> &str {
    match scope {
        EnvelopeScope::Goal(id) | EnvelopeScope::Program(id) | EnvelopeScope::Engagement(id) => id,
    }
}

// ---------------------------------------------------------------------------
// Grantability — the surface's own refusals, all of them narrowings
// ---------------------------------------------------------------------------

/// Whether these limits may be granted at all, before the store is asked.
///
/// Three refusals, each one a bound the caller must state rather than one this
/// surface may invent:
///
/// - **no expiry, no grant.** The store enforces this for standing envelopes
///   only, because a reviewed batch is exhausted by its own list. Over HTTP that
///   exemption is worth giving up: a batch whose instances are never all reached
///   never becomes exhausted, so an expiry-less batch is an open authority with
///   no clock on it. Defaulting an expiry here would be worse than refusing —
///   the owner would believe they chose a window they never saw.
/// - **an expiry already reached is not a window.** Expiry is inclusive
///   (`now >= expires_at` is expired), so an envelope granted at its own expiry
///   authorises nothing and would sit in the store reading `expired` from birth.
/// - **a standing envelope needs a count cap.** It is consent to acts the owner
///   has not seen; bounded only by time, it authorises an unbounded number of
///   them inside the window. A count, never a rate: `max_acts` or
///   `per_recipient_cap` answers "how many", which is the question an owner can
///   actually check afterwards.
pub fn check_grantable_limits(
    kind: &EnvelopeKind,
    limits: &EnvelopeLimits,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let Some(expires_at) = limits.expires_at else {
        return Err(
            "an envelope with no expiry is not grantable: it is consent that never lapses, and \
             this surface refuses rather than choosing a window the owner never saw"
                .to_string(),
        );
    };
    if now >= expires_at {
        return Err(format!(
            "`limits.expires_at` ({}) is already reached at {}: expiry is inclusive, so this \
             envelope would authorise nothing from the moment it was granted",
            expires_at.to_rfc3339(),
            now.to_rfc3339()
        ));
    }
    if matches!(kind, EnvelopeKind::Standing)
        && limits.max_acts.is_none()
        && limits.per_recipient_cap.is_none()
    {
        return Err(
            "a standing envelope must cap how many acts it authorises: `limits.max_acts` or \
             `limits.per_recipient_cap`. Bounded only by time it is an unbounded number of \
             unseen acts inside the window"
                .to_string(),
        );
    }
    Ok(())
}

/// What a second grant of the same outcome, in the same scope, means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantReplay {
    /// Nothing live matches; this is a new grant.
    Fresh,
    /// An identical live envelope already exists — resume it rather than minting
    /// a second one that says the same thing.
    Resumes { envelope_id: String },
    /// A live envelope claims the same outcome on different terms.
    Conflicts {
        envelope_id: String,
        differences: Vec<String>,
    },
}

/// Decide a grant against what is already live in the scope.
///
/// The store derives an envelope id from `(principal, workspace, scope, outcome,
/// grant instant)`, so two grants a millisecond apart are two envelopes even
/// when every word matches. Over HTTP the caller does not supply the instant,
/// which makes the store's own replay-resumes property unreachable from here: a
/// retried POST would mint a second envelope, and the owner's cap would quietly
/// double.
///
/// So sameness is decided on the store's own terms minus the clock — same scope,
/// same outcome — and then the payloads are compared:
///
/// - identical terms **resume** the live envelope;
/// - changed terms are a **conflict**, never a silent no-op. Returning the old
///   envelope for a new payload would tell the owner they had raised a cap they
///   had not;
/// - only `Active` envelopes are considered. A revoked, expired or exhausted one
///   is terminal, and re-granting past it must mint a new envelope rather than
///   resurrect the dead one.
pub fn classify_grant_replay(
    live: &[(EnvelopeSummary, ApprovalEnvelope)],
    request: &GrantEnvelope,
) -> GrantReplay {
    for (summary, existing) in live {
        if !summary.standing.authorises_anything() {
            continue;
        }
        if existing.scope != request.scope || existing.outcome != request.outcome {
            continue;
        }
        let differences = grant_differences(existing, request);
        return if differences.is_empty() {
            GrantReplay::Resumes {
                envelope_id: existing.envelope_id.clone(),
            }
        } else {
            GrantReplay::Conflicts {
                envelope_id: existing.envelope_id.clone(),
                differences,
            }
        };
    }
    GrantReplay::Fresh
}

/// Every term on which a request differs from a stored envelope, named.
///
/// Named rather than counted because the owner has to decide what to do about
/// it, and "this grant differs" without saying how is a dead end. `covers` and
/// `boundary` compare as sets: order in a JSON array is not a term of the grant.
fn grant_differences(existing: &ApprovalEnvelope, request: &GrantEnvelope) -> Vec<String> {
    let mut differences = Vec::new();
    if existing.kind != request.kind {
        differences.push("kind".to_string());
    }
    if !same_set(&existing.covers, &request.covers) {
        differences.push("covers".to_string());
    }
    if existing.limits != request.limits {
        differences.push("limits".to_string());
    }
    if !same_set(&existing.boundary, &request.boundary) {
        differences.push("boundary".to_string());
    }
    differences
}

fn same_set<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len()
        && left.iter().all(|item| right.contains(item))
        && right.iter().all(|item| left.contains(item))
}

/// Tally summaries by the standing the projection already derived.
///
/// Counts, never rates. "3 of 5 active" is checkable against the list below it;
/// "60% active" is a number an owner cannot reconcile with anything on screen.
pub fn standing_counts(summaries: &[EnvelopeSummary]) -> serde_json::Value {
    let tally = |wanted: EnvelopeStanding| {
        summaries
            .iter()
            .filter(|summary| summary.standing == wanted)
            .count()
    };
    json!({
        "total": summaries.len(),
        "active": tally(EnvelopeStanding::Active),
        "revoked": tally(EnvelopeStanding::Revoked),
        "expired": tally(EnvelopeStanding::Expired),
        "exhausted": tally(EnvelopeStanding::Exhausted),
    })
}

// ---------------------------------------------------------------------------
// Request/response shapes
// ---------------------------------------------------------------------------

/// Legacy query shape retained for wire compatibility. Reads resolve scope from
/// the middleware-verified bearer identity through [`resolve_required_scope`].
#[derive(Debug, Default, Deserialize)]
pub struct EnvelopeScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Query for the listing: the authenticated scope, plus the envelope scope.
#[derive(Debug, Default, Deserialize)]
pub struct EnvelopeListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub scope_kind: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
}

/// The grant body.
///
/// `scope`, `outcome`, `kind`, `covers` and `limits` carry no serde default: an
/// absent one is a deserialisation error, which is the refusal the plan asks for
/// — the caller states the scope, the limits and the expiry, or nothing is
/// granted.
///
/// There is deliberately no `granted_by`. The grantor is the authenticated
/// principal; a self-declared one is a signature anybody can write, and the
/// audit record exists precisely to be checked afterwards.
#[derive(Debug, Deserialize)]
pub struct GrantEnvelopeRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub scope: EnvelopeScope,
    pub outcome: String,
    pub kind: EnvelopeKind,
    pub covers: Vec<ConsequenceClass>,
    pub limits: EnvelopeLimits,
    #[serde(default)]
    pub boundary: Vec<BoundaryPredicate>,
}

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

fn not_found(envelope_id: &str) -> HttpResponse {
    api_error_response(
        StatusCode::NOT_FOUND,
        "envelope_not_found",
        format!("no envelope `{envelope_id}` in this scope"),
        None,
    )
}

/// A store that could not be read.
///
/// **Never an empty listing.** An unreadable log folded to "no envelopes" tells
/// the owner nothing is authorised, which is the single most reassuring wrong
/// answer this surface could give. `magician_v2::jsonl` propagates the failure
/// exactly so it can arrive here as a fault.
fn store_unreadable(error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "envelope_store_unreadable",
        "the envelope log could not be read, so what is authorised is unknown — and unknown is \
         not `nothing`",
        Some(json!({ "detail": format!("{error:#}") })),
    )
}

// ---------------------------------------------------------------------------
// Handlers — thin
// ---------------------------------------------------------------------------

/// `GET /api/magician/v2/approval-envelopes?scope_kind=&scope_id=`
///
/// Every envelope granted against one scope, newest first, including revoked and
/// expired ones: an owner asking "what did this do" after revoking is the main
/// reason to look, and filtering the dead ones out would remove the answer at
/// the moment it is wanted.
pub async fn list_envelopes_handler(
    req: HttpRequest,
    query: web::Query<EnvelopeListQuery>,
    api: web::Data<ApprovalEnvelopeApi>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let envelope_scope =
        match parse_envelope_scope(query.scope_kind.as_deref(), query.scope_id.as_deref()) {
            Ok(scope) => scope,
            Err(message) => return bad_request(message),
        };

    let now = Utc::now();
    let summaries = match api
        .owner_view(&principal, &workspace)
        .list(&envelope_scope, now)
    {
        Ok(summaries) => summaries,
        Err(error) => return store_unreadable(&error),
    };

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "scope": envelope_scope,
        "as_of": now,
        "counts": standing_counts(&summaries),
        "envelopes": summaries,
    }))
}

/// `GET /api/magician/v2/approval-envelopes/{envelope_id}`
///
/// One envelope and its full consumption ledger, oldest act first — §7's audit
/// property as a page: what was done under this, to whom, and which predicates
/// matched.
pub async fn get_envelope_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EnvelopeScopeQuery>,
    api: web::Data<ApprovalEnvelopeApi>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let envelope_id = path.into_inner();
    if let Err(message) = guard_path_id("an envelope id", &envelope_id) {
        return bad_request(message);
    }

    let now = Utc::now();
    match api
        .owner_view(&principal, &workspace)
        .detail(&envelope_id, now)
    {
        Ok(Some(detail)) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of": now,
            "envelope": detail.summary,
            "consumed": detail.consumed,
        })),
        Ok(None) => not_found(&envelope_id),
        Err(error) => store_unreadable(&error),
    }
}

/// `POST /api/magician/v2/approval-envelopes`
///
/// Grant, with the scope, the limits and the expiry stated by the caller.
///
/// The ordering is load-bearing. The replay read runs before the grant, so an
/// unreadable log fails the request as a fault rather than as a refusal — and,
/// past that read, a failure out of `store.grant` is a validation refusal the
/// caller can fix, which is why it answers 400 rather than 500.
pub async fn grant_envelope_handler(
    req: HttpRequest,
    body: web::Json<GrantEnvelopeRequest>,
    api: web::Data<ApprovalEnvelopeApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    // Every component the store will hash into the envelope id, guarded at the
    // only door a caller-supplied string comes through.
    for (label, value) in [
        ("the principal", principal.as_str()),
        ("the workspace", workspace.as_str()),
        ("a scope id", scope_id_of(&body.scope)),
        ("the outcome", body.outcome.as_str()),
    ] {
        if let Err(message) = guard_derivation_component(label, value) {
            return bad_request(message);
        }
    }

    let now = Utc::now();
    if let Err(message) = check_grantable_limits(&body.kind, &body.limits, now) {
        return bad_request(message);
    }

    let request = GrantEnvelope {
        scope: body.scope.clone(),
        outcome: body.outcome.clone(),
        kind: body.kind.clone(),
        covers: body.covers.clone(),
        limits: body.limits.clone(),
        boundary: body.boundary.clone(),
        granted_by: principal.clone(),
    };

    let view = api.owner_view(&principal, &workspace);
    let live = match load_scope_terms(&api, &principal, &workspace, &body.scope, now) {
        Ok(live) => live,
        Err(error) => return store_unreadable(&error),
    };

    match classify_grant_replay(&live, &request) {
        GrantReplay::Conflicts {
            envelope_id,
            differences,
        } => api_error_response(
            StatusCode::CONFLICT,
            "envelope_terms_changed",
            format!(
                "a live envelope already authorises `{}` in this scope on different terms; \
                 returning it for these terms would tell you that you had granted something you \
                 had not. Revoke `{envelope_id}` and grant again, or grant a different outcome",
                request.outcome
            ),
            Some(json!({
                "envelope_id": envelope_id,
                "differs_on": differences,
            })),
        ),
        GrantReplay::Resumes { envelope_id } => match view.detail(&envelope_id, now) {
            Ok(Some(detail)) => HttpResponse::Ok().json(json!({
                "principal": principal,
                "workspace": workspace,
                "as_of": now,
                "resumed": true,
                "envelope": detail.summary,
                "consumed": detail.consumed,
            })),
            Ok(None) => not_found(&envelope_id),
            Err(error) => store_unreadable(&error),
        },
        GrantReplay::Fresh => {
            let summary = match view.grant(&request, now) {
                Ok(summary) => summary,
                Err(error) => {
                    return api_error_response(
                        StatusCode::BAD_REQUEST,
                        "envelope_not_grantable",
                        format!("{error:#}"),
                        None,
                    )
                },
            };
            // Read back through the projection rather than returning the grant's
            // own summary: `OwnerView::grant` builds its summary against an empty
            // ledger, and the detail read is the same shape every other route
            // answers with.
            match view.detail(&summary.envelope_id, now) {
                Ok(Some(detail)) => HttpResponse::Created().json(json!({
                    "principal": principal,
                    "workspace": workspace,
                    "as_of": now,
                    "resumed": false,
                    "envelope": detail.summary,
                    "consumed": detail.consumed,
                })),
                Ok(None) => not_found(&summary.envelope_id),
                Err(error) => store_unreadable(&error),
            }
        },
    }
}

/// `POST /api/magician/v2/approval-envelopes/{envelope_id}/revoke`
///
/// Forward-only. There is no route back: revocation appends a successor to the
/// envelope's log and nothing edits it, so a revoked envelope stays revoked and
/// re-granting the same words mints a new envelope with its own id and its own
/// ledger. Revoking twice is a replay and reports the same state rather than an
/// error the owner has to interpret — `was_already_revoked` says which happened.
pub async fn revoke_envelope_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EnvelopeScopeQuery>,
    api: web::Data<ApprovalEnvelopeApi>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let envelope_id = path.into_inner();
    if let Err(message) = guard_path_id("an envelope id", &envelope_id) {
        return bad_request(message);
    }

    let now = Utc::now();
    let view = api.owner_view(&principal, &workspace);

    // Read before writing so an id belonging to no envelope in THIS scope is a
    // 404 rather than a store error — and so the reply can say whether this call
    // is the one that revoked it.
    let was_already_revoked = match view.detail(&envelope_id, now) {
        Ok(Some(detail)) => detail.summary.revoked_at.is_some(),
        Ok(None) => return not_found(&envelope_id),
        Err(error) => return store_unreadable(&error),
    };

    match view.revoke(&envelope_id, &principal, now) {
        Ok(summary) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of": now,
            "was_already_revoked": was_already_revoked,
            "envelope": summary,
        })),
        Err(error) => store_unreadable(&error),
    }
}

// ---------------------------------------------------------------------------
// Preview — the read that never spends
// ---------------------------------------------------------------------------

/// Body for the preview: one act, described the way a dispatch would describe
/// it, plus the scope it belongs to.
#[derive(Debug, Deserialize)]
pub struct PreviewWaiverBody {
    #[serde(default)]
    pub workspace: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub capability: String,
    pub action: String,
    /// The arguments the act would carry. The effective request — recipients,
    /// escape hatches — is derived from these here rather than accepted from the
    /// caller, so a preview cannot be asked about a tidier act than the one that
    /// would actually run.
    #[serde(default)]
    pub params: std::collections::HashMap<String, serde_json::Value>,
    /// The key the act would be debited under. **Required, and not defaulted.**
    /// An act with no stable key is refused by the gate as `unidentified_act`,
    /// so a preview that invented one would answer a question about a different
    /// act than the one that will run.
    pub act_ref: String,
    #[serde(default)]
    pub engagement_id: Option<String>,
    /// The engagement's known identities. An empty list is not "nobody to check
    /// against" — the recipient predicate fails closed on a recipient absent
    /// from it, so an empty list refuses every recipient.
    #[serde(default)]
    pub engagement_identities: Vec<String>,
    #[serde(default)]
    pub attachments_outside_ledger: Option<usize>,
    #[serde(default)]
    pub value_micros: Option<u64>,
}

/// `POST /api/magician/v2/approval-envelopes/preview`
///
/// Whether one act *would* be waived, without consuming anything.
///
/// # Why this route cannot authorise
///
/// It resolves through
/// [`magician::magician_v2::approval_envelopes::preview_approval_waiver`], which
/// downgrades every live posture to shadow before resolving. So the answer is
/// **advisory**: between this call and the real decision an envelope can expire,
/// be revoked, or be spent by another act. Nothing may act on this reply — the
/// deciding call is the one at dispatch, which debits.
///
/// `off` stays `off`: with envelopes not switched on the reply says `ask` and
/// names no envelope, rather than showing coverage that could never apply.
pub async fn preview_waiver_handler(
    req: HttpRequest,
    body: web::Json<PreviewWaiverBody>,
    api: web::Data<ApprovalEnvelopeApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let envelope_scope =
        match parse_envelope_scope(Some(body.scope_kind.as_str()), Some(body.scope_id.as_str())) {
            Ok(scope) => scope,
            Err(message) => return bad_request(message),
        };
    if body.act_ref.trim().is_empty() {
        return bad_request(
            "`act_ref` is required: the debit is idempotent on it, so an act with no key is              refused as unidentified rather than sharing a slot with every other keyless act",
        );
    }
    if body.capability.trim().is_empty() || body.action.trim().is_empty() {
        return bad_request(
            "both `capability` and `action` are required: the consequence class is derived from              the pair, and an unclassified gated act is treated as a commitment",
        );
    }

    let effective = magician::magician_v2::execution::resolve_effective_action(
        &body.capability,
        &body.action,
        &body.params,
    );
    let gate = EnvelopeGate::new(
        api.store.clone(),
        EnvelopeStoreScope::new(principal.as_str(), workspace.as_str()),
    );
    let now = Utc::now();
    let context = ApprovalContext {
        act_ref: &body.act_ref,
        effective: &effective,
        envelope_scope: Some(envelope_scope),
        engagement_id: body.engagement_id.as_deref(),
        engagement_identities: &body.engagement_identities,
        attachments_outside_ledger: body.attachments_outside_ledger,
        value_micros: body.value_micros,
        now,
    };

    let waiver = match preview_approval_waiver(
        &gate,
        magician::magician_v2::approval_envelopes::envelope_mode(),
        &body.capability,
        &body.action,
        context,
    ) {
        Ok(waiver) => waiver,
        Err(error) => return store_unreadable(&error),
    };

    // A preview resolves through shadow semantics, so the answer is always
    // `ask` — the useful signal is the REASON. `not_enforcing` is the one that
    // means "a live envelope covered this act and the only thing standing
    // between it and a waiver is the posture"; every other reason names a way
    // the envelopes did not cover it. Reporting a bare "not waived" would be
    // true of every possible reply and would tell the owner nothing.
    let reason = match &waiver {
        ApprovalWaiver::Ask { reason } => reason.as_ref(),
        // Unreachable: `preview_approval_waiver` never authorises. Handled
        // rather than asserted so a future change to the preview's semantics
        // surfaces here instead of silently reporting a waiver this route
        // promised it could not make.
        ApprovalWaiver::Waived { .. } => None,
    };
    let would_be_covered = matches!(
        reason,
        Some(magician::magician_v2::approval_envelopes::NotCoveredReason::NotEnforcing)
    );

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        "mode": magician::magician_v2::approval_envelopes::envelope_mode().as_str(),
        // Whether a live envelope covers this act. NOT permission to perform it:
        // the deciding call at dispatch resolves again and debits, and between
        // the two an envelope can expire, be revoked, or be spent.
        "would_be_covered": would_be_covered,
        // `null` when the posture is off, because nothing was resolved at all —
        // deliberately distinct from a reason, which means envelopes were
        // consulted and did not cover the act.
        "reason": reason.map(reason_label),
        "audit_line": waiver.audit_line(),
        "advisory": true,
    }))
}

/// Load every envelope in a scope paired with its raw stored terms.
///
/// The summary carries the derived standing; the raw envelope carries the terms
/// a replay has to be compared against (`per_recipient_cap` and the boundary
/// predicates' arguments never reach the summary, and a comparison that skipped
/// them would call two different grants identical).
fn load_scope_terms(
    api: &ApprovalEnvelopeApi,
    principal: &str,
    workspace: &str,
    envelope_scope: &EnvelopeScope,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<(EnvelopeSummary, ApprovalEnvelope)>> {
    let store_scope = EnvelopeStoreScope::new(principal, workspace);
    let summaries = api
        .owner_view(principal, workspace)
        .list(envelope_scope, now)?;
    let mut paired = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let Some(state) = api.store.load(&store_scope, &summary.envelope_id)? else {
            continue;
        };
        paired.push((summary, state.envelope));
    }
    Ok(paired)
}

/// Route registration. Mounted under `web::scope("/api/magician/v2")`.
///
/// The literal `/revoke` child is registered before the bare `{envelope_id}`
/// read for clarity; actix matches on the full pattern, so the two cannot
/// shadow each other, but keeping the order explicit means a later sibling
/// route added above it inherits the same discipline.
pub fn configure_approval_envelope_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/approval-envelopes", web::get().to(list_envelopes_handler))
        .route(
            "/approval-envelopes",
            web::post().to(grant_envelope_handler),
        )
        .route(
            "/approval-envelopes/preview",
            web::post().to(preview_waiver_handler),
        )
        .route(
            "/approval-envelopes/{envelope_id}/revoke",
            web::post().to(revoke_envelope_handler),
        )
        .route(
            "/approval-envelopes/{envelope_id}",
            web::get().to(get_envelope_handler),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;
    use chrono::Duration;
    use magician::magician_v2::approval_envelopes::BatchInstance;

    fn api_on(dir: &tempfile::TempDir) -> ApprovalEnvelopeApi {
        ApprovalEnvelopeApi::new(ArtifactV2Workspace::new(dir.path().to_path_buf()))
    }

    fn app_data(dir: &tempfile::TempDir) -> web::Data<ApprovalEnvelopeApi> {
        web::Data::new(api_on(dir))
    }

    fn service_config(cfg: &mut web::ServiceConfig) {
        configure_approval_envelope_routes(cfg);
    }

    fn standing_grant(scope: EnvelopeScope, outcome: &str) -> serde_json::Value {
        json!({
            "scope": scope,
            "outcome": outcome,
            "kind": { "kind": "standing" },
            "covers": ["bounded_communication"],
            "limits": {
                "max_acts": 5,
                "expires_at": (Utc::now() + Duration::days(7)).to_rfc3339(),
            },
            "boundary": [],
        })
    }

    // -- guards -------------------------------------------------------------

    /// A caller string carrying U+001F is refused before it can reach the id
    /// derivation. The store joins its components with that separator and does
    /// not check them, so a value carrying one shifts the boundary between two
    /// components and two different grants derive a single id — one silently
    /// resuming the other's cap and ledger.
    #[test]
    fn a_derivation_component_carrying_the_separator_is_refused() {
        let error = guard_derivation_component("the outcome", "reach out\u{1f}to anyone")
            .expect_err("U+001F must be refused");
        assert!(error.contains("U+001F"), "{error}");

        assert_eq!(
            guard_derivation_component("the outcome", "reach out"),
            Ok(())
        );
        assert!(guard_derivation_component("the outcome", "   ").is_err());
        assert!(guard_derivation_component("the outcome", "two\nlines").is_err());
    }

    /// An envelope id from the URL becomes a filename under the authenticated
    /// scope root. Without this guard `../` walks out of the scope and reads
    /// another tenant's envelope — the leak that makes a scoped surface worse
    /// than no surface.
    #[test]
    fn a_path_id_that_could_escape_the_scope_is_refused() {
        for hostile in [
            "../../beta/prod/approval_envelopes/envelopes/env-x",
            "..",
            "env/../../x",
            "env-x/../../y",
            "env x",
            "env-\u{1f}x",
        ] {
            assert!(
                guard_path_id("an envelope id", hostile).is_err(),
                "`{hostile}` must never become a path component"
            );
        }
        assert_eq!(
            guard_path_id("an envelope id", "env-0123456789abcdef0123456789abcdef"),
            Ok(())
        );
    }

    /// An unrecognised scope kind is refused, never rounded to the nearest one.
    /// A listing that guessed `program` for a typo would show a different piece
    /// of work's authority, and the answer would look correct.
    #[test]
    fn an_unrecognised_scope_kind_is_refused_not_guessed() {
        assert_eq!(
            parse_envelope_scope(Some("program"), Some("p-1")),
            Ok(EnvelopeScope::Program("p-1".to_string()))
        );
        assert_eq!(
            parse_envelope_scope(Some("Engagement"), Some("e-1")),
            Ok(EnvelopeScope::Engagement("e-1".to_string()))
        );
        assert!(parse_envelope_scope(Some("programme"), Some("p-1")).is_err());
        assert!(parse_envelope_scope(Some("workspace"), Some("w")).is_err());
        assert!(parse_envelope_scope(None, Some("p-1")).is_err());
        assert!(parse_envelope_scope(Some("program"), None).is_err());
        assert!(parse_envelope_scope(Some("program"), Some("  ")).is_err());
    }

    // -- grantability -------------------------------------------------------

    /// An envelope with no expiry is refused instead of being given a default
    /// window. A defaulted expiry is consent to a window the owner never chose,
    /// and it would read on the surface as though they had.
    #[test]
    fn a_grant_without_an_expiry_is_refused_rather_than_defaulted() {
        let now = Utc::now();
        let error = check_grantable_limits(
            &EnvelopeKind::Standing,
            &EnvelopeLimits {
                max_acts: Some(3),
                ..EnvelopeLimits::default()
            },
            now,
        )
        .expect_err("no expiry is not grantable");
        assert!(error.contains("no expiry is not grantable"), "{error}");

        // A reviewed batch gets no exemption here, though the store gives it one:
        // a batch whose instances are never all reached never exhausts.
        let batch = EnvelopeKind::ReviewedBatch {
            instances: vec![BatchInstance {
                recipient: "someone@example.test".to_string(),
                content_ref: None,
            }],
        };
        assert!(check_grantable_limits(&batch, &EnvelopeLimits::default(), now).is_err());
    }

    /// Expiry is INCLUSIVE, so an envelope granted at its own expiry instant is
    /// already expired. Accepting it would put a row on the owner's surface that
    /// reads `expired` from birth and authorised nothing.
    #[test]
    fn an_expiry_already_reached_is_refused_and_the_boundary_is_inclusive() {
        let now = Utc::now();
        let at_the_instant = EnvelopeLimits {
            max_acts: Some(1),
            expires_at: Some(now),
            ..EnvelopeLimits::default()
        };
        assert!(
            check_grantable_limits(&EnvelopeKind::Standing, &at_the_instant, now).is_err(),
            "now >= expires_at is expired; the boundary instant is not a window"
        );

        let one_nano_later = EnvelopeLimits {
            max_acts: Some(1),
            expires_at: Some(now + Duration::nanoseconds(1)),
            ..EnvelopeLimits::default()
        };
        assert_eq!(
            check_grantable_limits(&EnvelopeKind::Standing, &one_nano_later, now),
            Ok(())
        );
    }

    /// A standing envelope bounded only by time authorises an unbounded number
    /// of acts the owner has not seen. The cap is a COUNT — the question an
    /// owner can check against the ledger afterwards.
    #[test]
    fn a_standing_envelope_bounded_only_by_time_is_refused() {
        let now = Utc::now();
        let time_only = EnvelopeLimits {
            expires_at: Some(now + Duration::days(1)),
            ..EnvelopeLimits::default()
        };
        let error = check_grantable_limits(&EnvelopeKind::Standing, &time_only, now)
            .expect_err("time alone is not a bound on how many");
        assert!(error.contains("max_acts"), "{error}");

        let with_per_recipient = EnvelopeLimits {
            per_recipient_cap: Some(1),
            ..time_only.clone()
        };
        assert_eq!(
            check_grantable_limits(&EnvelopeKind::Standing, &with_per_recipient, now),
            Ok(())
        );
        // A batch is exhausted by its own list, so it needs no count cap.
        let batch = EnvelopeKind::ReviewedBatch {
            instances: vec![BatchInstance {
                recipient: "someone@example.test".to_string(),
                content_ref: None,
            }],
        };
        assert_eq!(check_grantable_limits(&batch, &time_only, now), Ok(()));
    }

    // -- replay -------------------------------------------------------------

    fn envelope_with(
        outcome: &str,
        limits: EnvelopeLimits,
        revoked: bool,
        now: DateTime<Utc>,
    ) -> (EnvelopeSummary, ApprovalEnvelope) {
        let envelope = ApprovalEnvelope {
            envelope_id: "env-existing".to_string(),
            scope: EnvelopeScope::Program("p-1".to_string()),
            outcome: outcome.to_string(),
            kind: EnvelopeKind::Standing,
            covers: vec![ConsequenceClass::BoundedCommunication],
            limits,
            boundary: Vec::new(),
            granted_by: "alpha".to_string(),
            granted_at: now - Duration::hours(1),
            revoked_at: revoked.then(|| now - Duration::minutes(1)),
        };
        let state = magician::magician_v2::approval_envelopes::EnvelopeState {
            envelope: envelope.clone(),
            consumed: Vec::new(),
        };
        (
            magician::magician_v2::approval_envelopes::owner_view::summarise(&state, now),
            envelope,
        )
    }

    fn request_with(outcome: &str, limits: EnvelopeLimits) -> GrantEnvelope {
        GrantEnvelope {
            scope: EnvelopeScope::Program("p-1".to_string()),
            outcome: outcome.to_string(),
            kind: EnvelopeKind::Standing,
            covers: vec![ConsequenceClass::BoundedCommunication],
            limits,
            boundary: Vec::new(),
            granted_by: "alpha".to_string(),
        }
    }

    /// A retried POST with the identical payload resumes the live envelope
    /// instead of minting a second one. The store's id folds in the grant
    /// instant, so without this a double-click would silently double the
    /// owner's cap — two five-act envelopes reading as five acts each.
    #[test]
    fn an_identical_replay_resumes_the_live_envelope() {
        let now = Utc::now();
        let limits = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now + Duration::days(7)),
            ..EnvelopeLimits::default()
        };
        let live = vec![envelope_with("keep in touch", limits.clone(), false, now)];
        assert_eq!(
            classify_grant_replay(&live, &request_with("keep in touch", limits)),
            GrantReplay::Resumes {
                envelope_id: "env-existing".to_string()
            }
        );
    }

    /// A changed payload is an ERROR, not a silent no-op. Returning the old
    /// envelope for a request that raised `max_acts` from 5 to 50 would answer
    /// 200 with an envelope that caps at 5 while the owner believes they raised
    /// it — the surface would have lied about what it did.
    #[test]
    fn a_changed_payload_conflicts_instead_of_silently_resuming() {
        let now = Utc::now();
        let stored = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now + Duration::days(7)),
            ..EnvelopeLimits::default()
        };
        let raised = EnvelopeLimits {
            max_acts: Some(50),
            ..stored.clone()
        };
        let live = vec![envelope_with("keep in touch", stored, false, now)];
        assert_eq!(
            classify_grant_replay(&live, &request_with("keep in touch", raised)),
            GrantReplay::Conflicts {
                envelope_id: "env-existing".to_string(),
                differences: vec!["limits".to_string()],
            }
        );
    }

    /// A revoked envelope is terminal and never resurrects. Re-granting the same
    /// words past a revocation is a NEW grant with its own id and its own empty
    /// ledger; resuming the revoked one would undo the revocation through the
    /// grant route.
    #[test]
    fn a_revoked_envelope_is_never_resumed_by_a_later_grant() {
        let now = Utc::now();
        let limits = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now + Duration::days(7)),
            ..EnvelopeLimits::default()
        };
        let live = vec![envelope_with("keep in touch", limits.clone(), true, now)];
        assert_eq!(
            classify_grant_replay(&live, &request_with("keep in touch", limits)),
            GrantReplay::Fresh
        );
    }

    /// An expired envelope is terminal too — the clock, not a decision, ended it,
    /// and a re-grant must mint a fresh window rather than revive a lapsed one.
    #[test]
    fn an_expired_envelope_is_never_resumed_by_a_later_grant() {
        let now = Utc::now();
        let lapsed = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now - Duration::seconds(1)),
            ..EnvelopeLimits::default()
        };
        let (summary, envelope) = envelope_with("keep in touch", lapsed, false, now);
        assert_eq!(
            summary.standing,
            EnvelopeStanding::Expired,
            "the projection derives expiry from the clock"
        );
        let fresh = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now + Duration::days(7)),
            ..EnvelopeLimits::default()
        };
        assert_eq!(
            classify_grant_replay(
                &[(summary, envelope)],
                &request_with("keep in touch", fresh)
            ),
            GrantReplay::Fresh
        );
    }

    /// A different outcome in the same scope is a different grant, not a replay.
    /// Folding two outcomes together would make one envelope's cap silently
    /// govern acts the owner authorised separately.
    #[test]
    fn a_different_outcome_is_a_fresh_grant() {
        let now = Utc::now();
        let limits = EnvelopeLimits {
            max_acts: Some(5),
            expires_at: Some(now + Duration::days(7)),
            ..EnvelopeLimits::default()
        };
        let live = vec![envelope_with("keep in touch", limits.clone(), false, now)];
        assert_eq!(
            classify_grant_replay(&live, &request_with("send the update", limits)),
            GrantReplay::Fresh
        );
    }

    // -- counts -------------------------------------------------------------

    /// The listing reports COUNTS by standing, taken from the standing the
    /// projection already derived. A rate would be a number the owner cannot
    /// reconcile with the rows printed beneath it.
    #[test]
    fn standing_counts_tally_the_projections_own_verdicts() {
        let now = Utc::now();
        let active = envelope_with(
            "a",
            EnvelopeLimits {
                max_acts: Some(5),
                expires_at: Some(now + Duration::days(1)),
                ..EnvelopeLimits::default()
            },
            false,
            now,
        )
        .0;
        let revoked = envelope_with(
            "b",
            EnvelopeLimits {
                max_acts: Some(5),
                expires_at: Some(now + Duration::days(1)),
                ..EnvelopeLimits::default()
            },
            true,
            now,
        )
        .0;
        let expired = envelope_with(
            "c",
            EnvelopeLimits {
                max_acts: Some(5),
                expires_at: Some(now - Duration::seconds(1)),
                ..EnvelopeLimits::default()
            },
            false,
            now,
        )
        .0;
        let counts = standing_counts(&[active, revoked, expired]);
        assert_eq!(counts["total"], 3);
        assert_eq!(counts["active"], 1);
        assert_eq!(counts["revoked"], 1);
        assert_eq!(counts["expired"], 1);
        assert_eq!(counts["exhausted"], 0);
    }

    // -- routes -------------------------------------------------------------

    /// Scope is required on every read. A listing that defaulted the principal
    /// would show one owner's authorisations to whoever asked without one, which
    /// is the leak that makes a scoped surface worse than no surface.
    #[actix_web::test]
    async fn a_read_without_scope_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;
        let req = actix_test::TestRequest::get()
            .uri("/approval-envelopes?scope_kind=program&scope_id=p-1")
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, req).await.status().as_u16(),
            400
        );
    }

    /// Granting, listing and revoking are one round trip, and the standing the
    /// list reports is the projection's — `active` while live, `revoked` the
    /// moment it is withdrawn, with the grant still listed so the owner can see
    /// what it did.
    #[actix_web::test]
    async fn a_grant_is_listed_active_then_revoked_and_stays_listed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;

        let granted: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(standing_grant(
                    EnvelopeScope::Program("p-1".to_string()),
                    "keep the pipeline warm",
                ))
                .to_request(),
        )
        .await;
        assert_eq!(granted["resumed"], false);
        assert_eq!(granted["envelope"]["standing"], "active");
        assert_eq!(granted["envelope"]["acts"]["used"], 0);
        assert_eq!(granted["envelope"]["acts"]["limit"], 5);
        assert_eq!(granted["envelope"]["granted_by"], "alpha");
        let envelope_id = granted["envelope"]["envelope_id"]
            .as_str()
            .expect("envelope id")
            .to_string();

        let listed: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/approval-envelopes?scope_kind=program&scope_id=p-1")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(listed["counts"]["total"], 1);
        assert_eq!(listed["counts"]["active"], 1);
        assert_eq!(listed["envelopes"][0]["envelope_id"], envelope_id.as_str());

        let revoked: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/approval-envelopes/{envelope_id}/revoke"))
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(revoked["was_already_revoked"], false);
        assert_eq!(revoked["envelope"]["standing"], "revoked");

        // Revoking again is a replay, not an error — and not a second revocation.
        let again: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/approval-envelopes/{envelope_id}/revoke"))
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(again["was_already_revoked"], true);
        assert_eq!(again["envelope"]["standing"], "revoked");
        assert_eq!(
            again["envelope"]["revoked_at"], revoked["envelope"]["revoked_at"],
            "revocation is forward-only: the second call must not move the instant"
        );

        // A revoked envelope stays in the listing — "what did this do" is the
        // main reason an owner looks after revoking.
        let after: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/approval-envelopes?scope_kind=program&scope_id=p-1")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(after["counts"]["total"], 1);
        assert_eq!(after["counts"]["active"], 0);
        assert_eq!(after["counts"]["revoked"], 1);
    }

    /// One tenant's envelope is invisible to another, both to read and to
    /// revoke. The store derives its paths from the scope it is handed, so this
    /// pins that the handler hands it the AUTHENTICATED scope and never the
    /// caller's claim about which envelope to open.
    #[actix_web::test]
    async fn another_principals_envelope_is_not_readable_or_revocable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;

        let granted: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(standing_grant(
                    EnvelopeScope::Program("p-1".to_string()),
                    "keep the pipeline warm",
                ))
                .to_request(),
        )
        .await;
        let envelope_id = granted["envelope"]["envelope_id"]
            .as_str()
            .expect("envelope id")
            .to_string();

        let read = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri(&format!("/approval-envelopes/{envelope_id}"))
                .insert_header(("X-Principal", "beta"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(read.status().as_u16(), 404);

        let revoke = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri(&format!("/approval-envelopes/{envelope_id}/revoke"))
                .insert_header(("X-Principal", "beta"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(revoke.status().as_u16(), 404);

        // And the owner's own envelope is untouched by the attempt.
        let mine: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri(&format!("/approval-envelopes/{envelope_id}"))
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(mine["envelope"]["standing"], "active");
    }

    /// A grant with no `limits` key is refused at deserialisation. The plan asks
    /// the caller to STATE the limits; a `limits` field that defaulted to "none
    /// of them" would turn the most permissive envelope into the one you get by
    /// leaving a field out.
    #[actix_web::test]
    async fn a_grant_body_without_limits_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(json!({
                    "scope": EnvelopeScope::Program("p-1".to_string()),
                    "outcome": "keep the pipeline warm",
                    "kind": { "kind": "standing" },
                    "covers": ["bounded_communication"],
                }))
                .to_request(),
        )
        .await;
        assert!(
            response.status().is_client_error(),
            "an absent `limits` must not deserialise to an unbounded envelope"
        );
    }

    /// A second POST of the same body resumes rather than minting a second
    /// envelope, and a third with a raised cap is a 409. Together these pin the
    /// whole replay contract at the route: identical resumes, changed errors.
    #[actix_web::test]
    async fn a_replayed_grant_resumes_and_a_changed_one_conflicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;
        let expires = (Utc::now() + Duration::days(7)).to_rfc3339();
        let body = json!({
            "scope": EnvelopeScope::Program("p-1".to_string()),
            "outcome": "keep the pipeline warm",
            "kind": { "kind": "standing" },
            "covers": ["bounded_communication"],
            "limits": { "max_acts": 5, "expires_at": expires },
            "boundary": [],
        });

        let first: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(body.clone())
                .to_request(),
        )
        .await;
        assert_eq!(first["resumed"], false);

        let second: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(body.clone())
                .to_request(),
        )
        .await;
        assert_eq!(second["resumed"], true);
        assert_eq!(
            second["envelope"]["envelope_id"], first["envelope"]["envelope_id"],
            "an identical replay must not mint a second envelope"
        );

        let mut raised = body;
        raised["limits"]["max_acts"] = json!(50);
        let conflict = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(raised)
                .to_request(),
        )
        .await;
        assert_eq!(conflict.status().as_u16(), 409);

        // Exactly one envelope exists: the conflict granted nothing.
        let listed: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri("/approval-envelopes?scope_kind=program&scope_id=p-1")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(listed["counts"]["total"], 1);
    }

    /// A class a standing envelope may never carry is refused by the STORE, and
    /// the handler surfaces that as a caller error rather than a fault. Pins
    /// that the surface asks the store rather than re-deciding the taxonomy —
    /// two copies of "what may be covered" is how one of them ends up wrong.
    #[actix_web::test]
    async fn a_class_a_standing_envelope_may_never_carry_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = actix_test::init_service(
            actix_web::App::new()
                .app_data(app_data(&dir))
                .configure(service_config),
        )
        .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/approval-envelopes")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(json!({
                    "scope": EnvelopeScope::Program("p-1".to_string()),
                    "outcome": "share the numbers",
                    "kind": { "kind": "standing" },
                    "covers": ["commitment_or_transaction"],
                    "limits": {
                        "max_acts": 1,
                        "expires_at": (Utc::now() + Duration::days(1)).to_rfc3339(),
                    },
                    "boundary": [],
                }))
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 400);
    }
}
