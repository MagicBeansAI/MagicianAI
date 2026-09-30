//! **The owner surface for the composable work modules** — durable runs, time
//! negotiations, artifact claim manifests, and the register of what is owed.
//!
//! Plan: `docs/plans/2026-08-07-opc-composable-work-modules.md` (§3 Module A,
//! §4/§5 Module C, Module B's provenance half, §6 Module D).
//!
//! Four modules were built, tested and reachable from nothing:
//! [`run_state`](magician_media::run_state),
//! [`scheduling`](magician_media::scheduling),
//! [`claim_manifest`](magician::magician_v2::claim_manifest) and
//! [`obligations`](magician::magician_v2::obligations). Every store was empty
//! on disk because no code path had ever written to one. This is the surface
//! that joins them to a person.
//!
//! # One module, not four
//!
//! The four are one programme and their routes are thin — a handler here
//! resolves a scope, guards the caller strings that feed a derived id, and
//! delegates. Four files would have meant four copies of the scope resolution,
//! the separator guard, the audience parser and the store-unreadable refusal,
//! and those copies are what drift. The seams the modules themselves keep
//! apart stay apart: nothing below lets a run reach into a negotiation, or the
//! register reach back into either.
//!
//! # Nothing here names a kind of relationship or a kind of work
//!
//! Every route that takes a relationship takes an **audience kind and id**,
//! parsed through [`AudienceKind::parse`], which refuses a word it does not
//! know rather than defaulting to one. A support-triage flow, a recruiting
//! loop and a vendor review reach these routes with `account`, `panel` and
//! `person` and need no edit to this file. A run's `purpose`, a negotiation's
//! `purpose`, an obligation's `what` and a manifest's claim refs are all
//! caller text; no vocabulary from any consumer appears below.
//!
//! # Fail closed, everywhere
//!
//! - **An unreadable store is a fault, never an empty answer.** Every listing
//!   route answers `500` on a read failure rather than `[]`. "Nothing is owed"
//!   out of a disk fault is the most reassuring wrong answer this surface
//!   could give.
//! - **A missing run, negotiation or obligation is `404`, never an empty
//!   outstanding list.** A mistyped id must not read as a finished form.
//! - **`U+001F` is refused at the door.** The run store and the sweep
//!   derivations refuse it themselves; the scheduling store, the claim
//!   manifest store and the obligation register do **not**, and every one of
//!   them joins caller strings with that separator to derive an id. A crafted
//!   component could shift bytes across the boundary and fuse two identities
//!   into one — one tenant's register row addressed from another's request.
//!   [`guard_id_component`] is the single door, and it runs before any store
//!   is touched.
//! - **No route derives a time state.** Lapsed, silent, awaiting and ready are
//!   all derived by the modules from the clock at read time; this surface
//!   passes `Utc::now()` in and serialises what comes back.
//!
//! # What this surface refuses to do
//!
//! - **It never sends and never books.** `offer_message` returns text and
//!   slots for *any* send capability and `hold_intent` returns what a calendar
//!   needs; the routes hand those back to the caller and take the resulting
//!   act ref on the way in. Baking a channel in here is the coupling Module A
//!   was arranged to avoid.
//! - **It never fires the submit gate.** `POST /work/runs/{id}/submit` carries
//!   a named person and records the covering outward act before the run seals;
//!   there is no path to `submitted` without both.
//! - **It never reads an inbox.** `POST /work/runs/{id}/expectations/fulfil`
//!   and `POST /work/negotiations/{id}/reply` take what the caller already
//!   extracted.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{AudienceKind, AudienceRef};
use magician::magician_v2::claim_manifest::{ClaimBinding, ClaimManifestScope, ClaimManifestStore};
use magician::magician_v2::evidence::OutwardAssertionStore;
use magician::magician_v2::obligations::{
    Obligation, ObligationDirection, ObligationScope, ObligationState, ObligationStore,
    RecordObligation, Settlement,
};
use magician_learning::data_room::FollowUpPolicy;
use magician_media::obligation_sweeps::attention::{attention_notes_for_scope, explain_register};
use magician_media::obligation_sweeps::worker::ObligationSweepConfig;
use magician_media::obligation_sweeps::{sweep_scope, SweepMemory, SweepPolicy};
use magician_media::run_state::{
    expectations_awaiting, fulfil_from_inbound, gaps_for_owner, Answer, FormSubmissionAct,
    InboundEvent, InboundOutcome, RunScope, RunStateStore,
};
use magician_media::scheduling::{
    absorb_reply, hold_intent, offer_message, record_hold, InboundReading, InboundReply,
    Negotiation, SchedulingScope, SchedulingStore, Slot,
};

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;

/// The separator every derived id in this programme is built from.
const FIELD_SEP: char = '\u{1f}';

/// Upper bound on an id taken from a URL path.
///
/// The stores' own ids are a short prefix plus 32 hex characters. Generous
/// rather than exact so a future id shape does not silently `400`, while still
/// refusing the pathological lengths a path component should never carry.
const MAX_PATH_ID_LEN: usize = 128;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Shared state for the work-module routes.
///
/// Holds the workspace layout rather than per-scope stores, because a store is
/// bound to nothing until it is handed a scope and the scope is resolved per
/// request. Every store below derives its paths from the scope it is given, so
/// a handler is physically unable to read another tenant's logs.
#[derive(Clone)]
pub struct WorkModulesApi {
    workspace_layout: ArtifactV2Workspace,
    /// The waiting windows a **reading** is derived under.
    ///
    /// The same type the sweep worker runs on, and it has to hold the same
    /// value. A follow-up's `due_at` is `shared_at + delivery_question_after`
    /// or `last_seen + follow_up_after`, and that instant is part of the
    /// register's identity tuple — so a reading derived under different windows
    /// names a row that was never written. Defaulted to the worker's own
    /// defaults so the two agree out of the box, and settable through
    /// [`Self::with_sweep_config`] so a deployment that configures the worker
    /// hands this the identical value rather than a second copy of the numbers.
    ///
    /// A mismatch is never silent: the join reports a handle the register does
    /// not hold rather than attaching it to the nearest row.
    sweep_config: ObligationSweepConfig,
    /// What the last sweep of each scope SAW, so the next one can tell what has
    /// since disappeared.
    ///
    /// Keyed by `principal/workspace` because the memory is a fact about a
    /// scope, not about who asked — two operators sweeping the same register
    /// share one previous view, which is the only way the second call can
    /// settle what the first observed.
    ///
    /// # Why this is not a cache
    ///
    /// Without it every request built a fresh `SweepMemory::unseeded()`, and
    /// unseeded is the documented honest form of *"first sweep"*: it records
    /// ripened obligations and settles only what the current facts themselves
    /// say is settled, never anything inferred from a relationship's absence.
    /// So the route could report the same obligation ripening forever and never
    /// retire a single row — a sweep that is green over work it can structurally
    /// never finish. Holding the previous view is what makes a *second* call
    /// different from a first.
    ///
    /// Process-local and deliberately not persisted: a restart legitimately has
    /// no previous view, and `unseeded` is the correct answer then. Persisting
    /// it would mean a memory that outlived the facts it was a memory of.
    sweep_memory: Arc<Mutex<HashMap<(String, String), SweepMemory>>>,
    /// Where a run's out-of-band verification arrives, if anywhere.
    ///
    /// `None` means the sweep has no source. Reported as `no_source` rather
    /// than as a quiet zero: a pass that closed nothing because it asked nobody
    /// must not read like a pass that closed nothing because nothing arrived —
    /// the second says the loop is working, the first says it is not running.
    run_inbox: Option<magician::config::RunInboxConfig>,
}

impl WorkModulesApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            sweep_config: ObligationSweepConfig::default(),
            sweep_memory: Arc::new(Mutex::new(HashMap::new())),
            run_inbox: None,
        }
    }

    /// The mailbox the run-inbox sweep reads.
    pub fn with_run_inbox(mut self, run_inbox: Option<magician::config::RunInboxConfig>) -> Self {
        self.run_inbox = run_inbox;
        self
    }

    /// Bind the reading windows to the sweep worker's own configuration.
    ///
    /// Takes the config rather than the two numbers, so the read and the sweep
    /// cannot be given windows that differ by a field somebody forgot to pass.
    pub fn with_sweep_config(mut self, sweep_config: ObligationSweepConfig) -> Self {
        self.sweep_config = sweep_config;
        self
    }

    /// The follow-up windows, or why they cannot be honoured.
    ///
    /// Refused at or below zero exactly as the worker refuses them: a zero
    /// window declares every share undelivered before the mail could arrive, so
    /// a substituted one would have this surface reading the room under a
    /// cadence nobody chose.
    fn follow_up_policy(&self) -> anyhow::Result<FollowUpPolicy> {
        Ok(*self.sweep_config.policy()?.follow_up())
    }

    fn runs(&self) -> RunStateStore {
        RunStateStore::new(self.workspace_layout.clone())
    }

    fn scheduling(&self) -> SchedulingStore {
        SchedulingStore::new(self.workspace_layout.clone())
    }

    fn manifests(&self) -> ClaimManifestStore {
        ClaimManifestStore::new(self.workspace_layout.clone())
    }

    fn register(&self) -> ObligationStore {
        ObligationStore::new(self.workspace_layout.clone())
    }

    fn assertions(&self) -> OutwardAssertionStore {
        OutwardAssertionStore::new(self.workspace_layout.clone())
    }
}

// ---------------------------------------------------------------------------
// Guards — pure, and the reason a caller string never reaches a derivation
// ---------------------------------------------------------------------------

/// A caller string that feeds an id derivation.
///
/// The scheduling store joins `(principal, workspace, audience_key,
/// counterparty, purpose)`, the claim manifest store joins `(principal,
/// workspace, artifact_ref, revision_ref)`, and the register joins
/// `(principal, workspace, audience_key, what, due_at, direction)` — each with
/// [`FIELD_SEP`], each then hashed. A component carrying that separator can
/// shift the boundary between two components, so two different asks derive one
/// id. None of those three stores checks, so nothing downstream will.
pub fn guard_id_component(label: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{label} must not be blank; a blank component derives an id nobody meant"
        ));
    }
    if trimmed.contains(FIELD_SEP) {
        return Err(format!(
            "{label} must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other, and a component carrying it could fuse \
             two different records into one id"
        ));
    }
    Ok(())
}

/// An id taken from a URL path.
///
/// Not a derivation component — the stores address by id rather than hashing
/// it — but a path segment all the same, so it is bounded and separator-free.
pub fn guard_path_id(label: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} must not be blank"));
    }
    if trimmed.len() > MAX_PATH_ID_LEN {
        return Err(format!(
            "{label} is longer than {MAX_PATH_ID_LEN} characters; a path id is a short derived \
             token"
        ));
    }
    if trimmed.contains(FIELD_SEP) {
        return Err(format!("{label} must not contain U+001F"));
    }
    Ok(())
}

/// The relationship a request names, or why it is not one.
///
/// The kind is parsed, never defaulted: [`AudienceKind::parse`] answers `None`
/// for a word it does not know, and turning that into `Engagement` would file a
/// programme's, an account's or a panel's record under the engagement key —
/// which [`AudienceRef::as_key`] exists precisely to keep apart. Every arm of
/// the enum is reachable from here, so a second flow needs no edit to this
/// file.
pub fn parse_audience(kind: &str, id: &str) -> Result<AudienceRef, String> {
    let Some(kind) = AudienceKind::parse(kind) else {
        return Err(format!(
            "`{kind}` is not an audience kind; the known kinds are {}. An unrecognised kind is \
             refused rather than defaulted, because filing one relationship's record under \
             another's key is unrecoverable",
            AudienceKind::ALL
                .iter()
                .map(|held| held.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    guard_id_component("an audience id", id)?;
    let reference = AudienceRef::new(kind, id.trim());
    if !reference.is_named() {
        return Err(
            "an audience must be named; an unnamed relationship is open-ended access by \
             another name, which this codebase refuses to represent"
                .to_string(),
        );
    }
    Ok(reference)
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

fn not_found(code: &str, error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::NOT_FOUND, code, error, None)
}

/// A store that could not be read.
///
/// **Never an empty listing.** An unreadable log folded to "no runs", "no
/// negotiations" or "nothing is owed" tells the owner everything is fine,
/// which is the single most reassuring wrong answer this surface could give.
/// `magician_v2::jsonl` propagates the failure exactly so it can arrive here.
fn store_unreadable(what: &str, error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "work_store_unreadable",
        format!("{what} could not be read, so the answer is unknown — and unknown is not `none`"),
        Some(json!({ "detail": format!("{error:#}") })),
    )
}

/// A write the store refused.
///
/// `409`, not `500`: every refusal these stores raise is a rule about what may
/// be recorded — a submission that is not ready, an acceptance of a slot
/// nobody offered, a rebind that would change a revision's claims. The caller
/// can act on all of them, and burying the message in a `500` would hide the
/// one thing they need.
fn refused(code: &str, error: &anyhow::Error) -> HttpResponse {
    api_error_response(StatusCode::CONFLICT, code, format!("{error:#}"), None)
}

// ---------------------------------------------------------------------------
// Shared request shapes
// ---------------------------------------------------------------------------

/// The authenticated scope, engraved internally from the workspace-bound bearer.
/// headers, or a `workspace` field.
#[derive(Debug, Default, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// The authenticated scope plus the relationship a negotiation lives in.
///
/// The audience is a parameter rather than a lookup because the scheduling
/// store keeps one log per relationship and a negotiation id alone cannot say
/// which log to open.
#[derive(Debug, Default, Deserialize)]
pub struct AudienceScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
}

fn resolve_scope(
    req: &HttpRequest,
    workspace: Option<String>,
) -> Result<(String, String), HttpResponse> {
    let (principal, workspace) = resolve_required_scope(req.headers(), workspace)?;
    for (label, value) in [
        ("the principal", principal.as_str()),
        ("the workspace", workspace.as_str()),
    ] {
        if let Err(message) = guard_id_component(label, value) {
            return Err(bad_request(message));
        }
    }
    Ok((principal, workspace))
}

// ---------------------------------------------------------------------------
// 1. Durable runs — what this run is waiting for, and what needs a human
// ---------------------------------------------------------------------------

/// Open a run.
#[derive(Debug, Deserialize)]
pub struct OpenRunRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub purpose: String,
    pub resource_ref: String,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// Declaration order is preserved; `required` is stated per field because
    /// the submit gate reads it and a defaulted one would let an unstated
    /// field silently stop blocking.
    #[serde(default)]
    pub fields: Vec<DeclaredField>,
    pub opened_by: String,
}

#[derive(Debug, Deserialize)]
pub struct DeclaredField {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeclareFieldRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub name: String,
    pub required: bool,
}

/// A grounded answer. `evidence_refs` carries no serde default: an absent one
/// is a deserialisation error, which is the refusal §5 asks for — *"never
/// answer from nothing"*.
#[derive(Debug, Deserialize)]
pub struct AnswerRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub field: String,
    pub text: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RaiseGapRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub field: String,
    pub question: String,
}

/// The owner's reply to a gap. `resolved_by` is required and never defaulted:
/// an unattributed resolution is synthesised text wearing an owner's authority.
#[derive(Debug, Deserialize)]
pub struct ResolveGapRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub field: String,
    pub question: String,
    pub text: String,
    pub evidence_refs: Vec<String>,
    pub resolved_by: String,
}

