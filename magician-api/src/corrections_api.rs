//! **We said something that turned out to be wrong. Who did we say it to?**
//!
//! Doc: `docs/plans/2026-08-07-opc-outward-assertions.md` phase 4. Two halves,
//! and only one of them existed as anything a person could reach.
//!
//! The **record** half — every disclosure that carried a claim, every disclosure
//! that rested on a source — was built and indexed at write time, which the
//! store's own note calls the feature: *"the reverse lookup IS the feature"*,
//! because a scan would be correct and would also be the thing nobody runs. The
//! **acting** half — raising a correction obligation against each affected
//! recipient, and recording what was decided about it — was built too, and had
//! zero production callers. So the store could answer *"who did we tell"* and
//! there was no way to ask.
//!
//! # What a correction obligation is, and is not
//!
//! It is a **debt**, pointed at an assertion that already went out. It never
//! rewrites the assertion: nothing here touches an act or an assertion-use row,
//! and they never learn about the obligation. A record a later event can edit is
//! not an audit trail.
//!
//! Three properties come from the store and this surface cannot soften them:
//!
//! - **Only ACTIVE disclosures raise a debt.** A `prepared` act never left and a
//!   `failed` one told nobody; raising against either would be inventing a debt.
//! - **`dispatch_unknown` DOES raise one.** It might have reached somebody, and
//!   the honest position is that we owe a check.
//! - **Idempotent on `(correction_ref, assertion_use_id)`.** Raising twice
//!   returns the same obligations rather than doubling them, so a caller that
//!   never saw our first response can retry.
//!
//! # "Nothing" is a resolution
//!
//! `resolve` takes free text, and *"we decided to say nothing"* is a legitimate
//! answer that must be recordable. A register that can only be closed by having
//! done something fills with debts nobody can close and stops being read, which
//! is worse than one that records an owner deciding to let it stand.

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::{AudienceKind, AudienceRef};
use magician::magician_v2::evidence::{ObligationState, OutwardAssertionStore, OutwardScope};
// The full module path, not the `evidence` re-export: these three are read by
// this one route and adding them to that list would touch a file three other
// lanes are editing. `outward_assertions` is a `pub mod`, so the path is stable.
use magician::magician_v2::evidence::outward_assertions::{
    AudienceActs, DEFAULT_AUDIENCE_ACT_LIMIT, MAX_AUDIENCE_ACT_LIMIT,
};

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;

pub struct CorrectionsApi {
    workspace_layout: ArtifactV2Workspace,
}

impl CorrectionsApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn store(&self) -> OutwardAssertionStore {
        OutwardAssertionStore::new(self.workspace_layout.clone())
    }

    /// The layout the served-claim read builds its own stores over.
    ///
    /// The claim manifests and the data rooms are separate registers with their
    /// own scopes; this surface is the coordinator that reads both, so it hands
    /// out the root rather than holding three handles that could be rooted
    /// differently.
    fn workspace(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }
}

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

/// A store that could not be read.
///
/// **Never an empty listing.** An unreadable index folded to "no affected
/// disclosures" says we told nobody, which is the one wrong answer that ends the
/// investigation.
fn store_unreadable(what: &str, error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "assertion_store_unreadable",
        format!("{what} could not be read, so the answer is unknown — and unknown is not `none`"),
        Some(json!({ "detail": format!("{error:#}") })),
    )
}

fn scope_of(req: &HttpRequest, workspace: Option<String>) -> Result<OutwardScope, HttpResponse> {
    let (principal, workspace) = resolve_required_scope(req.headers(), workspace)?;
    Ok(OutwardScope::new(principal, workspace))
}

// ---------------------------------------------------------------------------
// The reverse lookups
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AffectedQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Exactly one of these. `claim_ref` asks *"who did we tell this"*;
    /// `evidence_ref` asks the other direction — *"when this source turned out
    /// to be wrong, what did we say on the strength of it"*.
    #[serde(default)]
    pub claim_ref: Option<String>,
    #[serde(default)]
    pub evidence_ref: Option<String>,
}