#[derive(Debug, Deserialize)]
pub struct RaiseExpectationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub description: String,
    pub source_hint: String,
}

/// One event the caller extracted from wherever it watches. The extraction is
/// the caller's; this surface reads no inbox.
#[derive(Debug, Deserialize)]
pub struct FulfilExpectationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub event_ref: String,
    pub source_hint: String,
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

/// Press send. There is deliberately no path here that omits `by` or the
/// payload: the module never fires the gate itself.
#[derive(Debug, Deserialize)]
pub struct SubmitRunRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub by: String,
    pub exact_payload_artifact_ref: String,
    pub recipients: Vec<String>,
}

/// `GET /api/magician/v2/work/runs`
///
/// Every run this scope has opened, with the state each derives from its own
/// contents. Submitted runs are included: what went out is the evidence, and
/// hiding it would leave the listing showing only unfinished work.
pub async fn list_runs_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = RunScope::new(principal.clone(), workspace.clone());
    let runs = match api.runs().all_runs(&scope) {
        Ok(runs) => runs,
        Err(error) => return store_unreadable("the run logs", &error),
    };
    let now = Utc::now();
    let summaries: Vec<Value> = runs.iter().map(run_summary).collect();
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        "counts": {
            "total": runs.len(),
            "drafting": count_state(&runs, "drafting"),
            "awaiting_external": count_state(&runs, "awaiting_external"),
            "ready_for_review": count_state(&runs, "ready_for_review"),
            "submitted": count_state(&runs, "submitted"),
        },
        "runs": summaries,
    }))
}

/// `POST /api/magician/v2/work/runs`
///
/// Open a run, or resume the one already open for this work. Idempotent on
/// `(scope, purpose, resource_ref)` in the store, so a retried POST lands on
/// THE run rather than a duplicate half-form beside it.
pub async fn open_run_handler(
    req: HttpRequest,
    body: web::Json<OpenRunRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match (body.audience_kind.as_deref(), body.audience_id.as_deref()) {
        (Some(kind), Some(id)) => match parse_audience(kind, id) {
            Ok(reference) => Some(reference),
            Err(message) => return bad_request(message),
        },
        (None, None) => None,
        // A half-named relationship is a caller bug, not an unbound run: a run
        // against the owner's own records binds nobody and says so by sending
        // neither field.
        _ => {
            return bad_request(
                "an audience needs both `audience_kind` and `audience_id`, or neither; half a \
                 relationship cannot be resolved and must not be silently dropped",
            )
        },
    };
    let fields: Vec<(String, bool)> = body
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.required))
        .collect();

    let scope = RunScope::new(principal.clone(), workspace.clone());
    match api.runs().open(
        &scope,
        &body.purpose,
        &body.resource_ref,
        audience,
        &fields,
        &body.opened_by,
        Utc::now(),
    ) {
        Ok(run) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "run": run,
            "state": run.state().as_str(),
        })),
        Err(error) => refused("run_refused", &error),
    }
}

/// `GET /api/magician/v2/work/runs/{run_id}`
///
/// The run, **what it is waiting for**, and **what needs a human** — the three
/// answers §5 says an owner needs, in one read.
///
/// `awaiting` carries each wait's age, derived from the clock on this read and
/// never stored: a written-down *"waiting three days"* needs somebody to keep
/// it honest, and the one thing a multi-session run cannot rely on is the
/// previous session having done that.
pub async fn get_run_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal.clone(), workspace.clone());
    let store = api.runs();

    let run = match store.load(&scope, &run_id) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return not_found(
                "run_not_found",
                format!(
                    "no run `{run_id}` in this scope; an absent run is not a run with nothing \
                     outstanding"
                ),
            )
        },
        Err(error) => return store_unreadable("the run log", &error),
    };

    let now = Utc::now();
    let awaiting = match expectations_awaiting(&store, &scope, &run_id, now) {
        Ok(awaiting) => awaiting,
        Err(error) => return store_unreadable("the run's open waits", &error),
    };
    let gaps = match gaps_for_owner(&store, &scope, &run_id) {
        Ok(gaps) => gaps,
        Err(error) => return store_unreadable("the run's open gaps", &error),
    };

    // `waiting_for` is a `chrono::Duration`, which has no JSON shape of its
    // own; seconds are the honest projection, and they are recomputed on every
    // read rather than stored.
    let waits: Vec<Value> = awaiting
        .iter()
        .map(|held| {
            json!({
                "expectation_id": held.expectation_id,
                "description": held.description,
                "source_hint": held.source_hint,
                "raised_at": held.raised_at,
                "waiting_for_seconds": held.waiting_for.num_seconds(),
            })
        })
        .collect();
    let questions: Vec<Value> = gaps
        .iter()
        .map(|gap| {
            json!({
                "field": gap.field,
                "question": gap.question,
                "raised_at": gap.raised_at,
                "blocks_review": gap.blocks_review,
            })
        })
        .collect();
    let fields_pending = run
        .fields
        .iter()
        .filter(|held| held.answer.is_none())
        .count();
    let blocking = gaps.iter().filter(|gap| gap.blocks_review).count();

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        "run": run,
        "state": run.state().as_str(),
        "revision_high_water": run.revision_high_water(),
        "counts": {
            "fields": run.fields.len(),
            "fields_pending": fields_pending,
            "gaps_open": gaps.len(),
            "gaps_blocking_review": blocking,
            "expectations_open": awaiting.len(),
        },
        "awaiting": waits,
        "gaps": questions,
    }))
}

/// `POST /api/magician/v2/work/runs/{run_id}/fields`
///
/// Declare a field the form revealed after opening — a conditional section.
pub async fn declare_field_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DeclareFieldRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal, workspace);
    match api
        .runs()
        .declare_field(&scope, &run_id, &body.name, body.required, Utc::now())
    {
        Ok(run) => HttpResponse::Ok().json(json!({ "run": run, "state": run.state().as_str() })),
        Err(error) => refused("field_refused", &error),
    }
}

/// Which of these evidence refs name a record marked **sensitive**.
///
/// §5: *"retrieval is filtered by `EvidenceRecord.sensitivity`, so private
/// material cannot reach an application through a well-meaning agent's
/// judgment — the boundary is enforced on the real source rather than on a
/// curated copy of it."*
///
/// The retrieval side already filters — `load_scoped_evidence` drops sensitive
/// records before an agent ever sees them. What was missing is this side: the
/// run accepted whatever refs a caller handed it, so an agent that had a
/// sensitive ref by any other route could cite it into an application and
/// nothing looked.
///
/// # It checks sensitivity, and deliberately not existence
///
/// A ref that resolves to nothing passes. That is not laxness — it is the
/// difference between the rule §5 states and a rule it never asked for. An
/// unresolvable ref may be agent-scoped task evidence, an artifact reference,
/// or a URL, and refusing those would break grounding for reasons the
/// sensitivity boundary has nothing to do with. What must not happen is a
/// record somebody marked private reaching a form, and that is exactly what
/// this refuses.
///
/// # A gate that resolves nothing says so
///
/// It keys on `evidence_id`. A caller whose refs are spelled some other way
/// would resolve none of them and be waved through by a check that looked like
/// it ran — the vacuous pass this codebase keeps finding. Nothing here can
/// decide which spelling is right, so it warns instead: resolving zero of what
/// it was given is a fact worth seeing, not one to infer from a quiet log.
///
/// # Unreadable is a refusal
///
/// `Err` means we could not check whether the citation is private, and "we
/// could not check" is not permission. It comes back as though every ref were
/// sensitive, which refuses the answer and says why.
async fn sensitive_evidence_refs(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    refs: &[String],
) -> Result<Vec<String>, String> {
    use magician::magician_v2::agents::memory::AgentMemoryService;
    use magician::magician_v2::evidence::is_sensitive;

    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let memory = AgentMemoryService::with_scoped_memory_scope_in_workspace(
        workspace_layout.clone(),
        principal,
        workspace,
    );
    let records = memory
        .load_user_work_evidence()
        .await
        .map_err(|error| format!("{error}"))?;

    let mut refused = Vec::new();
    let mut resolved = 0usize;
    for wanted in refs {
        let Some(record) = records
            .iter()
            .find(|record| record.evidence_id.as_str() == wanted.as_str())
        else {
            continue;
        };
        resolved += 1;
        if is_sensitive(&record.sensitivity) {
            refused.push(wanted.clone());
        }
    }

    // The one way this gate fails silently: it keys on `evidence_id`, and a
    // caller spelling its refs some other way would resolve NOTHING and be
    // waved through by a check that looked like it ran. Nothing here can decide
    // which spelling is right — but a gate that resolved none of what it was
    // given is a gate that checked nothing, and that must be visible rather
    // than inferred from a suspiciously quiet log.
    if resolved == 0 {
        tracing::warn!(
            "[WORK-MODULES] none of the {} cited evidence ref(s) resolved against \
             `{principal}/{workspace}`'s evidence, so the sensitivity boundary checked nothing \
             for this answer. Either the citations name evidence this scope does not hold, or \
             they are spelled differently from `evidence_id`.",
            refs.len()
        );
    }
    Ok(refused)
}

/// Refuse when any cited ref is private, or when we could not tell.
fn sensitivity_refusal(refused: &[String]) -> HttpResponse {
    api_error_response(
        StatusCode::CONFLICT,
        "evidence_not_clearable",
        format!(
            "these citations name evidence marked sensitive and may not be carried outward: {}. \
             Private material must not reach an application through an agent's judgment about \
             what is fine to share — raise a gap and let the owner decide instead",
            refused.join(", ")
        ),
        None,
    )
}

/// `POST /api/magician/v2/work/runs/{run_id}/answers`
///
/// Record a grounded answer. An answer citing no evidence is refused by the
/// store, never stored — the caller records a gap and asks the owner instead.
pub async fn record_answer_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AnswerRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    // §5's boundary, enforced on the real source. Checked BEFORE the store
    // writes: an answer recorded and then found to cite private material is
    // already in the run, and the run is what gets submitted.
    match sensitive_evidence_refs(
        &api.workspace_layout,
        &principal,
        &workspace,
        &body.evidence_refs,
    )
    .await
    {
        Ok(refused) if !refused.is_empty() => return sensitivity_refusal(&refused),
        Ok(_) => {},
        Err(detail) => {
            return api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "evidence_clearance_unreadable",
                format!(
                    "the evidence these citations name could not be read, so whether they are \
                     private is unknown — and unknown is not permission ({detail})"
                ),
                None,
            )
        },
    }

    let scope = RunScope::new(principal, workspace);
    let answer = Answer {
        text: body.text,
        evidence_refs: body.evidence_refs,
    };
    match api
        .runs()
        .record_answer(&scope, &run_id, &body.field, &answer, Utc::now())
    {
        Ok(run) => HttpResponse::Ok().json(json!({ "run": run, "state": run.state().as_str() })),
        Err(error) => refused("answer_refused", &error),
    }
}

/// `POST /api/magician/v2/work/runs/{run_id}/gaps`
///
/// Turn a question the evidence cannot ground into an owner-facing gap.
pub async fn raise_gap_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RaiseGapRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal, workspace);
    match api
        .runs()
        .raise_gap(&scope, &run_id, &body.field, &body.question, Utc::now())
    {
        Ok(run) => HttpResponse::Ok().json(json!({ "run": run, "state": run.state().as_str() })),
        Err(error) => refused("gap_refused", &error),
    }
}

/// `POST /api/magician/v2/work/runs/{run_id}/gaps/resolve`
///
/// **The owner answers.** §5: *"The owner's reply is new evidence"* — the
/// reply's own ref belongs in `evidence_refs`, which is what lets an
/// owner-supplied fact clear the same grounding bar as everything else.
pub async fn resolve_gap_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveGapRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    // §5's boundary, enforced on the real source. Checked BEFORE the store
    // writes: an answer recorded and then found to cite private material is
    // already in the run, and the run is what gets submitted.
    match sensitive_evidence_refs(
        &api.workspace_layout,
        &principal,
        &workspace,
        &body.evidence_refs,
    )
    .await
    {
        Ok(refused) if !refused.is_empty() => return sensitivity_refusal(&refused),
        Ok(_) => {},
        Err(detail) => {
            return api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "evidence_clearance_unreadable",
                format!(
                    "the evidence these citations name could not be read, so whether they are \
                     private is unknown — and unknown is not permission ({detail})"
                ),
                None,
            )
        },
    }

    let scope = RunScope::new(principal, workspace);
    let answer = Answer {
        text: body.text,
        evidence_refs: body.evidence_refs,
    };
    match api.runs().resolve_gap(
        &scope,
        &run_id,
        &body.field,
        &body.question,
        &answer,
        &body.resolved_by,
        Utc::now(),
    ) {
        Ok(run) => HttpResponse::Ok().json(json!({ "run": run, "state": run.state().as_str() })),
        Err(error) => refused("gap_resolution_refused", &error),
    }
}

/// `POST /api/magician/v2/work/runs/{run_id}/expectations`
///
/// Raise an out-of-band wait: the run is now waiting on an event that arrives
/// somewhere else. This surface watches nothing; `source_hint` is where whoever
/// does watch should look.
pub async fn raise_expectation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RaiseExpectationRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal, workspace);
    match api.runs().raise_expectation(
        &scope,
        &run_id,
        &body.description,
        &body.source_hint,
        Utc::now(),
    ) {
        Ok(expectation) => HttpResponse::Ok().json(json!({
            "expectation_id": expectation.expectation_id,
            "description": expectation.description,
            "source_hint": expectation.source_hint,
            "raised_at": expectation.raised_at,
        })),
        Err(error) => refused("expectation_refused", &error),
    }
}

/// `POST /api/magician/v2/work/runs/{run_id}/expectations/fulfil`
///
/// **The fulfil route.** An arrived event in, matched to one open wait by its
/// source hint, or matched to nothing at all.
///
/// The three outcomes are kept apart in the response because they mean
/// different things to the caller: `fulfilled` closed a wait, `already`
/// recognises a re-delivered event and wrote nothing, and `unmatched` is the
/// ordinary case of an inbox carrying something else — not a failure, and
/// answering `404` for it would make the loop unusable.
pub async fn fulfil_expectation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FulfilExpectationRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal, workspace);
    let inbound = InboundEvent {
        event_ref: body.event_ref,
        source_hint: body.source_hint,
        // The event's own time when the caller knows it, this instant
        // otherwise — never a stored "now" pretending to be the arrival.
        at: body.at.unwrap_or_else(Utc::now),
    };
    match fulfil_from_inbound(&api.runs(), &scope, &run_id, &inbound) {
        Ok(InboundOutcome::Fulfilled {
            expectation_id,
            run,
        }) => HttpResponse::Ok().json(json!({
            "outcome": "fulfilled",
            "expectation_id": expectation_id,
            "run": run,
            "state": run.state().as_str(),
        })),
        Ok(InboundOutcome::AlreadyFulfilled {
            expectation_id,
            run,
        }) => HttpResponse::Ok().json(json!({
            "outcome": "already_fulfilled",
            "expectation_id": expectation_id,
            "run": run,
            "state": run.state().as_str(),
        })),
        Ok(InboundOutcome::Unmatched) => HttpResponse::Ok().json(json!({
            "outcome": "unmatched",
            "detail": "nothing open on this run watches that source; nothing was written",
        })),
        Err(error) => refused("fulfilment_refused", &error),
    }
}