/// `GET /api/magician/v2/assertions/affected`
///
/// Every disclosure that carried a claim, or rested on a source.
pub async fn affected_disclosures_handler(
    req: HttpRequest,
    query: web::Query<AffectedQuery>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    let query = query.into_inner();
    let scope = match scope_of(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = api.store();
    // Exactly one, refused rather than defaulted. Serving both would answer a
    // question nobody asked; picking one silently would answer a different
    // question from the one the caller thinks it asked, and the two indexes
    // return different sets.
    let found = match (query.claim_ref.as_deref(), query.evidence_ref.as_deref()) {
        (Some(claim_ref), None) => store.disclosures_carrying_claim(&scope, claim_ref),
        (None, Some(evidence_ref)) => store.disclosures_resting_on_evidence(&scope, evidence_ref),
        _ => {
            return bad_request(
                "give exactly one of `claim_ref` or `evidence_ref`; they read different indexes \
                 and return different sets, so answering both or guessing between them would \
                 answer a question the caller did not ask",
            )
        },
    };
    match found {
        Ok(disclosures) => {
            let active = disclosures
                .iter()
                .filter(|held| held.status.is_active_disclosure())
                .count();
            HttpResponse::Ok().json(json!({
                "count": disclosures.len(),
                // Reported beside the total because only these would raise a
                // debt: a `prepared` act never left and a `failed` one told
                // nobody.
                "active_disclosures": active,
                "disclosures": disclosures,
            }))
        },
        Err(error) => store_unreadable("the outward assertion index", &error),
    }
}

// ---------------------------------------------------------------------------
// Raising and settling the debt
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RaiseCorrectionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The claim that turned out to be wrong.
    pub approved_claim_ref: String,
    /// What the correction IS — the new claim, the retraction note, the revised
    /// figure. Part of the obligation's derived id, so two different corrections
    /// to one claim are two sets of obligations rather than one overwritten set.
    pub correction_ref: String,
}

/// `POST /api/magician/v2/corrections`
pub async fn raise_correction_handler(
    req: HttpRequest,
    body: web::Json<RaiseCorrectionRequest>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match scope_of(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if body.approved_claim_ref.trim().is_empty() || body.correction_ref.trim().is_empty() {
        return bad_request(
            "both `approved_claim_ref` and `correction_ref` are required; the obligation id \
             derives from the correction, so an empty one would collapse every correction to a \
             claim into a single set",
        );
    }
    match api.store().raise_correction_obligations(
        &scope,
        &body.approved_claim_ref,
        &body.correction_ref,
        &Utc::now().to_rfc3339(),
    ) {
        Ok(raised) => HttpResponse::Ok().json(json!({
            "correction_ref": body.correction_ref,
            "approved_claim_ref": body.approved_claim_ref,
            // Counts, not a boolean. Raising zero obligations is a real and
            // common answer — the claim went out through no active disclosure —
            // and it must not read like the call failing.
            "raised": raised.len(),
            "obligations": raised,
        })),
        Err(error) => store_unreadable("the disclosures carrying this claim", &error),
    }
}

#[derive(Debug, Deserialize)]
pub struct CorrectionListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// `open` (the default) or `all`. Open is what somebody has to act on; all
    /// is what an auditor reads.
    #[serde(default)]
    pub state: Option<String>,
}

/// `GET /api/magician/v2/corrections/{correction_ref}`
pub async fn list_correction_obligations_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CorrectionListQuery>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    let query = query.into_inner();
    let scope = match scope_of(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = api.store();
    let correction_ref = path.into_inner();
    let wanted = query.state.as_deref().unwrap_or("open");
    let found = match wanted {
        "open" => store.open_obligations_for_correction(&scope, &correction_ref),
        "all" => store.obligations_for_correction(&scope, &correction_ref),
        other => return bad_request(format!("unknown state `{other}`; expected `open` or `all`")),
    };
    match found {
        Ok(obligations) => HttpResponse::Ok().json(json!({
            "correction_ref": correction_ref,
            "state": wanted,
            "count": obligations.len(),
            "obligations": obligations,
        })),
        Err(error) => store_unreadable("the correction obligation register", &error),
    }
}

#[derive(Debug, Deserialize)]
pub struct ResolveObligationRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// What was decided. Free text, and *"nothing"* is a legitimate answer —
    /// correct, replace, withdraw or let it stand is the owner's call, and a
    /// register that can only be closed by having acted fills with debts nobody
    /// can close.
    pub resolution: String,
}

/// `POST /api/magician/v2/corrections/obligations/{obligation_id}/resolve`
pub async fn resolve_obligation_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveObligationRequest>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    let body = body.into_inner();
    let scope = match scope_of(&req, body.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if body.resolution.trim().is_empty() {
        return bad_request(
            "`resolution` must say what was decided; an empty one closes a debt with no record \
             of why, which is indistinguishable from losing it",
        );
    }
    let obligation_id = path.into_inner();
    // Looked up before resolving, so that a store which cannot be READ is not
    // reported as a debt that does not EXIST. `resolve_obligation` bails for
    // both, and an operator working down a queue of obligations reads 404 as
    // "already handled" — which is how an unreadable register discharges every
    // debt in it. `load_obligation` separates them: `None` is absent, `Err` is
    // unreadable, and only the first is a 404.
    let held = match api.store().load_obligation(&scope, &obligation_id) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return api_error_response(
                StatusCode::NOT_FOUND,
                "correction_obligation_not_found",
                format!("no correction obligation `{obligation_id}` in this workspace"),
                None,
            )
        },
        Err(error) => {
            return api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "correction_obligation_unreadable",
                format!(
                    "the obligation register could not be read, so whether `{obligation_id}` is \
                     owed is unknown — which is not the same as it not being owed: {error:#}"
                ),
                None,
            )
        },
    };
    // Recorded rather than refused. The store is append-only, so an earlier
    // resolution is still in the log — but the FOLD takes the last one, so
    // re-resolving quietly replaces the reason a debt was discharged. Saying so
    // in the response is what keeps that from happening unnoticed.
    let previously_resolved = held.state == ObligationState::Resolved;
    match api.store().resolve_obligation(
        &scope,
        &obligation_id,
        &body.resolution,
        &Utc::now().to_rfc3339(),
    ) {
        Ok(()) => HttpResponse::Ok().json(json!({
            "obligation_id": obligation_id,
            "state": "resolved",
            "resolution": body.resolution,
            "was_already_resolved": previously_resolved,
            "previous_resolution": held.resolution,
        })),
        // Not a 404. The obligation was just read, so it exists; a failure here
        // is the WRITE failing, and the debt is still owed.
        Err(error) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "correction_obligation_not_resolved",
            format!(
                "`{obligation_id}` is owed and could not be resolved, so it is still owed: \
                 {error:#}"
            ),
            None,
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct ServedClaimQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// The claim that turned out to be wrong.
    pub claim_ref: String,
    /// What the correction IS. The proposed obligations point at it; one
    /// pointing nowhere cannot be acted on.
    pub correction_ref: String,
}