/// `POST /api/magician/v2/work/runs/{run_id}/submit`
///
/// **A person presses send.** The covering outward act is recorded on the
/// `Form` channel first, and only then does the run seal — an act prepared for
/// a run that refuses to submit would be a record of a submission that never
/// happened, and a sealed run with no recorded disclosure is a submission
/// nobody can account for.
pub async fn submit_run_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitRunRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_id = path.into_inner();
    if let Err(message) = guard_path_id("a run id", &run_id) {
        return bad_request(message);
    }
    let scope = RunScope::new(principal, workspace);
    let assertions = api.assertions();
    let act = FormSubmissionAct {
        assertions: &assertions,
        exact_payload_artifact_ref: &body.exact_payload_artifact_ref,
        recipients: &body.recipients,
    };
    match api
        .runs()
        .submit(&scope, &run_id, &body.by, &act, Utc::now())
    {
        Ok(run) => HttpResponse::Ok().json(json!({
            "run": run,
            "state": run.state().as_str(),
            "submission": run.submission,
        })),
        Err(error) => refused("submission_refused", &error),
    }
}

fn run_summary(run: &magician_media::run_state::Run) -> Value {
    let fields_pending = run
        .fields
        .iter()
        .filter(|held| held.answer.is_none())
        .count();
    let gaps_open = run.gaps.iter().filter(|gap| gap.is_open()).count();
    let expectations_open = run
        .expectations
        .iter()
        .filter(|held| held.is_open())
        .count();
    json!({
        "run_id": run.run_id,
        "purpose": run.purpose,
        "resource_ref": run.resource_ref,
        "audience": run.audience,
        "state": run.state().as_str(),
        "opened_at": run.opened_at,
        "opened_by": run.opened_by,
        "fields": run.fields.len(),
        "fields_pending": fields_pending,
        "gaps_open": gaps_open,
        "expectations_open": expectations_open,
    })
}

fn count_state(runs: &[magician_media::run_state::Run], wanted: &str) -> usize {
    runs.iter()
        .filter(|run| run.state().as_str() == wanted)
        .count()
}

// ---------------------------------------------------------------------------
// 2. Time negotiations — offer, absorb, hold, re-offer, close
// ---------------------------------------------------------------------------

/// A candidate time, as a caller states it.
#[derive(Debug, Deserialize)]
pub struct SlotRequest {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl SlotRequest {
    fn build(&self) -> Result<Slot, String> {
        Slot::new(self.start, self.end).map_err(|error| format!("{error:#}"))
    }
}

fn build_slots(slots: &[SlotRequest]) -> Result<Vec<Slot>, String> {
    slots.iter().map(SlotRequest::build).collect()
}

#[derive(Debug, Deserialize)]
pub struct OpenNegotiationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    pub counterparty: String,
    pub purpose: String,
    pub slots: Vec<SlotRequest>,
    /// The outward act that carried the offer, when the caller has already
    /// sent it. Optional because the send is the caller's and may not have
    /// happened yet — the offer text comes back from this route for exactly
    /// that case.
    #[serde(default)]
    pub offer_act_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReOfferRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    pub slots: Vec<SlotRequest>,
    #[serde(default)]
    pub offer_act_ref: Option<String>,
}

/// What the caller read out of an inbound message.
///
/// The reading is **supplied**, never inferred: turning prose into a verdict is
/// language work that belongs to whoever holds the conversation, and a parser
/// guessing "no problem" for a decline would put words in a counterparty's
/// mouth.
#[derive(Debug, Deserialize)]
#[serde(tag = "reading", rename_all = "snake_case")]
pub enum ReadingRequest {
    /// They took the time **starting** at this instant. A start rather than a
    /// whole slot: a reply names a time, and the range it belongs to is the one
    /// that was offered.
    Accepted {
        starting_at: DateTime<Utc>,
    },
    Declined {
        #[serde(default)]
        reason: Option<String>,
    },
    Countered {
        slots: Vec<SlotRequest>,
    },
}

#[derive(Debug, Deserialize)]
pub struct AbsorbReplyRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    /// The message or transcript this was read from. Required, and the store's
    /// idempotency key: a re-read inbox re-delivers the same source, and the
    /// second delivery must change nothing.
    pub source_ref: String,
    /// When they replied — the message's own time, never this surface's clock.
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub reading: ReadingRequest,
}

#[derive(Debug, Deserialize)]
pub struct HoldRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    /// The calendar event the caller's capability created. Required: without it
    /// the record cannot point at the outward act it claims exists.
    pub calendar_event_ref: String,
}

#[derive(Debug, Deserialize)]
pub struct ReasonedRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    pub reason: String,
}

/// `GET /api/magician/v2/work/negotiations`
///
/// Every negotiation in the scope, whatever relationship it lives in, with the
/// state each derives from its own contents.
///
/// A relationship filter is available through `audience_kind`/`audience_id`;
/// without one the whole book comes back, because an owner asking *"what am I
/// waiting on"* is asking across relationships and a per-audience read alone
/// would leave them looping.
pub async fn list_negotiations_handler(
    req: HttpRequest,
    query: web::Query<AudienceScopeQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = SchedulingScope::new(principal.clone(), workspace.clone());
    let store = api.scheduling();

    let negotiations = match (query.audience_kind.as_deref(), query.audience_id.as_deref()) {
        (Some(kind), Some(id)) => {
            let audience = match parse_audience(kind, id) {
                Ok(reference) => reference,
                Err(message) => return bad_request(message),
            };
            store.for_audience(&scope, &audience)
        },
        (None, None) => store.all_negotiations(&scope),
        _ => {
            return bad_request(
                "a relationship filter needs both `audience_kind` and `audience_id`, or \
                 neither; half a filter would silently widen to the whole book",
            )
        },
    };
    let negotiations = match negotiations {
        Ok(held) => held,
        Err(error) => return store_unreadable("the negotiation logs", &error),
    };

    let now = Utc::now();
    let summaries: Vec<Value> = negotiations.iter().map(negotiation_summary).collect();
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        "counts": {
            "total": negotiations.len(),
            "awaiting_reply": count_negotiations(&negotiations, "awaiting_reply"),
            "accepted": count_negotiations(&negotiations, "accepted"),
            "declined": count_negotiations(&negotiations, "declined"),
            "countered": count_negotiations(&negotiations, "countered"),
            "held": count_negotiations(&negotiations, "held"),
            "closed": count_negotiations(&negotiations, "closed"),
        },
        "negotiations": summaries,
    }))
}

/// `POST /api/magician/v2/work/negotiations`
///
/// Open an ask and record its offer. The response carries the **offer
/// message** — words and times, and no channel — so whatever send capability
/// the conversation is on can carry it. This surface sends nothing.
pub async fn open_negotiation_handler(
    req: HttpRequest,
    body: web::Json<OpenNegotiationRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    // Both reach the negotiation id derivation, which the store does not guard.
    for (label, value) in [
        ("the counterparty", body.counterparty.as_str()),
        ("the purpose", body.purpose.as_str()),
    ] {
        if let Err(message) = guard_id_component(label, value) {
            return bad_request(message);
        }
    }
    let slots = match build_slots(&body.slots) {
        Ok(slots) => slots,
        Err(message) => return bad_request(message),
    };

    let scope = SchedulingScope::new(principal.clone(), workspace.clone());
    match api.scheduling().open(
        &scope,
        &audience,
        &body.counterparty,
        &body.purpose,
        &slots,
        body.offer_act_ref.clone(),
        Utc::now(),
    ) {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
            "offer_message": offer_message_json(&negotiation),
        })),
        Err(error) => refused("negotiation_refused", &error),
    }
}

/// `GET /api/magician/v2/work/negotiations/{negotiation_id}`
///
/// One negotiation, plus the two intents the consumer derives from it: the
/// **offer to send** when one is genuinely standing and unanswered, and the
/// **hold to book** when they have accepted a time.
///
/// Both come back as `null` with a `refusal` string rather than being omitted,
/// because *"there is nothing to send"* and *"you must not send from this
/// state"* are the same absence to a client and very different facts to a
/// person. The refusal is the module's own words.
pub async fn get_negotiation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AudienceScopeQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match required_audience(&query) {
        Ok(reference) => reference,
        Err(response) => return response,
    };
    let scope = SchedulingScope::new(principal.clone(), workspace.clone());
    let negotiation = match api.scheduling().load(&scope, &audience, &negotiation_id) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return not_found(
                "negotiation_not_found",
                format!(
                    "no negotiation `{negotiation_id}` on `{}`",
                    audience.as_key()
                ),
            )
        },
        Err(error) => return store_unreadable("the negotiation log", &error),
    };

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": Utc::now(),
        "negotiation": negotiation,
        "state": negotiation.state().as_str(),
        "standing_slots": negotiation.standing_slots(),
        "offer_message": offer_message_json(&negotiation),
        "hold_intent": hold_intent_json(&negotiation),
    }))
}

/// `POST /api/magician/v2/work/negotiations/{negotiation_id}/reply`
///
/// Absorb what came back. Idempotent on `source_ref` — the store remembers
/// every source ever absorbed on the ask, across round resets and reopened
/// generations, so a re-read inbox changes nothing.
pub async fn absorb_reply_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AbsorbReplyRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let reading = match &body.reading {
        ReadingRequest::Accepted { starting_at } => {
            InboundReading::AcceptedStartingAt(*starting_at)
        },
        ReadingRequest::Declined { reason } => InboundReading::Declined {
            reason: reason.clone(),
        },
        ReadingRequest::Countered { slots } => match build_slots(slots) {
            Ok(slots) => InboundReading::Countered { slots },
            Err(message) => return bad_request(message),
        },
    };
    let inbound = InboundReply {
        source_ref: body.source_ref,
        at: body.at,
        reading,
    };
    let scope = SchedulingScope::new(principal, workspace);
    match absorb_reply(
        &api.scheduling(),
        &scope,
        &audience,
        &negotiation_id,
        &inbound,
    ) {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
            "standing_slots": negotiation.standing_slots(),
            "hold_intent": hold_intent_json(&negotiation),
        })),
        Err(error) => refused("reply_refused", &error),
    }
}

/// `POST /api/magician/v2/work/negotiations/{negotiation_id}/re-offer`
///
/// Our answer to their decline or counter: new times **replace** the standing
/// ones. Without this transition the negotiation wedges — a changed offer
/// lands nowhere and their acceptance of it is refused as an agreement that
/// never happened.
pub async fn re_offer_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReOfferRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let slots = match build_slots(&body.slots) {
        Ok(slots) => slots,
        Err(message) => return bad_request(message),
    };
    let scope = SchedulingScope::new(principal, workspace);
    match api.scheduling().re_offer(
        &scope,
        &audience,
        &negotiation_id,
        &slots,
        body.offer_act_ref.clone(),
        Utc::now(),
    ) {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
            "offer_message": offer_message_json(&negotiation),
        })),
        Err(error) => refused("re_offer_refused", &error),
    }
}

/// `POST /api/magician/v2/work/negotiations/{negotiation_id}/hold`
///
/// File the calendar event the caller's capability created against the slot
/// **they accepted**. The slot is never taken from the request: it is read off
/// the acceptance, so a booking cannot land on a time nobody agreed to.
pub async fn hold_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<HoldRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let scope = SchedulingScope::new(principal, workspace);
    let store = api.scheduling();
    let negotiation = match store.load(&scope, &audience, &negotiation_id) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return not_found(
                "negotiation_not_found",
                format!(
                    "no negotiation `{negotiation_id}` on `{}`",
                    audience.as_key()
                ),
            )
        },
        Err(error) => return store_unreadable("the negotiation log", &error),
    };
    let intent = match hold_intent(&negotiation) {
        Ok(intent) => intent,
        Err(error) => return refused("hold_refused", &error),
    };
    match record_hold(
        &store,
        &scope,
        &intent,
        &body.calendar_event_ref,
        Utc::now(),
    ) {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
            "held": negotiation.held,
        })),
        Err(error) => refused("hold_refused", &error),
    }
}

/// `POST /api/magician/v2/work/negotiations/{negotiation_id}/reschedule`
///
/// A booked time died. The round resets and a fresh offer is owed; the dead
/// times and their reason stay in the history.
pub async fn reschedule_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReasonedRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let scope = SchedulingScope::new(principal, workspace);
    match api
        .scheduling()
        .reschedule(&scope, &audience, &negotiation_id, &body.reason, Utc::now())
    {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
        })),
        Err(error) => refused("reschedule_refused", &error),
    }
}

/// `POST /api/magician/v2/work/negotiations/{negotiation_id}/close`
///
/// End the current **round**, not the identity: a fresh open on the same ask
/// afterwards starts the next round under the same id, and the closed round
/// stays in the log.
pub async fn close_negotiation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReasonedRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let negotiation_id = path.into_inner();
    if let Err(message) = guard_path_id("a negotiation id", &negotiation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let scope = SchedulingScope::new(principal, workspace);
    match api
        .scheduling()
        .close(&scope, &audience, &negotiation_id, &body.reason, Utc::now())
    {
        Ok(negotiation) => HttpResponse::Ok().json(json!({
            "negotiation": negotiation,
            "state": negotiation.state().as_str(),
            "closed": negotiation.closed,
        })),
        Err(error) => refused("close_refused", &error),
    }
}

fn required_audience(query: &AudienceScopeQuery) -> Result<AudienceRef, HttpResponse> {
    let (Some(kind), Some(id)) = (query.audience_kind.as_deref(), query.audience_id.as_deref())
    else {
        return Err(bad_request(
            "this route needs `audience_kind` and `audience_id`: the store keeps one log per \
             relationship, and a negotiation id alone cannot say which log to open",
        ));
    };
    parse_audience(kind, id).map_err(bad_request)
}

fn negotiation_summary(negotiation: &Negotiation) -> Value {
    json!({
        "negotiation_id": negotiation.negotiation_id,
        "audience": negotiation.audience,
        "counterparty": negotiation.counterparty,
        "purpose": negotiation.purpose,
        "state": negotiation.state().as_str(),
        "offered_at": negotiation.offered_at,
        "offer_act_ref": negotiation.offer_act_ref,
        "standing_slots": negotiation.standing_slots(),
        "replies": negotiation.replies.len(),
        "reschedules": negotiation.reschedules.len(),
        "held": negotiation.held,
        "closed": negotiation.closed,
    })
}

/// The offer to send, or the module's own words for why there is none.
fn offer_message_json(negotiation: &Negotiation) -> Value {
    match offer_message(negotiation) {
        Ok(message) => json!({
            "negotiation_id": message.negotiation_id,
            "audience": message.audience,
            "counterparty": message.counterparty,
            "purpose": message.purpose,
            "slots": message.slots,
            "offer_act_ref": message.offer_act_ref,
            "offered_at": message.offered_at,
            "body": message.body,
        }),
        Err(error) => json!({ "refusal": format!("{error:#}") }),
    }
}

/// What to book, or the module's own words for why there is nothing to book.
fn hold_intent_json(negotiation: &Negotiation) -> Value {
    match hold_intent(negotiation) {
        Ok(intent) => json!({
            "negotiation_id": intent.negotiation_id,
            "audience": intent.audience,
            "counterparty": intent.counterparty,
            "purpose": intent.purpose,
            "slot": intent.slot,
        }),
        Err(error) => json!({ "refusal": format!("{error:#}") }),
    }
}

fn count_negotiations(negotiations: &[Negotiation], wanted: &str) -> usize {
    negotiations
        .iter()
        .filter(|held| held.state().as_str() == wanted)
        .count()
}

// ---------------------------------------------------------------------------
// 3. Claim manifests — what a built revision claims, and what still carries it
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BindManifestRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub artifact_ref: String,
    pub revision_ref: String,
    /// One binding per claim. A binding with no evidence refs is refused: a
    /// claim in an artifact that cannot cite evidence is an invented figure
    /// with a slide layout.
    pub claims: Vec<ClaimBindingRequest>,
    pub bound_by: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaimBindingRequest {
    pub claim_ref: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ManifestQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub artifact_ref: Option<String>,
    #[serde(default)]
    pub revision_ref: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CarryingQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub claim_ref: Option<String>,
    /// `true` narrows to artifacts whose LATEST bound revision still carries
    /// the claim — the "still carrying it now" question, which is what a
    /// correction actually chases. Default `false` is the full history:
    /// "carried it once".
    #[serde(default)]
    pub latest_only: bool,
}

/// `POST /api/magician/v2/work/claim-manifests`
///
/// Bind a revision's claim set — the one write this store has. A revision's
/// claims are immutable: an identical rebind resumes, a different one is
/// refused, and the fix is a new revision.
pub async fn bind_manifest_handler(
    req: HttpRequest,
    body: web::Json<BindManifestRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // Both reach the manifest id derivation, which the store does not guard.
    for (label, value) in [
        ("an artifact ref", body.artifact_ref.as_str()),
        ("a revision ref", body.revision_ref.as_str()),
    ] {
        if let Err(message) = guard_id_component(label, value) {
            return bad_request(message);
        }
    }
    let claims: Vec<ClaimBinding> = body
        .claims
        .into_iter()
        .map(|binding| ClaimBinding {
            claim_ref: binding.claim_ref,
            evidence_refs: binding.evidence_refs,
        })
        .collect();

    let scope = ClaimManifestScope::new(principal.clone(), workspace.clone());
    match api.manifests().bind(
        &scope,
        &body.artifact_ref,
        &body.revision_ref,
        claims,
        &body.bound_by,
        Utc::now(),
    ) {
        Ok(manifest) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "manifest": manifest,
        })),
        Err(error) => refused("manifest_refused", &error),
    }
}

/// `GET /api/magician/v2/work/claim-manifests?artifact_ref=&revision_ref=`
///
/// The revision history of what an artifact claimed, oldest first — or one
/// revision's manifest when `revision_ref` is given.
pub async fn read_manifests_handler(
    req: HttpRequest,
    query: web::Query<ManifestQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(artifact_ref) = query.artifact_ref.as_deref() else {
        return bad_request(
            "this route needs `artifact_ref`: manifests are keyed by the artifact they \
             describe, and a listing with no artifact would have to scan every log in the scope",
        );
    };
    if let Err(message) = guard_id_component("an artifact ref", artifact_ref) {
        return bad_request(message);
    }
    let scope = ClaimManifestScope::new(principal.clone(), workspace.clone());
    let store = api.manifests();

    if let Some(revision_ref) = query.revision_ref.as_deref() {
        if let Err(message) = guard_id_component("a revision ref", revision_ref) {
            return bad_request(message);
        }
        return match store.manifest_for(&scope, artifact_ref, revision_ref) {
            Ok(Some(manifest)) => HttpResponse::Ok().json(json!({
                "principal": principal,
                "workspace": workspace,
                "manifest": manifest,
            })),
            Ok(None) => not_found(
                "manifest_not_found",
                format!("revision `{revision_ref}` of `{artifact_ref}` has no bound manifest"),
            ),
            Err(error) => store_unreadable("the manifest log", &error),
        };
    }

    match store.manifests_for_artifact(&scope, artifact_ref) {
        Ok(manifests) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "artifact_ref": artifact_ref,
            "counts": { "revisions": manifests.len() },
            "manifests": manifests,
        })),
        Err(error) => store_unreadable("the manifest log", &error),
    }
}

/// `GET /api/magician/v2/work/claim-manifests/carrying?claim_ref=&latest_only=`
///
/// **The correction-propagation query.** When a claim is corrected, these are
/// the decks and drafts still carrying it — exactly what the sent-record
/// cannot find, because nothing has been sent yet.
pub async fn revisions_carrying_handler(
    req: HttpRequest,
    query: web::Query<CarryingQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(claim_ref) = query.claim_ref.as_deref() else {
        return bad_request(
            "this route needs `claim_ref`: it is served by the per-claim reverse index, and a \
             query with no claim would have nothing to look up",
        );
    };
    if let Err(message) = guard_id_component("a claim ref", claim_ref) {
        return bad_request(message);
    }
    let scope = ClaimManifestScope::new(principal.clone(), workspace.clone());
    let store = api.manifests();

    if query.latest_only {
        return match store.latest_revision_carrying(&scope, claim_ref) {
            Ok(artifacts) => HttpResponse::Ok().json(json!({
                "principal": principal,
                "workspace": workspace,
                "claim_ref": claim_ref,
                "counts": { "artifacts": artifacts.len() },
                "artifacts": artifacts,
            })),
            Err(error) => store_unreadable("the claim index", &error),
        };
    }

    match store.revisions_carrying(&scope, claim_ref) {
        Ok(revisions) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "claim_ref": claim_ref,
            "counts": { "revisions": revisions.len() },
            "revisions": revisions,
        })),
        Err(error) => store_unreadable("the claim index", &error),
    }
}

// ---------------------------------------------------------------------------
// 4. The register — what is owed, what lapsed, and the sweep that fills it
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct RegisterQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// `owed_by_us` or `owed_to_us`. Absent means both — kept apart in the
    /// response either way, because a promise WE broke and a reply THEY owe
    /// need different words and different urgency.
    #[serde(default)]
    pub direction: Option<String>,
    /// `true` narrows to what is past its deadline and unsettled.
    #[serde(default)]
    pub lapsed_only: bool,
    /// `true` includes settled rows. Default `false`, because the question a
    /// cycle asks is what is still owed.
    #[serde(default)]
    pub include_settled: bool,
}

#[derive(Debug, Deserialize)]
pub struct RecordObligationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    #[serde(default)]
    pub program_id: Option<String>,
    /// What was promised, in the words it was promised in.
    pub what: String,
    pub due_at: DateTime<Utc>,
    /// `owed_by_us` or `owed_to_us`. No default: a lapse means two different
    /// things and one that guessed would surface *"you are late"* for
    /// something the other side owes.
    pub direction: String,
    pub created_by: String,
    #[serde(default)]
    pub source_act_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SettleObligationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub audience_kind: String,
    pub audience_id: String,
    /// `met` — it was done — or `released` — it stopped applying. Distinct
    /// because a register that conflated them would report a hit rate that was
    /// partly wishful.
    pub settlement: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SweepRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default = "default_silence_window_hours")]
    pub silence_window_hours: i64,
    #[serde(default = "default_delivery_question_hours")]
    pub delivery_question_after_hours: i64,
    #[serde(default = "default_follow_up_hours")]
    pub follow_up_after_hours: i64,
}

fn default_silence_window_hours() -> i64 {
    72
}

fn default_delivery_question_hours() -> i64 {
    48
}

fn default_follow_up_hours() -> i64 {
    120
}

fn parse_direction(label: &str) -> Result<ObligationDirection, String> {
    match label.trim().to_ascii_lowercase().as_str() {
        "owed_by_us" => Ok(ObligationDirection::OwedByUs),
        "owed_to_us" => Ok(ObligationDirection::OwedToUs),
        other => Err(format!(
            "`{other}` is not a direction; it is `owed_by_us` or `owed_to_us`. An unrecognised \
             direction is refused rather than defaulted, because a lapse means two different \
             things and one list that read alike for both would train the owner to skim it"
        )),
    }
}

/// `GET /api/magician/v2/work/obligations`
///
/// **The owner read, and the one path an agent's cycle surfaces obligations
/// by.** What is owed and what lapsed, across every relationship in the scope,
/// split by direction — and, per row, what the counterparty actually did.
///
/// Lapsing is derived from the clock on this read, never stored: the register
/// must not depend on a sweep having run, because catching what nobody
/// remembered is the entire job.
///
/// # The reading beside the row
///
/// The register's `what` is part of its identity tuple, so it must be
/// byte-stable across sweeps — which is why a row reads *"opened, no reply"*
/// identically for somebody who glanced once and somebody who came back five
/// times and never reached the second document. `data_room::cycle` composes
/// that distinction and had no caller anywhere. It is joined on here, by
/// `obligation_id`, so the cycle reads one list in one order with one idea of
/// what is outstanding — never a second queue, which is how two sources of
/// truth start disagreeing.
///
/// The reading is **derived on demand and never recorded**: it carries a visit
/// count, and a count inside the recorded text would mint a fresh obligation on
/// every visit.
///
/// # What its absence means
///
/// `reading` is present on a row only when a reading explains it. When the
/// rooms could not be read at all, `readings.available` is `false` and carries
/// the reason — because "no rooms need attention" out of a disk fault is the
/// most reassuring wrong answer this surface could give. The register itself is
/// still answered: an explanation that fails must not take the primary read
/// down with it.
pub async fn read_register_handler(
    req: HttpRequest,
    query: web::Query<RegisterQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let direction = match query.direction.as_deref().map(parse_direction).transpose() {
        Ok(direction) => direction,
        Err(message) => return bad_request(message),
    };
    let scope = ObligationScope::new(principal.clone(), workspace.clone());
    let store = api.register();

    let rows = match (query.audience_kind.as_deref(), query.audience_id.as_deref()) {
        (Some(kind), Some(id)) => {
            let audience = match parse_audience(kind, id) {
                Ok(reference) => reference,
                Err(message) => return bad_request(message),
            };
            store.for_audience(&scope, &audience)
        },
        (None, None) => store.all_obligations(&scope),
        _ => {
            return bad_request(
                "a relationship filter needs both `audience_kind` and `audience_id`, or \
                 neither; half a filter would silently widen to the whole register",
            )
        },
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => return store_unreadable("the obligation register", &error),
    };

    let now = Utc::now();

    // §6's own sentence — "opened the deck three times, never opened the
    // financials" — for the rows this read already loaded.
    //
    // Joined against the UNFILTERED `rows`: a direction or lapsed-only filter
    // is a display choice, and letting it decide that a reading addresses
    // nothing would turn that choice into an alarm about a sweep that has not
    // run.
    let readings = api
        .follow_up_policy()
        .and_then(|policy| attention_notes_for_scope(&api.workspace_layout, &scope, &policy, now))
        .map(|notes| explain_register(&rows, notes));

    let selected: Vec<&Obligation> = rows
        .iter()
        .filter(|held| direction.is_none_or(|want| held.direction == want))
        .filter(|held| !query.lapsed_only || held.state(now) == ObligationState::Lapsed)
        .filter(|held| query.include_settled || held.is_outstanding(now))
        .collect();
    let listed: Vec<Value> = selected
        .into_iter()
        .map(|held| {
            let mut row = obligation_json(held, now);
            // The key appears only when a reading actually explains this row. A
            // `null` would say "nothing to report" in the one case — the rooms
            // were unreadable — where the honest answer is "unknown", and
            // `readings.available` is what tells those apart.
            if let Ok(readings) = readings.as_ref() {
                if let Some(note) = readings.for_obligation(&held.obligation_id) {
                    if let (Some(fields), Ok(reading)) =
                        (row.as_object_mut(), serde_json::to_value(note))
                    {
                        fields.insert("reading".to_string(), reading);
                    }
                }
            }
            row
        })
        .collect();
    let readings = match readings {
        Ok(readings) => json!({
            "available": true,
            // The windows the readings were derived under, stated rather than
            // implied: they are what a follow-up's `due_at` is built from, so a
            // caller comparing them against the sweep's own windows can see why
            // a handle did not land.
            "delivery_question_after_hours": api.sweep_config.delivery_question_after_hours,
            "follow_up_after_hours": api.sweep_config.follow_up_after_hours,
            // Counts, never rates. "Three rows explained, one reading with no
            // row" is a fact an owner can reconcile against the register in
            // front of them; a percentage hides which rows moved.
            "counts": {
                "explaining_a_row": readings.attached.len(),
                "row_not_in_register": readings.row_not_in_register.len(),
                "not_yet_raised": readings.not_yet_raised.len(),
            },
            // A ripe reading the register holds no row for: the sweep has not
            // run since it ripened, or it ran under different windows. Carried
            // rather than dropped — a silently missing explanation reads
            // exactly like a room nobody looked at.
            "row_not_in_register": readings.row_not_in_register,
            // Nothing is owed and nothing is wrong: they replied, or the
            // waiting window has not closed. Carried so a cycle can tell a live
            // conversation from a quiet one rather than inferring silence from
            // the absence of a note.
            "not_yet_raised": readings.not_yet_raised,
        }),
        // Never zeroed counts. The rooms could not be read, and "nobody is
        // waiting on us" out of a disk fault is the single most reassuring
        // wrong answer this surface could give.
        Err(error) => json!({
            "available": false,
            "error": "what the counterparty did could not be read, so it is unknown — and \
                      unknown is not `none`",
            "detail": format!("{error:#}"),
        }),
    };
    // Counted apart, and over the WHOLE register: a promise WE broke and a
    // reply THEY owe need different words and different urgency.
    let lapsed_ours = count_direction(&rows, now, ObligationDirection::OwedByUs);
    let lapsed_theirs = count_direction(&rows, now, ObligationDirection::OwedToUs);

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        // Counts over the WHOLE register, never over the filtered view: a
        // filtered tally would report "nothing lapsed" to a caller who asked
        // for one direction, which is the answer they must not be given.
        "counts": {
            "total": rows.len(),
            "open": count_register(&rows, now, ObligationState::Open),
            "lapsed": count_register(&rows, now, ObligationState::Lapsed),
            "met": count_register(&rows, now, ObligationState::Met),
            "released": count_register(&rows, now, ObligationState::Released),
            "lapsed_owed_by_us": lapsed_ours,
            "lapsed_owed_to_us": lapsed_theirs,
        },
        "obligations": listed,
        // What the counterparty did, keyed to the rows above. See the handler
        // note on why an absent `reading` is not a claim that nothing happened.
        "readings": readings,
    }))
}

/// `POST /api/magician/v2/work/obligations`
///
/// Record a promise by hand — the owner noting *"they said Friday"*, or any
/// flow that derives an obligation and has nowhere else to put it. Idempotent
/// on `(audience, what, due_at, direction)`, so the same promise noticed twice
/// is one row.
pub async fn record_obligation_handler(
    req: HttpRequest,
    body: web::Json<RecordObligationRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let direction = match parse_direction(&body.direction) {
        Ok(direction) => direction,
        Err(message) => return bad_request(message),
    };
    // `what` reaches the register's id derivation, which the store does not
    // guard.
    if let Err(message) = guard_id_component("the promise text", &body.what) {
        return bad_request(message);
    }
    let request = RecordObligation {
        audience,
        program_id: body.program_id.clone(),
        what: body.what.clone(),
        due_at: body.due_at,
        direction,
        created_by: body.created_by.clone(),
        source_act_ref: body.source_act_ref.clone(),
    };
    let scope = ObligationScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();
    match api.register().record(&scope, &request, now) {
        Ok(obligation) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "obligation": obligation_json(&obligation, now),
        })),
        Err(error) => refused("obligation_refused", &error),
    }
}