/// `GET /api/magician/v2/corrections/served`
///
/// Which data rooms are serving a retracted claim, and what that owes.
///
/// The **served** half of a retraction. `GET /assertions/affected` answers who
/// we TOLD; this answers who can still read it — a deck sitting in a room is not
/// an outward act, it is a standing offer to read one, and a correction chasing
/// only what was emailed leaves the wrong number on a page somebody can open
/// today.
///
/// # It proposes, and it does not withdraw anything
///
/// Nothing is written and no document is removed. Withdrawing automatically
/// would be exactly the silent swap §4 forbids: a room whose contents change
/// under a reader, with no event, is the failure this machinery exists to make
/// impossible.
///
/// # Read `unpinned` and `past` — they are the two that mislead
///
/// - **`unpinned`** — the room names an artifact with no revision, so it serves
///   whatever is current and nothing here can say whether that carries the
///   claim. Counted apart rather than folded either way.
/// - **`exposure: "past"`** — withdrawn, or the room is closed. Access is
///   blocked; **a copy already taken is not recalled**, so the conversation is
///   still owed and no action available now changes that.
pub async fn served_claim_handler(
    req: HttpRequest,
    query: web::Query<ServedClaimQuery>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    use magician::magician_v2::claim_manifest::{ClaimManifestScope, ClaimManifestStore};
    use magician_learning::data_room::{DataRoomScope, DataRoomStore};
    use magician_learning::retraction::retraction_sweep;

    let query = query.into_inner();
    let scope = match scope_of(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let manifests = ClaimManifestStore::new(api.workspace().clone());
    let rooms = DataRoomStore::new(api.workspace().clone());
    let manifest_scope = ClaimManifestScope::new(scope.principal.clone(), scope.workspace.clone());
    let room_scope = DataRoomScope::new(scope.principal.clone(), scope.workspace.clone());

    match retraction_sweep(
        &manifests,
        &rooms,
        &manifest_scope,
        &room_scope,
        &query.claim_ref,
        &query.correction_ref,
        "served-claim-read",
        Utc::now(),
    ) {
        Ok(sweep) => HttpResponse::Ok().json(json!({
            "claim_ref": query.claim_ref,
            "correction_ref": query.correction_ref,
            // The denominators. "Reached nothing" over one room and over three
            // hundred are different facts, and so is "no revision carries it".
            "rooms_seen": sweep.rooms_seen,
            "revisions_carrying": sweep.revisions_carrying,
            "affected": sweep.affected(),
            // Non-empty means the sweep GUESSED at how to read these, and a
            // wrong guess matches no carried artifact — so a room that does
            // carry the claim is skipped without appearing anywhere above.
            // "Found none" and "could not read three of them" have to be
            // different answers on a correction.
            "ambiguous_document_refs": &sweep.ambiguous_document_refs,
            "confirmed": sweep.confirmed.iter().map(affected_room_json).collect::<Vec<_>>(),
            "unpinned": sweep.unpinned.iter().map(affected_room_json).collect::<Vec<_>>(),
            "proposed_obligations": sweep
                .proposed
                .iter()
                .map(|held| json!({
                    "audience": held.audience,
                    "what": held.what,
                    "due_at": held.due_at,
                    "direction": held.direction.as_str(),
                    "source_act_ref": held.source_act_ref,
                }))
                .collect::<Vec<_>>(),
        })),
        Err(error) => store_unreadable("the claim manifests and data rooms", &error),
    }
}

#[derive(Debug, Deserialize)]
pub struct AudienceActsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// `engagement`, `program`, `account`, `panel` or `person`.
    ///
    /// Required and parsed, never defaulted: an audience is `(kind, id)` and
    /// the kind is half the key, so `acme` alone cannot be told apart from an
    /// account, a programme and an engagement of the same name.
    pub kind: String,
    /// The relationship's own id, **as it was filed**.
    ///
    /// Matched byte-for-byte against the index, because that is what the writer
    /// keyed on. A padded value is refused rather than trimmed: trimming would
    /// read a different index file from the one an act was filed under, and the
    /// answer would come back as `indexed: 0` — *"we told them nothing"* — from
    /// a stray space.
    pub id: String,
    /// How many acts to open, up to [`MAX_AUDIENCE_ACT_LIMIT`].
    /// Defaults to [`DEFAULT_AUDIENCE_ACT_LIMIT`].
    #[serde(default)]
    pub limit: Option<usize>,
    /// The previous page's `next_after`, to continue from.
    ///
    /// A keyset cursor rather than an offset. Act refs sort stably, so "after
    /// this one" means the same thing across pages even when the index gains
    /// entries or is rewritten by a repair between them — an offset would
    /// silently skip or repeat rows in exactly that case, which on a
    /// **correction** is a room left unflagged.
    ///
    /// Absent starts at the beginning. Blank is treated as absent rather than
    /// refused: an empty query parameter is how a client renders "no cursor".
    #[serde(default)]
    pub after: Option<String>,
}