/// `POST /api/magician/v2/work/obligations/{obligation_id}/settle`
///
/// Done, or no longer applicable. Idempotent: the first settlement is the
/// settlement, because *"we did it on Tuesday"* is a fact a second call must
/// not move.
pub async fn settle_obligation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SettleObligationRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let obligation_id = path.into_inner();
    if let Err(message) = guard_path_id("an obligation id", &obligation_id) {
        return bad_request(message);
    }
    let audience = match parse_audience(&body.audience_kind, &body.audience_id) {
        Ok(reference) => reference,
        Err(message) => return bad_request(message),
    };
    let settlement = match body.settlement.trim().to_ascii_lowercase().as_str() {
        "met" => Settlement::Met {
            note: body.note.clone(),
        },
        "released" => {
            let Some(reason) = body
                .reason
                .as_deref()
                .filter(|held| !held.trim().is_empty())
            else {
                return bad_request(
                    "a released obligation must say why it stopped applying; `met` and \
                     `released` are different facts, and an unexplained release is one nobody \
                     can check later",
                );
            };
            Settlement::Released {
                reason: reason.to_string(),
            }
        },
        other => {
            return bad_request(format!(
                "`{other}` is not a settlement; it is `met` — it was done — or `released` — it \
                 stopped applying. Conflating them would report a hit rate that was partly \
                 wishful"
            ))
        },
    };
    let scope = ObligationScope::new(principal, workspace);
    let now = Utc::now();
    match api
        .register()
        .settle(&scope, &audience, &obligation_id, settlement, now)
    {
        Ok(obligation) => {
            HttpResponse::Ok().json(json!({ "obligation": obligation_json(&obligation, now) }))
        },
        Err(error) => refused("settlement_refused", &error),
    }
}

/// `POST /api/magician/v2/work/obligations/sweep`
///
/// Run the sweeps for this scope now — the on-demand twin of the worker.
///
/// The worker is what makes the register honest; this route is what makes it
/// checkable. It passes an **unseeded** memory, which the derivations
/// document as the honest form of *"first sweep"*: it records every ripened
/// obligation and settles only what the current facts themselves say is
/// settled, never anything inferred from a relationship's absence. A one-shot
/// caller has no previous view, and pretending otherwise is how a whole
/// register gets released in one call.
pub async fn sweep_register_handler(
    req: HttpRequest,
    body: web::Json<SweepRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let follow_up = match FollowUpPolicy::new(
        Duration::hours(body.delivery_question_after_hours),
        Duration::hours(body.follow_up_after_hours),
    ) {
        Ok(policy) => policy,
        Err(error) => return bad_request(format!("{error:#}")),
    };
    let policy = match SweepPolicy::new(Duration::hours(body.silence_window_hours), follow_up) {
        Ok(policy) => policy,
        Err(error) => return bad_request(format!("{error:#}")),
    };
    let scope = ObligationScope::new(principal.clone(), workspace.clone());
    let now = Utc::now();
    let memory_key = (principal.clone(), workspace.clone());

    // The lock is held across the sweep on purpose. Two concurrent sweeps of one
    // scope reading the same previous view would each derive settlements from
    // it and each write a memory the other never saw, so the second write would
    // silently discard the first sweep's observations. Serialising them makes
    // the second call see what the first did, which is the entire property this
    // memory exists for. Sweeps are per-scope and infrequent; contention here is
    // two operators pressing the same button.
    let mut memories = match api.sweep_memory.lock() {
        Ok(guard) => guard,
        // A poisoned lock means a previous sweep panicked mid-derivation, so
        // the memory behind it describes a view no sweep completed. Take it
        // anyway and REPLACE it: an unseeded first sweep is always a safe
        // answer, and refusing the request would leave the register permanently
        // unsweepable for one bad call.
        Err(poisoned) => {
            warn!(
                "[WORK-MODULES] the sweep memory was poisoned by an earlier panic; \
                 treating this scope as never swept"
            );
            let mut guard = poisoned.into_inner();
            guard.remove(&memory_key);
            guard
        },
    };
    let previous = memories
        .get(&memory_key)
        .cloned()
        .unwrap_or_else(SweepMemory::unseeded);

    match sweep_scope(&api.workspace_layout, &scope, &previous, &policy, now) {
        Ok((report, memory)) => {
            let first_sweep = !memories.contains_key(&memory_key);
            memories.insert(memory_key, memory);
            drop(memories);
            HttpResponse::Ok().json(json!({
                "principal": principal,
                "workspace": workspace,
                "as_of": now,
                "negotiations_seen": report.negotiations_seen,
                "rooms_seen": report.rooms_seen,
                "recorded": report.recorded,
                "settled": report.settled,
                "absent": report.absent,
                // Reported, because the two answers mean different things and
                // look identical: a first sweep settles nothing derived from
                // absence BY DESIGN, and a caller who does not know this was a
                // first sweep will read that zero as "nothing is finished".
                "first_sweep": first_sweep,
            }))
        },
        Err(error) => store_unreadable("the activity this sweep reads", &error),
    }
}

fn obligation_json(obligation: &Obligation, now: DateTime<Utc>) -> Value {
    // Seconds, because a `chrono::Duration` has no JSON shape of its own —
    // and `None` when it is not overdue, which is a fact rather than a zero.
    let overdue_by_seconds = obligation.overdue_by(now).map(|held| held.num_seconds());
    json!({
        "obligation_id": obligation.obligation_id,
        "audience": obligation.audience,
        "program_id": obligation.program_id,
        "what": obligation.what,
        "due_at": obligation.due_at,
        "direction": obligation.direction.as_str(),
        "state": obligation.state(now).as_str(),
        // Whether a lapse is OURS drives how it is surfaced, so the two never
        // read alike: a broken promise of ours and an unanswered request of
        // theirs need different words and different urgency.
        "lapse_is_ours": obligation.direction.lapse_is_ours(),
        "overdue_by_seconds": overdue_by_seconds,
        "created_at": obligation.created_at,
        "created_by": obligation.created_by,
        "source_act_ref": obligation.source_act_ref,
        "settled_at": obligation.settled_at,
        "settlement": obligation.settlement,
    })
}

fn count_register(rows: &[Obligation], now: DateTime<Utc>, wanted: ObligationState) -> usize {
    rows.iter().filter(|held| held.state(now) == wanted).count()
}

/// Lapsed rows in one direction.
///
/// Separate from [`count_register`] because the split is the point: one tally
/// that read alike for *"we are late"* and *"they have not replied"* would
/// train the owner to skim the register.
fn count_direction(rows: &[Obligation], now: DateTime<Utc>, wanted: ObligationDirection) -> usize {
    rows.iter()
        .filter(|held| held.state(now) == ObligationState::Lapsed && held.direction == wanted)
        .count()
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Mount every work-module route under the caller's scope.
///
/// A second `.route` on the same literal is how this crate registers a second
/// method: `ServiceConfig::route` moves the method guard onto the resource, so
/// a non-matching method falls through to the next registration rather than
/// answering 405 from the first.
#[derive(Debug, Deserialize)]
pub struct ReplyTargetQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The relationship, when the caller already knows it.
    #[serde(default)]
    pub audience_kind: Option<String>,
    #[serde(default)]
    pub audience_id: Option<String>,
    /// The channel and address the reply arrived on, when the caller does not.
    ///
    /// This is the form whoever RECEIVES a reply actually has, and the reason
    /// the route accepts it: an address and a channel, not a relationship.
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channel_address: Option<String>,
    #[serde(default)]
    pub channel_address_kind: Option<String>,
    /// A caller may say the channel did NOT prove its sender. It may never say
    /// that it did. Escalation is exactly the hole §9 step 6 exists to keep
    /// closed: it used to be read from the query string and defaulted to TRUE.
    #[serde(default)]
    pub channel_verified: Option<bool>,
}

/// The relationship a reply belongs to: named outright, or resolved from the
/// address it arrived on.
///
/// # The address form confers nothing on its own
///
/// It goes through the same two proofs `/chat/active` uses, built the same way.
/// The verification is **server-derived** — the outer boundary's own fact, with
/// a caller claim that may only DE-escalate. A caller that could assert its own
/// verification could point a forged `From:` header at a counterparty's open
/// asks, which is §9 step 6's hole exactly.
///
/// Then `engagement_lane_in_process` requires `identification.authority()` —
/// `Some` for one state only, where the channel established who sent it AND an
/// owner has proved the address reaches that organisation. `ContextOnly` is a
/// known address on an unproved message and answers `None`, and `None` is never
/// permission. Every failure lands on "cannot resolve", which is a refusal.
async fn resolve_reply_audience(
    req: &HttpRequest,
    principal: &str,
    workspace: &str,
    query: &ReplyTargetQuery,
) -> Result<AudienceRef, HttpResponse> {
    use magician::magician_v2::chat::inbound_authority::engagement_lane_in_process;
    use magician::magician_v2::chat::inbound_sender::identify_inbound_sender_in_process;
    use magician::magician_v2::counterparties::{IdentityKind, InboundVerification};

    // Named outright wins. A caller that knows the relationship is not asking
    // this route to guess at one.
    if let (Some(kind), Some(id)) = (query.audience_kind.as_deref(), query.audience_id.as_deref()) {
        return parse_audience(kind, id).map_err(bad_request);
    }
    let (Some(channel), Some(address)) =
        (query.channel.as_deref(), query.channel_address.as_deref())
    else {
        return Err(bad_request(
            "give either `audience_kind` + `audience_id`, or the `channel` + `channel_address` \
             the reply arrived on. With neither there is no relationship to look in, and \
             answering over every relationship would hand one counterparty's asks to another",
        ));
    };

    // Exactly what `identify_inbound_sender_for_query` builds — the boundary's
    // own fact, then the caller's claim, which may only de-escalate. NOT
    // `channel_is_verified`: that is envoy's ROUTING question and folding it in
    // here would make the register's verification a third thing, stricter than
    // the path this is supposed to mirror. Two notions of "verified" that must
    // agree and do not share code disagree eventually.
    let verified =
        InboundVerification::from_boundary(crate::chat_api::request_is_authenticated(req))
            .with_caller_claim(query.channel_verified);
    let sender = match identify_inbound_sender_in_process(
        principal,
        workspace,
        channel,
        address,
        query
            .channel_address_kind
            .as_deref()
            .and_then(IdentityKind::parse),
        verified,
        Utc::now(),
    ) {
        Ok(sender) => sender,
        // Unreadable is a refusal, never a stranger. Folding it would report a
        // broken register as a counterparty we have never heard of.
        Err(error) => return Err(store_unreadable("the counterparty register", &error)),
    };

    let outcome = engagement_lane_in_process(principal, workspace, &sender, Utc::now()).await;
    match outcome.into_lane() {
        Some(lane) => Ok(AudienceRef::engagement(lane.engagement_id())),
        None => Err(api_error_response(
            StatusCode::NOT_FOUND,
            "no_engagement_for_sender",
            "this address does not authoritatively reach an engagement, so there is no \
             relationship to look in. An address that is merely on file is not proof the sender \
             owns it, and a reply matched on one would file a stranger's words against a \
             counterparty's ask",
            None,
        )),
    }
}

/// `GET /api/magician/v2/work/negotiations/open-ask`
///
/// Which scheduling ask, if any, a reply from this relationship belongs to.
///
/// §3's first non-skippable item: *"the invite returns on a channel… it must
/// resolve to the same counterparty. **Scheduling without that produces orphan
/// replies.**"* `scheduling` holds up its half and says so — it *"does not
/// resolve identities"*, and takes an already-resolved audience and negotiation
/// id, which is what lets any flow on any channel use it. What was missing is
/// this: whoever receives the reply has an address and a relationship, not a
/// negotiation id.
///
/// # Three answers, and only one of them is a target
///
/// - `one_open_ask` — absorb against `negotiation_id`.
/// - `no_open_ask` — **not an error.** A counterparty writes about many things,
///   and treating every message that is not a scheduling reply as a failure
///   would make the loop unusable.
/// - `ambiguous_open_asks` — two asks are waiting, and one reply answers one
///   ask. Picking is a coin flip whose loser is settled by words that were never
///   about it, so the ids come back and the caller chooses. A counterparty with a
///   demo AND a contract review is legitimate; the reply names which only in
///   prose, which nothing here reads.
///
/// It does not read the reply, and it does not absorb. The reading stays
/// supplied — turning prose into a verdict is language work, and a parser
/// reading *"no problem, but let me check with my co-founder"* as an acceptance
/// puts words in a counterparty's mouth and then books a room.
pub async fn open_ask_handler(
    req: HttpRequest,
    query: web::Query<ReplyTargetQuery>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    use magician_media::reply_routing::{open_ask_for, ReplyTarget};

    let query = query.into_inner();
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let audience = match resolve_reply_audience(&req, &principal, &workspace, &query).await {
        Ok(audience) => audience,
        Err(response) => return response,
    };
    let scope = SchedulingScope::new(principal, workspace);

    match open_ask_for(&api.scheduling(), &scope, &audience) {
        Ok(target) => {
            let reason = target.reason();
            let body = match target {
                ReplyTarget::One(negotiation) => json!({
                    "negotiation_id": negotiation.negotiation_id,
                    "purpose": negotiation.purpose,
                    "state": negotiation.state().as_str(),
                    "standing_slots": negotiation.standing_slots(),
                }),
                ReplyTarget::NoOpenAsk => json!(null),
                ReplyTarget::Ambiguous { negotiation_ids } => json!({
                    "negotiation_ids": negotiation_ids,
                }),
            };
            HttpResponse::Ok().json(json!({
                "audience": audience,
                "outcome": reason,
                "ask": body,
            }))
        },
        // An unreadable log is not "they have no ask waiting". Folded, it turns
        // a reply into an orphan for a reason nothing records.
        Err(error) => store_unreadable("the scheduling log for this relationship", &error),
    }
}

/// What an introducer-debt read is asked for.
#[derive(Debug, Deserialize)]
pub struct IntroducerDebtQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// How long an introducer may go un-updated before we owe them one.
    /// Refused at or below zero by the policy: a zero window owes an update the
    /// instant the introduction lands, which is a debt nobody could discharge.
    #[serde(default = "default_introducer_window_days")]
    pub update_after_days: i64,
}

fn default_introducer_window_days() -> i64 {
    30
}

/// `GET /api/magician/v2/work/introducers`
///
/// Who introduced us to whom, and which of them we owe an update.
///
/// # It proposes; it does not record
///
/// The obligations come back as **proposals**. Nothing is written, because
/// *"we owe Sarah an update on the intro she made"* is a judgement about a
/// relationship and the moment to make it is not "whenever somebody polled a
/// route". A caller that agrees records them through `POST /work/obligations`,
/// which is the register's own door and applies its own rules.
///
/// # What comes back, and what deliberately does not
///
/// `introductions` and `introducers` are the denominators — *"Sarah introduced
/// four"* and *"four people introduced us"* are different facts and one number
/// conflates them. `owed` is the introducers past the window, each with the
/// obligation to record.
///
/// There is no `unresolved` list and no `unaddressable` one. The graph is built
/// from a single grouped read, so a register that cannot be read fails the
/// request rather than producing a partial answer with holes in it — a partial
/// referral graph would say *"nobody introduced us to them"* about people
/// somebody did. And nothing needs resolving: the debt is owed to a **person**
/// keyed by the introducer, so whether the register holds them is a question
/// about how to reach them, not about whether they are owed an update.
pub async fn introducer_debts_handler(
    req: HttpRequest,
    query: web::Query<IntroducerDebtQuery>,
    _api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    use magician::magician_v2::counterparties::{global_counterparty_store, CounterpartyScope};
    use magician::magician_v2::introductions::{
        introducer_debts, referral_graph, IntroducerPolicy,
    };

    let query = query.into_inner();
    let (principal, workspace) = match resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // Bounded BEFORE `Duration::days`, which panics on a value large enough to
    // overflow its millisecond representation. A caller-supplied integer
    // reaching that constructor unchecked is a 500 from a query string.
    if !(1..=3650).contains(&query.update_after_days) {
        return bad_request(format!(
            "`update_after_days` must be between 1 and 3650; `{}` is not a window \
             anybody chose — zero owes an update the instant an introduction lands, and \
             ten years owes one never",
            query.update_after_days
        ));
    }
    let policy = match IntroducerPolicy::new(Duration::days(query.update_after_days)) {
        Ok(policy) => policy,
        Err(error) => return bad_request(format!("{error:#}")),
    };
    // Absent is unreadable, never empty. Answering "no introductions" for a
    // register nobody opened is the failure this whole subsystem keeps hitting.
    let Some(counterparties) = global_counterparty_store() else {
        return store_unreadable(
            "the counterparty register",
            &anyhow::anyhow!("no counterparty register is installed in this process"),
        );
    };
    let scope = CounterpartyScope::new(principal.clone(), workspace.clone());

    let graph = match referral_graph(counterparties.as_ref(), &scope) {
        Ok(graph) => graph,
        Err(error) => return store_unreadable("the referral graph", &error),
    };

    // No register lookup here, and that is the fix rather than an omission. The
    // debt is owed to a PERSON keyed by the introducer, so there is no
    // relationship to resolve — and whether we can REACH somebody is a
    // different question from whether we owe them an update.
    let now = Utc::now();
    let owed = introducer_debts(&graph, policy, "introducer-read", now);

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of": now,
        "introductions": graph.len(),
        "introducers": graph.by_introducer.len(),
        "owed": owed
            .iter()
            .map(|debt| json!({
                "introducer": debt.introducer,
                "covers": debt.covers
                    .iter()
                    .map(|held| json!({
                        "counterparty_id": held.counterparty_id,
                        "display_name": held.display_name,
                        "identity": held.identity,
                        "introduced_at": held.introduced_at,
                        "last_seen": held.last_seen,
                    }))
                    .collect::<Vec<_>>(),
                "proposed_obligation": {
                    "audience": debt.obligation.audience,
                    "what": debt.obligation.what,
                    "due_at": debt.obligation.due_at,
                    "direction": debt.obligation.direction.as_str(),
                },
            }))
            .collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct RunInboxSweepRequest {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `POST /api/magician/v2/work/runs/inbox-sweep`
///
/// Read the run inbox and close whatever wait it answers.
///
/// §5's *"the agent must read its own inbox mid-flow, extract the code, and
/// continue — this is the primitive nobody has."* `run_state` held up its half
/// from the day it was built: it records that a wait exists, matches an
/// arriving event to it, and is idempotent on a re-delivered one. Its header
/// says outright that it *"never reads an inbox"*, and that refusal is what
/// keeps a run usable whatever channel its verification arrives on. Nothing
/// ever read a mailbox and told it, so a run raised a wait and waited forever.
///
/// # Nothing is consumed, and re-running is free
///
/// No message is settled, moved or deleted — a run inbox is an ordinary mailbox
/// and consuming what the sweep walked past would take mail that was never
/// ours. The run store's idempotency on the event ref IS the cursor, which is
/// better than one this surface would have to keep honest.
///
/// # Read `ambiguous` and `refused` — those are the two that need a person
///
/// A source more than one run is waiting on closes NEITHER. One message
/// satisfies one wait, and picking is a coin flip whose loser is closed by an
/// event that never satisfied it. The fix is narrowing the hints, which is a
/// judgement about the work rather than something a sweep can do.
pub async fn run_inbox_sweep_handler(
    req: HttpRequest,
    body: web::Json<RunInboxSweepRequest>,
    api: web::Data<WorkModulesApi>,
) -> HttpResponse {
    use magician_media::run_inbox::{sweep_run_inbox, MaildirRunInbox};

    let body = body.into_inner();
    let (principal, workspace) = match resolve_scope(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(configured) = api.run_inbox.as_ref() else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "run_inbox_no_source",
            "no run inbox is configured, so a wait can only be closed by hand. This is \
             not a healthy state and is deliberately not reported as a sweep that found \
             nothing: a pass that closed nothing because it asked nobody and one that \
             closed nothing because nothing arrived are opposite facts.",
            None,
        );
    };

    let inbox = MaildirRunInbox::new(&configured.maildir_path, &configured.source_hint);
    let scope = RunScope::new(principal.clone(), workspace.clone());
    match sweep_run_inbox(&inbox, &api.runs(), &scope, Utc::now()) {
        Ok(sweep) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "source_hint": configured.source_hint,
            // Denominators. "Closed none" over three messages and over three
            // hundred are different facts, and so is "no run was waiting".
            "examined": sweep.examined,
            // Must equal `examined`. Reported so a reader can check rather than
            // trust: a shortfall means a message went somewhere nothing
            // reports, which is how a verification that arrived becomes a run
            // that waits forever.
            "accounted_for": sweep.accounted_for(),
            "runs_waiting": sweep.runs_waiting,
            "fulfilled": sweep
                .fulfilled
                .iter()
                .map(|held| json!({
                    "run_id": held.run_id,
                    "expectation_id": held.expectation_id,
                    "event_ref": held.event_ref,
                }))
                .collect::<Vec<_>>(),
            // Already done, and not-for-us. Both are no-ops and opposite facts:
            // one says the verification arrived, the other says it never did.
            "already_fulfilled": sweep.already_fulfilled,
            "unmatched": sweep.unmatched,
            "ambiguous": sweep
                .ambiguous
                .iter()
                .map(|held| json!({
                    "source_hint": held.source_hint,
                    "run_ids": held.run_ids,
                    "messages": held.messages,
                }))
                .collect::<Vec<_>>(),
            // Runs the store refused — in practice, a run holding two open
            // waits on one source. Carried rather than propagated: one
            // misconfigured run must not stop every other run's verification
            // from landing, and a refusal nobody can see is a run that waits
            // forever for a reason nothing reports.
            "refused": sweep
                .refused
                .iter()
                .map(|held| json!({
                    "run_id": held.run_id,
                    "reason": held.reason,
                }))
                .collect::<Vec<_>>(),
        })),
        Err(error) => store_unreadable("the run inbox", &error),
    }
}