/// `GET /api/magician/v2/corrections/audience`
///
/// **Which acts did we perform for this audience?**
///
/// The counterparty-shaped question, next to the two claim-shaped ones.
/// `GET /assertions/affected` asks what a claim reached and
/// `GET /corrections/served` asks who can still open a room holding it; this
/// asks the question somebody chasing a correction asks first — *what have we
/// told these people at all* — and until now the index that answers it was
/// written on every room grant and read by nothing.
///
/// # Three numbers to read before the list
///
/// - **`indexed`** is the denominator: how many acts the relationship has
///   **filed against it**, whole, on every page. It is NOT what this page
///   could have returned — `remaining` is, and on the first page (no `after`)
///   the two are equal.
///
///   The identity that holds on every page is `examined == count +
///   unresolved`: every entry this read opened either became an act or is
///   counted as one it could not. `remaining - examined` is what the cap
///   dropped and `next_after` is how to ask for it; `indexed - remaining` is
///   what earlier pages already served. A page whose `capped_at` is null and
///   whose `count` is still short of `examined` lost a disclosure — that
///   difference is `unresolved` and nothing else.
/// - **`unresolved`** counts index entries that resolved to no act. Non-zero
///   means this store filed a disclosure it can no longer produce, which is a
///   data fault to chase and not an act to correct.
/// - **`active_disclosures`** is the subset **of the acts returned** that would
///   raise a debt — a `prepared` act never left and a `failed` one told nobody.
///   It is counted over `acts` and not over `indexed`, so when `capped_at` is
///   set it is a **floor** and not the number owed: the acts past the cap were
///   never opened and their statuses are unknown. Raise `limit` before quoting
///   it as a total.
///
/// # `limit` has a ceiling, and `after` is how to get past it
///
/// `limit` is refused above [`MAX_AUDIENCE_ACT_LIMIT`], because it is a number
/// of file opens and a query parameter passed straight through would otherwise
/// buy an unbounded scan. It bounds ONE page, not the relationship: an audience
/// with more filings than the ceiling is read whole by paging — send
/// `next_after` back as `after` until it comes home absent.
///
/// `next_after` is the ONLY end signal. A short `acts` list is not one: the cap
/// counts index entries opened, and an entry that resolved to no act is opened
/// and counted in `unresolved` without adding to `acts`, so a full page can
/// come back short and still have more behind it.
///
/// `capped_at` says *this page was cut short*, so it is null on the last page
/// however large `indexed` is. An operator who has paged to a null `next_after`
/// has seen the relationship; one who stopped at a set `capped_at` has not, and
/// closing a correction there closes it on a list that merely looked complete.
///
/// # `audience: null` on an act does not mean it was performed for nobody
///
/// The register is append-only, so a row is never rewritten to look tidier than
/// its history was. An act first written by the plain dispatch path — which
/// knows the work it served, not the relationship — and only afterwards filed
/// under this audience by the resume path therefore carries the filing and no
/// audience of its own; so does any row written before the record field
/// existed at all. Those are counted as `audience_not_on_record`, and they are
/// **answers**: the index entry is the evidence, and the record's silence is
/// only the absence of a second copy. Treat them exactly like
/// `confirmed_by_record`.
///
/// The one that needs a second look is `recorded_for_another_audience` — the
/// act is filed here and its record names somebody else, which happens when one
/// idempotency key was prepared for two relationships. Both filings are true;
/// the record is simply the first one's.
///
/// # An empty answer is "not filed", not "never told"
///
/// Nothing dispatched before the audience axis existed is filed under it, and
/// nothing can backfill a relationship a record never carried. So `indexed: 0`
/// over a workspace with a long history means this index has nothing to say,
/// not that the workspace told this counterparty nothing.
pub async fn audience_acts_handler(
    req: HttpRequest,
    query: web::Query<AudienceActsQuery>,
    api: web::Data<CorrectionsApi>,
) -> HttpResponse {
    let query = query.into_inner();
    let scope = match scope_of(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // Refused rather than defaulted, for the reason `AudienceKind::parse`
    // gives: falling back to `Engagement` would answer a panel's question out
    // of the engagement index and report the result as acts performed for that
    // panel.
    let Some(kind) = AudienceKind::parse(&query.kind) else {
        return bad_request(format!(
            "`{}` is not an audience kind; this build knows {}. An unrecognised kind is refused \
             rather than defaulted, because answering out of another kind's index would report \
             acts performed for a relationship nobody asked about",
            query.kind,
            AudienceKind::ALL
                .iter()
                .map(|known| known.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    if query.id.trim().is_empty() {
        return bad_request(
            "`id` must name the relationship; every unnamed audience shares one index file, so a \
             blank id would answer with acts performed for somebody else",
        );
    }
    // Not trimmed — refused. The writer keys the index on the id exactly as it
    // holds it, so trimming here would read a different file and answer
    // `indexed: 0` for a relationship that has disclosures.
    if query.id.trim() != query.id {
        return bad_request(
            "`id` has leading or trailing whitespace; the index is keyed on the id exactly as it \
             was filed, so a padded one reads a different file and would answer `nothing found` \
             for a relationship that has disclosures",
        );
    }
    let limit = query.limit.unwrap_or(DEFAULT_AUDIENCE_ACT_LIMIT);
    if limit == 0 || limit > MAX_AUDIENCE_ACT_LIMIT {
        return bad_request(format!(
            "`limit` must be between 1 and {MAX_AUDIENCE_ACT_LIMIT}, not {limit}: zero reports \
             `no acts` for an audience that has some, and an unbounded one turns a query string \
             into one file open per index entry"
        ));
    }
    let audience = AudienceRef::new(kind, query.id.as_str());
    // `after` is a keyset cursor: the last act ref of the previous page. It is
    // what makes an audience with more filings than one page can hold
    // enumerable at all — the cap alone could only ever show the first N, and a
    // caller had no way to ask for the rest.
    let after = query
        .after
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match api
        .store()
        .acts_for_audience(&scope, &audience, limit, after)
    {
        Ok(answer) => HttpResponse::Ok().json(audience_acts_json(&answer, limit)),
        // Only ever a read fault, and so only ever a 500. The store's other two
        // refusals — a blank id and a limit outside the bound — are checked
        // above against the same conditions, so a caller's mistake cannot reach
        // this arm and be reported as a store that could not be read.
        Err(error) => store_unreadable("the audience index for this relationship", &error),
    }
}

/// The response body, split out so the counts are written once and read as a
/// block: they only mean anything together.
fn audience_acts_json(answer: &AudienceActs, limit: usize) -> serde_json::Value {
    json!({
        "audience": &answer.audience,
        // The key the store files under, so a caller comparing two responses
        // does not have to re-derive that `engagement:acme` and `account:acme`
        // are different relationships.
        "audience_key": answer.audience.as_key(),
        "limit": limit,
        // The denominators. "Told them nothing" over an empty index and "told
        // them four hundred things, here are the first fifty" are different
        // facts, and `count` alone cannot separate them.
        "indexed": answer.indexed,
        "examined": answer.examined,
        "capped_at": answer.capped_at,
        // The end-of-relationship signal, and the ONLY one. A caller must not
        // infer the end from a short page: the cap is applied after the cursor
        // filter, so a page can be short and still have more behind it.
        // Present means "ask again with this as `after`"; absent means done.
        "next_after": &answer.next_after,
        "remaining": answer.remaining,
        "count": answer.acts.len(),
        // Non-zero means a filed disclosure can no longer be produced. Reported
        // rather than dropped: silently skipping it would make the remaining
        // list look like the whole one.
        "unresolved": answer.unresolved,
        // These three partition `acts`. See this handler's note: only the last
        // is a disagreement, and a null audience is not one.
        "confirmed_by_record": answer.confirmed_by_record,
        "audience_not_on_record": answer.audience_not_on_record,
        "recorded_for_another_audience": answer.recorded_for_another_audience,
        // The subset OF THE ACTS RETURNED that would raise a debt, on the same
        // reading `/assertions/affected` uses. Counted over `acts` rather than
        // over `indexed`, because the acts past the cap were never opened and
        // their statuses are unknown — so when `capped_at` is set this is a
        // floor, and quoting it as the number owed would report a truncated
        // correction as a complete one. `/assertions/affected` has no cap, so
        // there the same field IS the total; here it is only when `capped_at`
        // is null.
        "active_disclosures": answer
            .acts
            .iter()
            .filter(|act| act.status.is_active_disclosure())
            .count(),
        "acts": &answer.acts,
    })
}

fn affected_room_json(room: &magician_learning::retraction::AffectedRoom) -> serde_json::Value {
    json!({
        "room_id": room.room_id,
        "audience": room.audience,
        "document_ref": room.document_ref,
        "artifact_ref": room.artifact_ref,
        "revision_ref": room.revision_ref,
        "certainty": room.certainty.as_str(),
        "exposure": room.exposure.as_str(),
    })
}

pub fn configure_correction_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/assertions/affected",
        web::get().to(affected_disclosures_handler),
    )
    .route("/corrections", web::post().to(raise_correction_handler))
    .route("/corrections/served", web::get().to(served_claim_handler))
    // Before `/corrections/{correction_ref}`, like `/corrections/served`: the
    // parameterised route matches any single segment, so registering it first
    // would make this one a correction ref spelled `audience`.
    .route(
        "/corrections/audience",
        web::get().to(audience_acts_handler),
    )
    .route(
        "/corrections/{correction_ref}",
        web::get().to(list_correction_obligations_handler),
    )
    .route(
        "/corrections/obligations/{obligation_id}/resolve",
        web::post().to(resolve_obligation_handler),
    );
}