pub fn configure_work_module_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/work/introducers", web::get().to(introducer_debts_handler))
        .route(
            "/work/runs/inbox-sweep",
            web::post().to(run_inbox_sweep_handler),
        )
        // Before `/work/negotiations/{negotiation_id}`, which it would otherwise
        // match. Safe as a literal here — unlike an agent id, a negotiation id is
        // derived (`neg-<hash>`) and can never be the word `open-ask`, so no record
        // can ever be shadowed by this route.
        .route(
            "/work/negotiations/open-ask",
            web::get().to(open_ask_handler),
        )
        .route("/work/runs", web::get().to(list_runs_handler))
        .route("/work/runs", web::post().to(open_run_handler))
        // The literal children come before `{run_id}`'s own children so a run
        // id can never shadow one of them.
        .route(
            "/work/runs/{run_id}/gaps/resolve",
            web::post().to(resolve_gap_handler),
        )
        .route(
            "/work/runs/{run_id}/expectations/fulfil",
            web::post().to(fulfil_expectation_handler),
        )
        .route(
            "/work/runs/{run_id}/fields",
            web::post().to(declare_field_handler),
        )
        .route(
            "/work/runs/{run_id}/answers",
            web::post().to(record_answer_handler),
        )
        .route(
            "/work/runs/{run_id}/gaps",
            web::post().to(raise_gap_handler),
        )
        .route(
            "/work/runs/{run_id}/expectations",
            web::post().to(raise_expectation_handler),
        )
        .route(
            "/work/runs/{run_id}/submit",
            web::post().to(submit_run_handler),
        )
        .route("/work/runs/{run_id}", web::get().to(get_run_handler))
        .route(
            "/work/negotiations",
            web::get().to(list_negotiations_handler),
        )
        .route(
            "/work/negotiations",
            web::post().to(open_negotiation_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}/reply",
            web::post().to(absorb_reply_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}/re-offer",
            web::post().to(re_offer_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}/hold",
            web::post().to(hold_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}/reschedule",
            web::post().to(reschedule_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}/close",
            web::post().to(close_negotiation_handler),
        )
        .route(
            "/work/negotiations/{negotiation_id}",
            web::get().to(get_negotiation_handler),
        )
        // `carrying` before the bare listing so the literal is never read as a
        // query-less manifest read.
        .route(
            "/work/claim-manifests/carrying",
            web::get().to(revisions_carrying_handler),
        )
        .route(
            "/work/claim-manifests",
            web::get().to(read_manifests_handler),
        )
        .route(
            "/work/claim-manifests",
            web::post().to(bind_manifest_handler),
        )
        .route(
            "/work/obligations/sweep",
            web::post().to(sweep_register_handler),
        )
        .route(
            "/work/obligations/{obligation_id}/settle",
            web::post().to(settle_obligation_handler),
        )
        .route("/work/obligations", web::get().to(read_register_handler))
        .route(
            "/work/obligations",
            web::post().to(record_obligation_handler),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;

    const SEP: &str = "\u{1f}";

    fn api_on(dir: &tempfile::TempDir) -> WorkModulesApi {
        WorkModulesApi::new(ArtifactV2Workspace::new(dir.path().to_path_buf()))
    }

    fn app_data(dir: &tempfile::TempDir) -> web::Data<WorkModulesApi> {
        web::Data::new(api_on(dir))
    }

    macro_rules! app {
        ($dir:expr) => {
            actix_test::init_service(
                actix_web::App::new()
                    .app_data(app_data($dir))
                    .configure(configure_work_module_routes),
            )
            .await
        };
    }

    /// A scoped POST, returning `(status, body)`.
    ///
    /// A macro rather than a generic fn so the tests need no name for actix's
    /// service type, which is unnameable without pulling in `actix_http`.
    macro_rules! post {
        ($app:expr, $uri:expr, $body:expr) => {{
            let response = actix_test::call_service(
                &$app,
                actix_test::TestRequest::post()
                    .uri($uri)
                    .insert_header(("X-Principal", "anonymous"))
                    .insert_header(("X-Workspace", "default"))
                    .set_json($body)
                    .to_request(),
            )
            .await;
            let status = response.status().as_u16();
            let payload: Value = actix_test::read_body_json(response).await;
            (status, payload)
        }};
    }

    /// A scoped GET, returning `(status, body)`.
    macro_rules! get {
        ($app:expr, $uri:expr) => {{
            let response = actix_test::call_service(
                &$app,
                actix_test::TestRequest::get()
                    .uri($uri)
                    .insert_header(("X-Principal", "anonymous"))
                    .insert_header(("X-Workspace", "default"))
                    .to_request(),
            )
            .await;
            let status = response.status().as_u16();
            let payload: Value = actix_test::read_body_json(response).await;
            (status, payload)
        }};
    }

    fn open_run_body() -> Value {
        json!({
            "purpose": "annual return",
            "resource_ref": "portal://filings/2026",
            "fields": [
                { "name": "turnover", "required": true },
                { "name": "notes", "required": false },
            ],
            "opened_by": "owner",
        })
    }

    // ── run_state ───────────────────────────────────────────────────────────

    /// A run's waits and gaps reach the owner with real values.
    ///
    /// Pins the failure this module exists for: `expectations_awaiting` and
    /// `gaps_for_owner` were complete and unreachable, so a run could wait
    /// forever on a code nobody was told to look for.
    #[actix_web::test]
    async fn a_runs_waits_and_gaps_reach_the_owner() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);

        let (status, opened) = post!(app, "/work/runs", open_run_body());
        assert_eq!(status, 200, "{opened}");
        let run_id = opened["run"]["run_id"]
            .as_str()
            .expect("run id")
            .to_string();
        assert_eq!(opened["state"], "drafting");

        let (status, _) = post!(
            app,
            &format!("/work/runs/{run_id}/expectations"),
            json!({ "description": "the verification code", "source_hint": "inbox:filings" })
        );
        assert_eq!(status, 200);
        let (status, _) = post!(
            app,
            &format!("/work/runs/{run_id}/gaps"),
            json!({ "field": "turnover", "question": "what was turnover last year?" })
        );
        assert_eq!(status, 200);

        let (status, view) = get!(app, &format!("/work/runs/{run_id}"));
        assert_eq!(status, 200, "{view}");
        assert_eq!(view["state"], "awaiting_external");
        assert_eq!(view["counts"]["expectations_open"], 1);
        assert_eq!(view["counts"]["gaps_open"], 1);
        assert_eq!(view["counts"]["gaps_blocking_review"], 1);
        assert_eq!(view["counts"]["fields_pending"], 2);
        assert_eq!(view["awaiting"][0]["source_hint"], "inbox:filings");
        assert_eq!(view["awaiting"][0]["description"], "the verification code");
        assert!(
            view["awaiting"][0]["waiting_for_seconds"]
                .as_i64()
                .expect("seconds")
                >= 0,
            "a wait's age is derived from the clock, so it can never be negative"
        );
        assert_eq!(view["gaps"][0]["question"], "what was turnover last year?");
    }

    /// An answer citing no evidence is refused and the field stays pending.
    ///
    /// Pins §5's grounding rule at the surface: an invented figure on a real
    /// application is unrecoverable in a way a missed deadline is not.
    #[actix_web::test]
    async fn an_ungrounded_answer_is_refused_and_the_field_stays_pending() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(app, "/work/runs", open_run_body());
        let run_id = opened["run"]["run_id"]
            .as_str()
            .expect("run id")
            .to_string();

        let (status, refusal) = post!(
            app,
            &format!("/work/runs/{run_id}/answers"),
            json!({ "field": "turnover", "text": "1.2m", "evidence_refs": [] })
        );
        assert_eq!(status, 409, "{refusal}");
        assert!(
            refusal["error"]
                .as_str()
                .expect("message")
                .contains("cites no retrievable evidence"),
            "the refusal must say why: {refusal}"
        );

        let (_, view) = get!(app, &format!("/work/runs/{run_id}"));
        assert_eq!(
            view["counts"]["fields_pending"], 2,
            "the refused answer must not have landed"
        );

        // A grounded one lands, so the refusal is about grounding, not writing.
        let (status, ok) = post!(
            app,
            &format!("/work/runs/{run_id}/answers"),
            json!({ "field": "turnover", "text": "1.2m", "evidence_refs": ["ev-accounts-2025"] })
        );
        assert_eq!(status, 200, "{ok}");
        let (_, view) = get!(app, &format!("/work/runs/{run_id}"));
        assert_eq!(view["counts"]["fields_pending"], 1);
    }

    /// An arrived event closes the wait it matches, and an unrelated one
    /// writes nothing.
    ///
    /// Pins the loop §5 calls *"the primitive nobody has"*: without the
    /// unmatched arm being an ordinary success, every unrelated inbox item
    /// would read as a failure and the loop would be unusable.
    #[actix_web::test]
    async fn an_inbound_event_closes_its_wait_and_an_unrelated_one_does_not() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(app, "/work/runs", open_run_body());
        let run_id = opened["run"]["run_id"]
            .as_str()
            .expect("run id")
            .to_string();
        post!(
            app,
            &format!("/work/runs/{run_id}/expectations"),
            json!({ "description": "the verification code", "source_hint": "inbox:filings" })
        );

        let (status, unmatched) = post!(
            app,
            &format!("/work/runs/{run_id}/expectations/fulfil"),
            json!({ "event_ref": "msg-99", "source_hint": "inbox:newsletter" })
        );
        assert_eq!(status, 200, "{unmatched}");
        assert_eq!(unmatched["outcome"], "unmatched");
        let (_, still) = get!(app, &format!("/work/runs/{run_id}"));
        assert_eq!(
            still["counts"]["expectations_open"], 1,
            "an unrelated message must not close a wait"
        );

        let (status, done) = post!(
            app,
            &format!("/work/runs/{run_id}/expectations/fulfil"),
            json!({ "event_ref": "msg-1", "source_hint": "INBOX:filings" })
        );
        assert_eq!(status, 200, "{done}");
        assert_eq!(done["outcome"], "fulfilled");

        let (_, replay) = post!(
            app,
            &format!("/work/runs/{run_id}/expectations/fulfil"),
            json!({ "event_ref": "msg-1", "source_hint": "inbox:filings" })
        );
        assert_eq!(
            replay["outcome"], "already_fulfilled",
            "an identical replay resumes rather than erroring"
        );
    }

    /// A gap resolved with no named person is refused.
    ///
    /// Pins the authority failure: an unattributed resolution is synthesised
    /// text wearing an owner's authority, and it would then ground an answer.
    #[actix_web::test]
    async fn a_gap_resolution_with_no_named_person_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(app, "/work/runs", open_run_body());
        let run_id = opened["run"]["run_id"]
            .as_str()
            .expect("run id")
            .to_string();
        post!(
            app,
            &format!("/work/runs/{run_id}/gaps"),
            json!({ "field": "turnover", "question": "what was turnover?" })
        );

        let (status, refusal) = post!(
            app,
            &format!("/work/runs/{run_id}/gaps/resolve"),
            json!({
                "field": "turnover",
                "question": "what was turnover?",
                "text": "1.2m",
                "evidence_refs": ["reply-7"],
                "resolved_by": "   ",
            })
        );
        assert_eq!(status, 409, "{refusal}");

        let (status, ok) = post!(
            app,
            &format!("/work/runs/{run_id}/gaps/resolve"),
            json!({
                "field": "turnover",
                "question": "what was turnover?",
                "text": "1.2m",
                "evidence_refs": ["reply-7"],
                "resolved_by": "the owner",
            })
        );
        assert_eq!(status, 200, "{ok}");
        let (_, view) = get!(app, &format!("/work/runs/{run_id}"));
        assert_eq!(
            view["counts"]["gaps_open"], 0,
            "a named resolution closes the gap"
        );
        assert_eq!(
            view["counts"]["fields_pending"], 1,
            "the owner's reply is written into the field too — one truth, not a \
             resolved gap beside a stale field"
        );
    }

    // ── scheduling ──────────────────────────────────────────────────────────

    fn open_negotiation_body(kind: &str, id: &str) -> Value {
        json!({
            "audience_kind": kind,
            "audience_id": id,
            "counterparty": "dana",
            "purpose": "quarterly review",
            "slots": [
                { "start": "2026-09-01T09:00:00Z", "end": "2026-09-01T10:00:00Z" },
                { "start": "2026-09-02T14:00:00Z", "end": "2026-09-02T15:00:00Z" },
            ],
            "offer_act_ref": "act-offer-1",
        })
    }

    /// The negotiation surface drives the whole arc, and hands back an offer
    /// that names no channel.
    ///
    /// Pins the coupling failure: `offer_message` returns text and slots for
    /// ANY send capability, and a surface that had baked a channel in would
    /// have made the module usable on exactly one.
    #[actix_web::test]
    async fn the_offer_comes_back_as_words_and_times_with_no_channel() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (status, opened) = post!(
            app,
            "/work/negotiations",
            open_negotiation_body("engagement", "eng-1")
        );
        assert_eq!(status, 200, "{opened}");
        assert_eq!(opened["state"], "awaiting_reply");
        let body = opened["offer_message"]["body"].as_str().expect("body");
        assert!(body.contains("quarterly review"), "{body}");
        assert!(body.contains("2026-09-01T09:00:00Z"), "{body}");
        assert_eq!(
            opened["offer_message"]["slots"]
                .as_array()
                .expect("slots")
                .len(),
            2
        );
        assert_eq!(opened["offer_message"]["offer_act_ref"], "act-offer-1");
        assert!(
            opened["offer_message"].get("channel").is_none(),
            "the offer must name no channel: {}",
            opened["offer_message"]
        );
    }

    /// Accepting a time nobody offered is refused; accepting a standing one
    /// yields a hold intent.
    ///
    /// Pins the agreement that never happened: recording an acceptance of an
    /// unoffered slot would put a time on a calendar nobody agreed to.
    #[actix_web::test]
    async fn an_acceptance_must_name_a_standing_slot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(
            app,
            "/work/negotiations",
            open_negotiation_body("engagement", "eng-1")
        );
        let id = opened["negotiation"]["negotiation_id"]
            .as_str()
            .expect("id")
            .to_string();

        let (status, refusal) = post!(
            app,
            &format!("/work/negotiations/{id}/reply"),
            json!({
                "audience_kind": "engagement",
                "audience_id": "eng-1",
                "source_ref": "msg-1",
                "at": "2026-08-21T09:00:00Z",
                "reading": "accepted",
                "starting_at": "2026-09-03T11:00:00Z",
            })
        );
        assert_eq!(status, 409, "{refusal}");

        let (status, accepted) = post!(
            app,
            &format!("/work/negotiations/{id}/reply"),
            json!({
                "audience_kind": "engagement",
                "audience_id": "eng-1",
                "source_ref": "msg-2",
                "at": "2026-08-21T09:00:00Z",
                "reading": "accepted",
                "starting_at": "2026-09-01T09:00:00Z",
            })
        );
        assert_eq!(status, 200, "{accepted}");
        assert_eq!(accepted["state"], "accepted");
        assert_eq!(
            accepted["hold_intent"]["slot"]["start"], "2026-09-01T09:00:00Z",
            "the intent must carry the slot THEY accepted: {}",
            accepted["hold_intent"]
        );

        let (status, held) = post!(
            app,
            &format!("/work/negotiations/{id}/hold"),
            json!({
                "audience_kind": "engagement",
                "audience_id": "eng-1",
                "calendar_event_ref": "cal-1",
            })
        );
        assert_eq!(status, 200, "{held}");
        assert_eq!(held["state"], "held");
        assert_eq!(held["held"]["calendar_event_ref"], "cal-1");
    }

    /// A non-engagement relationship negotiates end to end, and an
    /// unrecognised kind refuses.
    ///
    /// Pins the coupling this whole tier is meant to avoid: a surface that
    /// hardcoded the engagement arm would have needed editing before a
    /// recruiting panel or a vendor account could ever use it.
    #[actix_web::test]
    async fn any_audience_kind_negotiates_and_an_unknown_kind_refuses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        for (kind, id) in [
            ("program", "prog-1"),
            ("account", "acct-1"),
            ("panel", "panel-1"),
            ("person", "person-1"),
        ] {
            let (status, opened) =
                post!(app, "/work/negotiations", open_negotiation_body(kind, id));
            assert_eq!(status, 200, "{kind} must negotiate: {opened}");
            assert_eq!(opened["negotiation"]["audience"]["kind"], kind);
        }

        let (status, listed) = get!(app, "/work/negotiations");
        assert_eq!(status, 200);
        assert_eq!(
            listed["counts"]["total"], 4,
            "the whole book comes back across relationships: {listed}"
        );

        let (status, refusal) = post!(
            app,
            "/work/negotiations",
            open_negotiation_body("customer", "c-1")
        );
        assert_eq!(status, 400, "{refusal}");
        assert!(
            refusal["error"]
                .as_str()
                .expect("message")
                .contains("not an audience kind"),
            "an unknown kind is refused, never defaulted: {refusal}"
        );
    }

    /// A re-offer replaces the standing times, and their acceptance of a new
    /// one is then recordable.
    ///
    /// Pins the wedge: without `re_offer` a changed offer landed nowhere, and
    /// their acceptance of it was refused as an agreement that never happened.
    #[actix_web::test]
    async fn a_re_offer_replaces_the_standing_times() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(
            app,
            "/work/negotiations",
            open_negotiation_body("account", "acct-1")
        );
        let id = opened["negotiation"]["negotiation_id"]
            .as_str()
            .expect("id")
            .to_string();
        post!(
            app,
            &format!("/work/negotiations/{id}/reply"),
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "source_ref": "msg-1",
                "at": "2026-08-21T09:00:00Z",
                "reading": "declined",
            })
        );

        let (status, again) = post!(
            app,
            &format!("/work/negotiations/{id}/re-offer"),
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "slots": [{ "start": "2026-09-08T09:00:00Z", "end": "2026-09-08T10:00:00Z" }],
                "offer_act_ref": "act-offer-2",
            })
        );
        assert_eq!(status, 200, "{again}");
        assert_eq!(again["state"], "awaiting_reply");
        assert_eq!(
            again["negotiation"]["offered"]
                .as_array()
                .expect("offered")
                .len(),
            1,
            "the dead times are gone: {}",
            again["negotiation"]["offered"]
        );

        let (status, accepted) = post!(
            app,
            &format!("/work/negotiations/{id}/reply"),
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "source_ref": "msg-2",
                "at": "2026-08-22T09:00:00Z",
                "reading": "accepted",
                "starting_at": "2026-09-08T09:00:00Z",
            })
        );
        assert_eq!(
            status, 200,
            "their acceptance of the replacement must record: {accepted}"
        );
    }

    // ── claim manifests ─────────────────────────────────────────────────────

    /// A manifest binds, reads back, and answers the correction query.
    ///
    /// Pins the gap the sent-record cannot fill: when a claim is corrected,
    /// the decks and drafts still carrying it are exactly what nothing else in
    /// the system can find.
    #[actix_web::test]
    async fn a_manifest_binds_and_answers_the_correction_query() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let bind = json!({
            "artifact_ref": "art-deck",
            "revision_ref": "rev-1",
            "claims": [
                { "claim_ref": "claim-arr", "evidence_refs": ["ev-2", "ev-1"] },
            ],
            "bound_by": "the analyst",
        });
        let (status, bound) = post!(app, "/work/claim-manifests", bind.clone());
        assert_eq!(status, 200, "{bound}");
        assert_eq!(
            bound["manifest"]["claims"][0]["evidence_refs"],
            json!(["ev-1", "ev-2"]),
            "evidence refs are stored canonical, so a reorder compares equal"
        );

        let (status, replay) = post!(app, "/work/claim-manifests", bind);
        assert_eq!(status, 200, "an identical rebind resumes: {replay}");

        let (status, changed) = post!(
            app,
            "/work/claim-manifests",
            json!({
                "artifact_ref": "art-deck",
                "revision_ref": "rev-1",
                "claims": [{ "claim_ref": "claim-other", "evidence_refs": ["ev-1"] }],
                "bound_by": "the analyst",
            })
        );
        assert_eq!(
            status, 409,
            "a revision's claim set is immutable: {changed}"
        );

        let (status, history) = get!(app, "/work/claim-manifests?artifact_ref=art-deck");
        assert_eq!(status, 200, "{history}");
        assert_eq!(history["counts"]["revisions"], 1);

        let (status, carrying) = get!(app, "/work/claim-manifests/carrying?claim_ref=claim-arr");
        assert_eq!(status, 200, "{carrying}");
        assert_eq!(carrying["counts"]["revisions"], 1);
        assert_eq!(carrying["revisions"][0]["artifact_ref"], "art-deck");
        assert_eq!(carrying["revisions"][0]["revision_ref"], "rev-1");
        assert_eq!(
            carrying["revisions"][0]["evidence_refs"],
            json!(["ev-1", "ev-2"]),
            "the citation travels, so a fix can be checked against it"
        );

        let (status, unrelated) = get!(
            app,
            "/work/claim-manifests/carrying?claim_ref=claim-nothing"
        );
        assert_eq!(status, 200, "{unrelated}");
        assert_eq!(unrelated["counts"]["revisions"], 0);
    }

    // ── the register ────────────────────────────────────────────────────────

    /// A promise recorded through the surface lapses on the clock and settles
    /// as met.
    ///
    /// Pins the register's whole reason to exist: nothing enforced Friday, and
    /// a lapse that had to be written down by a sweep would miss exactly the
    /// promise nobody remembered.
    #[actix_web::test]
    async fn a_promise_lapses_on_the_clock_and_settles_as_met() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (status, recorded) = post!(
            app,
            "/work/obligations",
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "what": "the quarterly metrics",
                "due_at": "2020-01-03T17:00:00Z",
                "direction": "owed_by_us",
                "created_by": "the owner",
            })
        );
        assert_eq!(status, 200, "{recorded}");
        assert_eq!(recorded["obligation"]["state"], "lapsed");
        assert_eq!(recorded["obligation"]["lapse_is_ours"], true);
        assert!(
            recorded["obligation"]["overdue_by_seconds"]
                .as_i64()
                .expect("overdue")
                > 0,
            "a deadline in the past is overdue by a positive amount"
        );
        let obligation_id = recorded["obligation"]["obligation_id"]
            .as_str()
            .expect("id")
            .to_string();

        let (status, register) = get!(app, "/work/obligations");
        assert_eq!(status, 200, "{register}");
        assert_eq!(register["counts"]["total"], 1);
        assert_eq!(register["counts"]["lapsed"], 1);
        assert_eq!(register["counts"]["lapsed_owed_by_us"], 1);
        assert_eq!(
            register["counts"]["lapsed_owed_to_us"], 0,
            "the two directions never read alike"
        );

        let (status, settled) = post!(
            app,
            &format!("/work/obligations/{obligation_id}/settle"),
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "settlement": "met",
                "note": "sent on the 4th",
            })
        );
        assert_eq!(status, 200, "{settled}");
        assert_eq!(settled["obligation"]["state"], "met");

        let (_, after) = get!(app, "/work/obligations");
        assert_eq!(after["counts"]["met"], 1);
        assert_eq!(after["counts"]["lapsed"], 0);
        assert_eq!(
            after["obligations"].as_array().expect("rows").len(),
            0,
            "a settled row is no longer outstanding, so it drops out of the default view"
        );
    }

    /// A release with no reason is refused.
    ///
    /// Pins the conflation the two settlement kinds exist to prevent: an
    /// unexplained release reports a hit rate that is partly wishful.
    #[actix_web::test]
    async fn a_release_with_no_reason_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, recorded) = post!(
            app,
            "/work/obligations",
            json!({
                "audience_kind": "panel", "audience_id": "panel-1",
                "what": "the audit pack", "due_at": "2020-01-03T17:00:00Z",
                "direction": "owed_to_us", "created_by": "the owner",
            })
        );
        let id = recorded["obligation"]["obligation_id"]
            .as_str()
            .expect("id")
            .to_string();
        let (status, refusal) = post!(
            app,
            &format!("/work/obligations/{id}/settle"),
            json!({ "audience_kind": "panel", "audience_id": "panel-1", "settlement": "released" })
        );
        assert_eq!(status, 400, "{refusal}");
        let (status, ok) = post!(
            app,
            &format!("/work/obligations/{id}/settle"),
            json!({
                "audience_kind": "panel", "audience_id": "panel-1",
                "settlement": "released", "reason": "the engagement ended",
            })
        );
        assert_eq!(status, 200, "{ok}");
        assert_eq!(ok["obligation"]["state"], "released");
    }

    /// The sweep route fills the register from real activity.
    ///
    /// Pins the whole tier-2 failure: `silence_obligations` had no caller, so
    /// an offer nobody answered left the register empty — indistinguishable
    /// from a counterparty who had replied.
    ///
    /// The ask is seeded through the store with an offer instant four days
    /// back, because an offer made by the route is made *now* and a sweep over
    /// it would prove only that the route runs.
    #[actix_web::test]
    async fn the_sweep_route_fills_the_register_from_an_unanswered_offer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(dir.path().to_path_buf());
        let start = Utc::now() + Duration::days(14);
        SchedulingStore::new(layout.clone())
            .open(
                &SchedulingScope::new("anonymous", "default"),
                &AudienceRef::account("acct-1"),
                "dana",
                "quarterly review",
                &[Slot::new(start, start + Duration::hours(1)).expect("slot")],
                Some("act-offer-1".to_string()),
                Utc::now() - Duration::days(4),
            )
            .expect("seed an unanswered ask");

        let app = app!(&dir);
        let register = ObligationStore::new(layout.clone());
        let scope = ObligationScope::new("anonymous", "default");
        assert!(
            register.all_obligations(&scope).expect("read").is_empty(),
            "the register starts empty, so anything in it came from the sweep"
        );

        let (status, swept) = post!(
            app,
            "/work/obligations/sweep",
            json!({ "silence_window_hours": 72 })
        );
        assert_eq!(status, 200, "{swept}");
        assert_eq!(swept["negotiations_seen"], 1);
        assert_eq!(swept["recorded"], 1);
        assert_eq!(swept["settled"], 0);

        let (status, owed) = get!(app, "/work/obligations");
        assert_eq!(status, 200, "{owed}");
        assert_eq!(owed["counts"]["total"], 1);
        assert_eq!(owed["counts"]["lapsed"], 1);
        assert_eq!(
            owed["counts"]["lapsed_owed_to_us"], 1,
            "a reply THEY owe is not a promise WE broke"
        );
        assert_eq!(
            owed["obligations"][0]["what"],
            "a reply to the scheduling ask to dana about quarterly review"
        );
        assert_eq!(owed["obligations"][0]["created_by"], "scheduling");
        assert_eq!(owed["obligations"][0]["source_act_ref"], "act-offer-1");
        assert_eq!(owed["obligations"][0]["lapse_is_ours"], false);

        // A second sweep resumes the row rather than minting a second.
        let (_, again) = post!(
            app,
            "/work/obligations/sweep",
            json!({ "silence_window_hours": 72 })
        );
        assert_eq!(again["recorded"], 1);
        let (_, owed) = get!(app, "/work/obligations");
        assert_eq!(
            owed["counts"]["total"], 1,
            "two sweeps over one unanswered ask leave one row"
        );
    }

    /// The register read carries what the counterparty actually did.
    ///
    /// Pins the tier-3 island: `data_room::cycle` composed §6's own sentence —
    /// *"opened the deck three times, never opened the financials"* — and its
    /// only caller anywhere was a `#[cfg(test)]` assertion. The register's own
    /// text cannot carry it (the text is part of the identity tuple, so a visit
    /// count in it would mint a row per visit), so an agent reading its book
    /// saw "opened, no reply" and could not tell a skim from a study.
    ///
    /// The activity is seeded through the stores with instants in the past,
    /// because a room shared *now* has ripened nothing and a read over it would
    /// prove only that the route runs.
    #[actix_web::test]
    async fn the_register_read_explains_a_row_with_what_the_room_observed() {
        use magician::magician_v2::audience::Audience;
        use magician::magician_v2::share_links::{IssueShareLink, ShareLinkScope, ShareLinkStore};
        use magician_learning::data_room::access_log::{AccessEvent, UserAgentClass};
        use magician_learning::data_room::access_store::{AccessScope, AccessStore};
        use magician_learning::data_room::{
            DataRoomScope, DataRoomStore, DocumentVisibility, GrantDisclosure, OpenDataRoom,
        };

        const DECK: &str = "artifact://deck@1";
        const FINANCIALS: &str = "artifact://financials@1";
        const PARTNER: &str = "partner@example.test";

        let dir = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(dir.path().to_path_buf());
        let audience = AudienceRef::engagement("eng-1");
        let rooms = DataRoomStore::new(layout.clone());
        let room_scope = DataRoomScope::new("anonymous", "default");
        let room = rooms
            .open(
                &room_scope,
                &OpenDataRoom {
                    audience: audience.clone(),
                    opened_by: "owner".to_string(),
                    closes_at: None,
                },
                Utc::now() - Duration::days(13),
            )
            .expect("open the room");
        ShareLinkStore::new(layout.clone())
            .issue(
                &ShareLinkScope::new("anonymous", "default"),
                &IssueShareLink {
                    resource_ref: room.room_id.clone(),
                    audience: audience.clone(),
                    issued_to: PARTNER.to_string(),
                    secret: "a-secret".to_string(),
                    expires_at: Utc::now() + Duration::days(30),
                },
                Utc::now() - Duration::days(12),
            )
            .expect("issue the grant");
        let assertions = OutwardAssertionStore::new(layout.clone());
        let roster = Audience::new(audience.clone(), vec![PARTNER.to_string()]);
        let holders = vec![PARTNER.to_string()];
        for artifact_ref in [DECK, FINANCIALS] {
            rooms
                .add_document(
                    &room_scope,
                    &room.room_id,
                    artifact_ref,
                    DocumentVisibility::Everyone,
                    "owner",
                    &GrantDisclosure {
                        assertions: &assertions,
                        audience: &roster,
                        holders: &holders,
                        disclosed_by: "owner",
                    },
                    Utc::now() - Duration::days(12),
                )
                .expect("put the document in the room");
        }
        // Three visits, and the financials were never reached.
        let access = AccessStore::new(layout.clone());
        let access_scope = AccessScope::new("anonymous", "default");
        for (sequence, document, days_ago) in
            [(1u32, None, 11i64), (2, Some(DECK), 10), (3, Some(DECK), 9)]
        {
            access
                .record_access(
                    &access_scope,
                    &room.room_id,
                    &AccessEvent {
                        room_id: room.room_id.clone(),
                        audience: audience.clone(),
                        token_issued_to: PARTNER.to_string(),
                        document_ref: document.map(str::to_string),
                        occurred_at: Utc::now() - Duration::days(days_ago),
                        dwell_ms: None,
                        sequence,
                        user_agent_class: UserAgentClass::Desktop,
                    },
                )
                .expect("record the visit");
        }
        assert_eq!(
            access
                .events_for(&access_scope, &room.room_id)
                .expect("read the access lane")
                .len(),
            3,
            "the seeded visits must be readable before anything is asserted about the read"
        );

        let app = app!(&dir);
        // The route's default windows and the API's reading windows are the
        // same numbers; a drift between them is what makes a handle address a
        // row that was never written.
        let (status, swept) = post!(app, "/work/obligations/sweep", json!({}));
        assert_eq!(status, 200, "{swept}");
        assert_eq!(swept["rooms_seen"], 1);
        assert_eq!(swept["recorded"], 1);

        let (status, owed) = get!(app, "/work/obligations");
        assert_eq!(status, 200, "{owed}");
        assert_eq!(owed["counts"]["total"], 1);
        assert_eq!(owed["readings"]["available"], true);
        assert_eq!(owed["readings"]["counts"]["explaining_a_row"], 1);
        assert_eq!(owed["readings"]["counts"]["row_not_in_register"], 0);

        let row = &owed["obligations"][0];
        // The register's own text carries neither the count nor the document —
        // it cannot, and that is why the reading exists.
        let what = row["what"].as_str().expect("the recorded text");
        assert!(
            !what.contains("3 times") && !what.contains(FINANCIALS),
            "the recorded text must stay free of the visit count and the unread document: \
             `{what}`"
        );
        assert_eq!(row["reading"]["reading"], "returned_then_silent");
        assert_eq!(row["reading"]["visits"], 3);
        assert_eq!(row["reading"]["documents_opened"][0], DECK);
        assert_eq!(row["reading"]["documents_unopened"][0], FINANCIALS);
        assert_eq!(
            row["reading"]["obligation_id"], row["obligation_id"],
            "a reading must address the row it is printed beside"
        );
    }

    /// A sweep window at or below zero is refused, never folded into an empty
    /// sweep.
    ///
    /// Pins the flood: at zero every offer is silent the instant it is made,
    /// and the rows it would write into an append-only register cannot be
    /// un-written.
    #[actix_web::test]
    async fn a_non_positive_sweep_window_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        for body in [
            json!({ "silence_window_hours": 0 }),
            json!({ "delivery_question_after_hours": 0 }),
            json!({ "follow_up_after_hours": -1 }),
        ] {
            let (status, refusal) = post!(app, "/work/obligations/sweep", body);
            assert_eq!(status, 400, "{refusal}");
        }
        let (status, ok) = post!(app, "/work/obligations/sweep", json!({}));
        assert_eq!(status, 200, "the shipped defaults sweep: {ok}");
        assert_eq!(ok["recorded"], 0, "an empty scope has nothing to record");
    }

    // ── guards ──────────────────────────────────────────────────────────────

    /// A caller string carrying U+001F is refused before any store is touched.
    ///
    /// Pins the id-fusion failure: the register joins its components with that
    /// separator and does NOT check, so a crafted `what` could address one
    /// tenant's row from another's request.
    #[actix_web::test]
    async fn a_separator_bearing_component_is_refused_before_the_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (status, refusal) = post!(
            app,
            "/work/obligations",
            json!({
                "audience_kind": "account",
                "audience_id": format!("acct{}1", SEP),
                "what": "the metrics",
                "due_at": "2026-09-01T17:00:00Z",
                "direction": "owed_by_us",
                "created_by": "the owner",
            })
        );
        assert_eq!(status, 400, "{refusal}");
        assert!(refusal["error"]
            .as_str()
            .expect("message")
            .contains("U+001F"));

        let (status, refusal) = post!(
            app,
            "/work/obligations",
            json!({
                "audience_kind": "account",
                "audience_id": "acct-1",
                "what": format!("the metrics{}by friday", SEP),
                "due_at": "2026-09-01T17:00:00Z",
                "direction": "owed_by_us",
                "created_by": "the owner",
            })
        );
        assert_eq!(status, 400, "{refusal}");

        let register = ObligationStore::new(ArtifactV2Workspace::new(dir.path().to_path_buf()));
        assert!(
            register
                .all_obligations(&ObligationScope::new("anonymous", "default"))
                .expect("read")
                .is_empty(),
            "neither refusal may have written a row"
        );
    }

    /// An unreadable log is a fault, never an empty listing.
    ///
    /// Pins the most reassuring wrong answer this surface could give: "nothing
    /// is owed" out of a disk fault.
    #[actix_web::test]
    async fn an_unreadable_log_is_a_fault_not_an_empty_listing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        post!(app, "/work/runs", open_run_body());

        // The listing works before the log is broken.
        let (status, listed) = get!(app, "/work/runs");
        assert_eq!(status, 200);
        assert_eq!(listed["counts"]["total"], 1);

        let run_dir = ArtifactV2Workspace::new(dir.path().to_path_buf())
            .scope_root("anonymous", "default")
            .join("run_state");
        let log = std::fs::read_dir(&run_dir)
            .expect("the run directory must exist")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .expect("one run log");
        let mut raw = std::fs::read_to_string(&log).expect("read");
        // A TERMINATED unparseable line is corruption, not a torn tail.
        raw.push_str("{not json}\n");
        std::fs::write(&log, raw).expect("write");

        let (status, fault) = get!(app, "/work/runs");
        assert_eq!(status, 500, "{fault}");
        assert_eq!(fault["code"], "work_store_unreadable");
    }

    /// A missing run is 404, never an empty outstanding list.
    ///
    /// Pins the vacuous truth that would let a mistyped id read as a finished
    /// form.
    #[actix_web::test]
    async fn a_missing_run_is_not_a_run_with_nothing_outstanding() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        // Seed a real run so the 404 cannot come from an empty scope.
        let (_, opened) = post!(app, "/work/runs", open_run_body());
        assert!(opened["run"]["run_id"].is_string());

        let (status, missing) = get!(app, "/work/runs/run-doesnotexist");
        assert_eq!(status, 404, "{missing}");
        assert_eq!(missing["code"], "run_not_found");
    }

    /// Scope is required before anything else is considered.
    ///
    /// Pins the tenant leak: a listing that defaulted the principal would hand
    /// one owner's runs and register to whoever asked without one.
    #[actix_web::test]
    async fn a_read_without_scope_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        for uri in [
            "/work/runs",
            "/work/negotiations",
            "/work/obligations",
            "/work/claim-manifests?artifact_ref=art-1",
        ] {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::get().uri(uri).to_request(),
            )
            .await;
            assert_eq!(
                response.status().as_u16(),
                400,
                "{uri} must refuse without a scope"
            );
        }
    }

    /// A run's own store is what the surface reads — the listing is not a
    /// second source of truth.
    ///
    /// Pins the drift a cached listing would introduce: the owner's page and
    /// the submit gate must agree about which required fields are unanswered.
    #[actix_web::test]
    async fn the_listing_reads_the_same_store_the_gate_does() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app!(&dir);
        let (_, opened) = post!(app, "/work/runs", open_run_body());
        let run_id = opened["run"]["run_id"].as_str().expect("id").to_string();

        let store = RunStateStore::new(ArtifactV2Workspace::new(dir.path().to_path_buf()));
        let scope = RunScope::new("anonymous", "default");
        let direct = store.load(&scope, &run_id).expect("read").expect("the run");
        assert_eq!(direct.fields.len(), 2);

        let (_, listed) = get!(app, "/work/runs");
        assert_eq!(listed["runs"][0]["run_id"], run_id);
        assert_eq!(listed["runs"][0]["fields"], 2);
        assert_eq!(listed["runs"][0]["fields_pending"], 2);
        assert_eq!(listed["counts"]["drafting"], 1);
    }
}
