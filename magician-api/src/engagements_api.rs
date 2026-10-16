//! Owner-facing HTTP surface for engagement authority.
//!
//! `magician::magician_v2::engagements` shipped the authority carrier with a
//! `list()` and no route, so an owner could not see — let alone withdraw — what
//! an engagement was allowed to do, and nothing anywhere could mint one. Five
//! reads and three writes close that:
//!
//! - `GET   /engagements` — every engagement in the authenticated scope, each
//!   with the ceiling that is **live right now**;
//! - `GET   /engagements/surface` — the cross-engagement read of §4.2b: one
//!   counterparty's whole surface, across every engagement that names them;
//! - `GET   /engagements/{engagement_id}` — one engagement;
//! - `GET   /engagements/{engagement_id}/audience` — **who it may reach**: the
//!   counterparty label resolved against the register, as verified addresses;
//! - `POST  /engagements` — **bound a piece of work**: who the counterparty is,
//!   what may be done, and when it lapses;
//! - `PATCH /engagements/{engagement_id}` — renew it, keeping its id;
//! - `POST  /engagements/{engagement_id}/revoke` — withdraw it.
//!
//! # The write path, and why its absence disabled five subsystems
//!
//! `EngagementStore::create` had no caller outside its own tests. No engagement
//! meant no `EngagementAuthorityRef` on any execution, so the envelope scope
//! resolved to `None`, every work binding read as `Unbound`, retrieval never
//! confined and the maturity sweep found nothing to sweep. Every one of those
//! five is *correct* behaviour given no authority exists — which is exactly why
//! nothing looked broken. `POST /engagements` is the missing act.
//!
//! # Stated as work, not as an engagement
//!
//! The create body carries a
//! [`magician::magician_v2::work_context::WorkContextKind`], not a `program_id`
//! and a bag of tool names. An engagement is one shape a bounded piece of work
//! takes; the work context is the shape every flow already holds, and
//! `EngagementStore::grant_for_work` lowers one onto the other. A support
//! triage, a recruiting loop or a vendor review binds through this same route,
//! naming its own program, with nothing in the executor to edit.
//!
//! # The label is joined to the register here, and resolved there
//!
//! An engagement's `counterparty` is an unvalidated owner-typed string. The
//! audience read hands it to
//! `magician::magician_v2::counterparties::audience_for_label`, which lives in
//! the generic module on purpose: the identity machinery must not learn what an
//! engagement is, so the OPC-specific side does the asking. A label matching no
//! organisation comes back as a **lead** with an empty audience — a fact for an
//! owner, never an error and never permission.
//!
//! # Live, never stored
//!
//! Standing here is not a column. Every row asks
//! [`EngagementStore::live_authority`] — the same call
//! [`authorize_engagement_dispatch`] makes before a side effect — and reports
//! what it answered. That is deliberate: if the surface derived liveness its own
//! way it could say `live` about an engagement dispatch refuses, or the reverse,
//! and an owner would have no way to tell which one was lying. Revocation and
//! expiry are computed against the clock at read time, and expiry is inclusive
//! (`now_ms >= expires_at_ms` is expired) because that is what the carrier does.
//!
//! # Revocation is effective mid-run, and this surface does not weaken that
//!
//! `authorize_engagement_dispatch` reads the store on **every** consequential
//! dispatch rather than trusting the revision an execution carries. So revoking
//! through this route lands on the next dispatch of an execution that is already
//! running — nothing needs chasing, because nothing was ever copied out. The
//! route calls `EngagementStore::revoke` and adds no cache, no snapshot and no
//! "effective from" field that could make a withdrawal look deferred.
//!
//! # Scope is the whole security property of the read
//!
//! `EngagementStore::revoke` and `narrow` find by id across the **whole roster**
//! — only `live_authority` and `list` filter by `(principal, workspace)`. So
//! every mutation here checks the id is in the caller's own scope *before*
//! touching the store, and every read is built from `list()`, which filters.
//! A surface that leaked another tenant's engagements would be worse than no
//! surface at all.
//!
//! # Generic
//!
//! An engagement is a bounded grant for a piece of outward work with one
//! counterparty. Nothing in this module knows what the work is: a supplier
//! negotiation, a hiring loop, a support escalation and a fundraising
//! conversation are the same shape, and the vocabulary here stays at that level.

use std::collections::{BTreeMap, BTreeSet};

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::approval_envelopes_api::{guard_derivation_component, guard_path_id};
use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::audience::{Audience, AudienceKind};
use magician::magician_v2::counterparties::{
    audience_for_label, counterparty_id_for, global_counterparty_store, resolve_label,
    CounterpartyScope, LabelStanding,
};
use magician::magician_v2::engagements::{
    global_engagement_store, program_id_for_work, AuthorityDenial, EngagementAuthority,
    EngagementStore, LiveAuthority,
};
use magician::magician_v2::execution::builtin_action_types::{
    is_delegation_dispatch_policy_name, CEILING_NAMEABLE_NON_PACK_POLICY_NAMES,
};
use magician::magician_v2::execution::capability::CapabilityRegistry;
use magician::magician_v2::work_context::{WorkContext, WorkContextKind};

// ---------------------------------------------------------------------------
// Derived standing
// ---------------------------------------------------------------------------

/// Whether an engagement authorises anything at this instant, and if not why.
///
/// Derived from what the live check answered, never read from a field. The
/// carrier has no "status" column precisely because a stored one would go stale
/// the moment an engagement expired with nobody writing to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementStanding {
    /// The live check granted: this engagement authorises its ceiling now.
    Live,
    Revoked,
    Expired,
    /// The store has no such engagement in this scope.
    Unknown,
    /// The store could not answer, or answered something that is not a grant.
    /// **Never rendered as authority.**
    Unavailable,
}

impl EngagementStanding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
        }
    }

    /// Only `Live` authorises. Everything else — including "we could not tell" —
    /// is the absence of authority, which is the carrier's own fail-closed rule
    /// (§4.2c row 8) carried onto the read surface.
    pub fn authorises_anything(self) -> bool {
        matches!(self, Self::Live)
    }

    /// Map a denial onto a standing.
    ///
    /// Matched exhaustively rather than with a catch-all: a variant added to
    /// `AuthorityDenial` later should make this fail to compile so somebody
    /// decides what it means, instead of being folded into a bucket by default.
    /// Every arm here is a non-authorising answer, which is the invariant that
    /// matters.
    pub fn from_denial(denial: &AuthorityDenial) -> Self {
        match denial {
            AuthorityDenial::UnknownEngagement => Self::Unknown,
            AuthorityDenial::Revoked => Self::Revoked,
            AuthorityDenial::Expired => Self::Expired,
            // Neither of these can come out of `live_authority` — they are
            // per-tool and per-target answers from the dispatch check. If one
            // ever arrives here it is a shape nobody predicted, and the safe
            // reading of an unpredicted answer is "not authority".
            AuthorityDenial::ToolOutsideCeiling { .. }
            | AuthorityDenial::TargetOutsideTeam { .. } => Self::Unavailable,
            AuthorityDenial::StoreUnavailable { .. } => Self::Unavailable,
        }
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One engagement as the owner sees it.
///
/// `live_tool_ceiling` is `None` — not `[]` — when the engagement does not
/// authorise anything. The distinction is the same one `Headroom::remaining()`
/// makes in the envelope projection: an empty list is a real, checkable answer
/// ("authorised, and the ceiling admits no tools"), while absence means "nothing
/// is authorised right now". Collapsing the two would let a revoked engagement
/// render as a live one with an empty ceiling, which reads as *safe* rather than
/// as *dead*.
///
/// The recorded sets are shown alongside so the owner can still see what the
/// engagement was minted with. They are labelled as the record, never as
/// authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngagementRow {
    pub engagement_id: String,
    pub program_id: String,
    pub counterparty: String,
    pub standing: EngagementStanding,
    pub authorises_now: bool,
    /// Why not, when it does not. `None` exactly when `standing` is `Live`.
    pub denial: Option<AuthorityDenial>,
    pub authority_revision: u64,
    pub live_tool_ceiling: Option<Vec<String>>,
    pub live_team: Option<Vec<String>>,
    pub recorded_tool_ceiling: Vec<String>,
    pub recorded_team: Vec<String>,
    /// Counts, not rates: "3 tools" is checkable against the list beside it.
    pub live_tool_count: Option<usize>,
    pub live_team_size: Option<usize>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

/// Project one record plus the live check's answer into a row.
///
/// Pure, so the whole "what does the owner see" question is decidable without a
/// store, a clock or a runtime.
pub fn engagement_row(
    record: &EngagementAuthority,
    live: &Result<LiveAuthority, AuthorityDenial>,
) -> EngagementRow {
    let recorded_tool_ceiling: Vec<String> = record.tool_ceiling.iter().cloned().collect();
    let recorded_team: Vec<String> = record.team.iter().cloned().collect();

    let (standing, denial, revision, live_ceiling, live_team) = match live {
        Ok(authority) => (
            EngagementStanding::Live,
            None,
            authority.authority_revision,
            Some(authority.tool_ceiling.iter().cloned().collect::<Vec<_>>()),
            Some(authority.team.iter().cloned().collect::<Vec<_>>()),
        ),
        Err(denial) => (
            EngagementStanding::from_denial(denial),
            Some(denial.clone()),
            record.authority_revision,
            None,
            None,
        ),
    };

    EngagementRow {
        engagement_id: record.engagement_id.clone(),
        program_id: record.program_id.clone(),
        counterparty: record.counterparty.clone(),
        standing,
        authorises_now: standing.authorises_anything(),
        denial,
        authority_revision: revision,
        live_tool_count: live_ceiling.as_ref().map(|tools| tools.len()),
        live_team_size: live_team.as_ref().map(|team| team.len()),
        live_tool_ceiling: live_ceiling,
        live_team,
        recorded_tool_ceiling,
        recorded_team,
        created_at_ms: record.created_at_ms,
        expires_at_ms: record.expires_at_ms,
        revoked_at_ms: record.revoked_at_ms,
    }
}

// ---------------------------------------------------------------------------
// Who an engagement may reach
// ---------------------------------------------------------------------------

/// A counterparty label nobody has recorded an organisation for.
///
/// The wire form of `counterparties::CounterpartyLead`. It is here rather than
/// omitted because *"the label on this engagement matches no counterparty"* is
/// the one fact an owner can act on in a single decision, and an empty audience
/// with no explanation is the same fact with the action hidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngagementCounterpartyLead {
    pub label: String,
    /// The id the register would file this label under, so recording it lands
    /// on the row this read was already looking for.
    pub would_be_counterparty_id: String,
}

/// One engagement's counterparty, resolved, and who it may reach right now.
///
/// # `covered_identities` is `None`, not `[]`, when nothing is authorised
///
/// The same distinction `live_tool_ceiling` makes, for the same reason.
/// `Some([])` means *"this engagement authorises acts, and the register holds
/// no proved address for its counterparty"* — a checkable answer. `None` means
/// *"this engagement authorises nothing right now"*, and collapsing the two
/// would let a revoked engagement render as a live one that happens to reach
/// nobody, which reads as safe rather than as dead.
///
/// # Resolving is not authorising, on both axes
///
/// `counterparty_standing`, `counterparty_id` and `audience_key` are what the
/// **register** answered about the label. They are reported whatever the
/// engagement's standing, because they are facts about the book rather than
/// grants — exactly as `recorded_tool_ceiling` is shown beside a dead
/// engagement. Nothing may read them as reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngagementAudienceRow {
    pub engagement_id: String,
    pub program_id: String,
    /// The owner-typed label the engagement record carries, verbatim.
    pub counterparty: String,
    pub standing: EngagementStanding,
    pub authorises_now: bool,
    /// `registered` when the register holds this label, `unregistered` when it
    /// is a lead. Never a guess at the nearest name.
    pub counterparty_standing: &'static str,
    /// The surviving record's id, or the id a lead would be filed under.
    pub counterparty_id: String,
    pub audience_kind: &'static str,
    /// `<kind>:<counterparty_id>` — the key this roster is bound under, which
    /// differs per kind so two relationships with one organisation never merge.
    pub audience_key: String,
    /// Verified addresses only, and only while the engagement authorises
    /// something. See the type note for why absence is not emptiness.
    pub covered_identities: Option<Vec<String>>,
    /// Counts, not rates.
    pub covered_identity_count: Option<usize>,
    pub lead: Option<EngagementCounterpartyLead>,
}

/// Project one engagement, its live check, and what the register said about its
/// label into a row.
///
/// Pure, so *"who may this engagement reach"* is decidable without a store, a
/// clock or a runtime — the same property `engagement_row` holds.
///
/// The audience is passed in already resolved because building it is the
/// register's job and deciding whether it may be shown is this function's:
/// separating them is what stops a future edit from making the resolution
/// itself conditional on the engagement's standing, which would hide a
/// counterparty an owner is entitled to see.
pub fn engagement_audience_row(
    record: &EngagementAuthority,
    live: &Result<LiveAuthority, AuthorityDenial>,
    label_standing: &LabelStanding,
    audience: &Audience,
) -> EngagementAudienceRow {
    let standing = match live {
        Ok(_) => EngagementStanding::Live,
        Err(denial) => EngagementStanding::from_denial(denial),
    };
    let authorises_now = standing.authorises_anything();
    // Withheld, not emptied: a dead engagement reaches nobody, and saying so
    // with `None` keeps it distinguishable from a live one whose counterparty
    // has no proved address.
    let covered_identities = authorises_now.then(|| audience.identities.clone());

    EngagementAudienceRow {
        engagement_id: record.engagement_id.clone(),
        program_id: record.program_id.clone(),
        counterparty: record.counterparty.clone(),
        standing,
        authorises_now,
        counterparty_standing: label_standing.as_str(),
        counterparty_id: audience.reference.id.clone(),
        audience_kind: audience.reference.kind.as_str(),
        audience_key: audience.reference.as_key(),
        covered_identity_count: covered_identities
            .as_ref()
            .map(|identities| identities.len()),
        covered_identities,
        lead: label_standing
            .lead()
            .map(|lead| EngagementCounterpartyLead {
                label: lead.label.clone(),
                would_be_counterparty_id: lead.would_be_counterparty_id.clone(),
            }),
    }
}

/// Tally rows by standing. Counts, never rates.
pub fn standing_counts(rows: &[EngagementRow]) -> serde_json::Value {
    let tally =
        |wanted: EngagementStanding| rows.iter().filter(|row| row.standing == wanted).count();
    json!({
        "total": rows.len(),
        "live": tally(EngagementStanding::Live),
        "revoked": tally(EngagementStanding::Revoked),
        "expired": tally(EngagementStanding::Expired),
        "unknown": tally(EngagementStanding::Unknown),
        "unavailable": tally(EngagementStanding::Unavailable),
    })
}

// ---------------------------------------------------------------------------
// The cross-engagement read (§4.2b)
// ---------------------------------------------------------------------------

/// Everything one counterparty is reachable through, across engagements.
///
/// §4.2b: several counterparties in one effort is *a program with N
/// engagements*, and what was missing was only the read model across them. This
/// is the transposed view — pivot on the counterparty rather than the program —
/// because the question an owner actually asks before an outward act is *"what
/// may we already do to this organisation?"*, and the answer is spread across
/// every engagement that names them.
///
/// The unions cover **live engagements only**, and are `None` when none are
/// live. A union that folded in revoked and expired engagements would overstate
/// what is authorised, and an owner would go revoking things the clock already
/// closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CounterpartySurface {
    pub counterparty: String,
    pub engagement_count: usize,
    pub live_count: usize,
    pub revoked_count: usize,
    pub expired_count: usize,
    /// Standings that are neither live nor a normal ending — an engagement the
    /// store could not answer for. Broken out rather than folded into the
    /// others so "we do not know" never reads as "it is over".
    pub indeterminate_count: usize,
    pub programs: Vec<String>,
    pub live_tool_ceiling_union: Option<Vec<String>>,
    pub live_team_union: Option<Vec<String>>,
    pub engagements: Vec<EngagementRow>,
}

/// Group rows by counterparty, optionally narrowed.
///
/// Both filters are **exact matches**, and deliberately so. The counterparty
/// module refuses fuzzy resolution because resolving an address it does not hold
/// hands one organisation's authority to another; the same asymmetry applies to
/// a label. Failing to match a name the owner typed slightly differently is
/// annoying and visible. Matching the wrong organisation shows the owner one
/// party's authority under another's name, and the answer looks correct.
pub fn counterparty_surfaces(
    rows: Vec<EngagementRow>,
    counterparty: Option<&str>,
    program_id: Option<&str>,
) -> Vec<CounterpartySurface> {
    let mut grouped: BTreeMap<String, Vec<EngagementRow>> = BTreeMap::new();
    for row in rows {
        if counterparty.is_some_and(|wanted| wanted != row.counterparty) {
            continue;
        }
        if program_id.is_some_and(|wanted| wanted != row.program_id) {
            continue;
        }
        grouped
            .entry(row.counterparty.clone())
            .or_default()
            .push(row);
    }

    grouped
        .into_iter()
        .map(|(counterparty, engagements)| {
            let tally = |wanted: EngagementStanding| {
                engagements
                    .iter()
                    .filter(|row| row.standing == wanted)
                    .count()
            };
            let live_count = tally(EngagementStanding::Live);
            let revoked_count = tally(EngagementStanding::Revoked);
            let expired_count = tally(EngagementStanding::Expired);
            let indeterminate_count =
                tally(EngagementStanding::Unknown) + tally(EngagementStanding::Unavailable);
            let programs: Vec<String> = engagements
                .iter()
                .map(|row| row.program_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();

            // Live rows only. `None` when nothing is live: an empty list would
            // read as "authorised, and the ceiling admits nothing", which is a
            // different — and far more comfortable — claim than "nothing here
            // authorises anything".
            let mut tool_union: BTreeSet<String> = BTreeSet::new();
            let mut team_union: BTreeSet<String> = BTreeSet::new();
            for row in &engagements {
                if let Some(tools) = row.live_tool_ceiling.as_ref() {
                    tool_union.extend(tools.iter().cloned());
                }
                if let Some(team) = row.live_team.as_ref() {
                    team_union.extend(team.iter().cloned());
                }
            }

            CounterpartySurface {
                counterparty,
                engagement_count: engagements.len(),
                live_count,
                revoked_count,
                expired_count,
                indeterminate_count,
                programs,
                live_tool_ceiling_union: (live_count > 0).then(|| tool_union.into_iter().collect()),
                live_team_union: (live_count > 0).then(|| team_union.into_iter().collect()),
                engagements,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Store-taking cores — where every decision lives
// ---------------------------------------------------------------------------

/// Every engagement in one scope, each asked live.
///
/// Ordered newest-created first, ties broken by id, so two reads of an unchanged
/// roster are byte-identical and a diff between them means something.
pub async fn engagement_rows_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    now_ms: i64,
) -> Vec<EngagementRow> {
    let records = store.list(principal, workspace).await;
    let mut rows = Vec::with_capacity(records.len());
    for record in &records {
        let live = store
            .live_authority(principal, workspace, &record.engagement_id, now_ms)
            .await;
        rows.push(engagement_row(record, &live));
    }
    rows.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.engagement_id.cmp(&right.engagement_id))
    });
    rows
}

/// One engagement in one scope, or `None` when the caller does not own it.
///
/// Built from the scope-filtered listing rather than from `live_authority`
/// alone, so an id belonging to another tenant is indistinguishable from an id
/// that does not exist — a 404 either way, telling the caller nothing about a
/// roster that is not theirs.
pub async fn engagement_row_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    engagement_id: &str,
    now_ms: i64,
) -> Option<EngagementRow> {
    let record = store
        .list(principal, workspace)
        .await
        .into_iter()
        .find(|record| record.engagement_id == engagement_id)?;
    let live = store
        .live_authority(principal, workspace, engagement_id, now_ms)
        .await;
    Some(engagement_row(&record, &live))
}

/// Why a revocation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeRefusal {
    /// No engagement with this id belongs to the caller's scope.
    NotInScope,
    /// The roster could not be written.
    Failed(String),
}

/// What a revocation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeOutcome {
    /// True when this call found the engagement already revoked. Revocation is
    /// forward-only, so a replay reports the same terminal state rather than
    /// moving the instant or failing.
    pub was_already_revoked: bool,
    pub row: EngagementRow,
}

/// Withdraw an engagement.
///
/// The scope check is not decoration. `EngagementStore::revoke` matches on
/// `engagement_id` across the entire roster with no scope filter, so without
/// this read one tenant could revoke another's authority by guessing an id.
/// The check runs first and the store is only touched once the id is known to
/// belong to the caller.
pub async fn revoke_engagement_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    engagement_id: &str,
    now_ms: i64,
) -> Result<RevokeOutcome, RevokeRefusal> {
    let Some(before) =
        engagement_row_with_store(store, principal, workspace, engagement_id, now_ms).await
    else {
        return Err(RevokeRefusal::NotInScope);
    };
    let was_already_revoked = before.revoked_at_ms.is_some();

    store
        .revoke(engagement_id, now_ms)
        .await
        .map_err(|error| RevokeRefusal::Failed(error.to_string()))?;

    let Some(row) =
        engagement_row_with_store(store, principal, workspace, engagement_id, now_ms).await
    else {
        return Err(RevokeRefusal::Failed(
            "the engagement vanished from the roster immediately after being revoked".to_string(),
        ));
    };
    Ok(RevokeOutcome {
        was_already_revoked,
        row,
    })
}

// ---------------------------------------------------------------------------
// What may be named in a ceiling
// ---------------------------------------------------------------------------

/// Why one name in a requested ceiling was refused.
///
/// Named rather than counted, and split rather than folded into one bucket: the
/// owner has to fix it, and *"that tool does not exist"* and *"that action is
/// bounded by who you may delegate to, not by what may be done"* need opposite
/// corrections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingRefusalReason {
    /// Nothing in this process dispatches under this name, so the entry would
    /// never be compared against anything.
    NoSuchCapability,
    /// A real action name, but one whose bound is the delegation team rather
    /// than the capability ceiling. Putting it here bounds nothing.
    BoundedByTeamNotByCeiling,
}

/// One refused ceiling entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeilingRefusal {
    pub capability: String,
    pub reason: CeilingRefusalReason,
    /// A dispatchable name differing from this one only in case.
    ///
    /// Not a fuzzy suggestion — an exact match modulo case, which is the single
    /// mistake the exactness of the dispatch check punishes hardest. Offered as
    /// information; it is never substituted, because a surface that quietly
    /// corrected a grant would be granting something the owner did not write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<String>,
}

/// Every name a dispatch in this process could compare a ceiling entry against.
///
/// Three sources, and the union is deliberately no wider:
///
/// - every registered provider, by its exact registered name. `CapabilityRegistry::has`
///   is **not** used, because it answers `true` for any `http_*` name whenever an
///   `http` provider exists — a widening that would admit a name nothing
///   dispatches under;
/// - every pack definition, plus the `<pack>__<action>` catalog leaves the pack
///   actually declares, which is exactly what `resolve_tool_definition` will
///   resolve later;
/// - the non-pack policy names the executor emits for built-in lanes and
///   sub-goals, spelled as it spells them.
///
/// The delegation names are **not** in here. They are real, and they are
/// answered separately, so a caller naming one is told which bound it belongs to
/// instead of being told it does not exist.
pub fn dispatchable_capability_names(registry: &CapabilityRegistry) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = registry.tool_names().into_iter().collect();
    for definition in registry.all_pack_definitions() {
        for action in definition.native_action_schemas.keys() {
            names.insert(format!("{}__{action}", definition.name));
        }
        names.insert(definition.name);
    }
    names.extend(
        CEILING_NAMEABLE_NON_PACK_POLICY_NAMES
            .iter()
            .map(|name| (*name).to_string()),
    );
    names
}

/// Check a requested ceiling against what can actually be dispatched.
///
/// Pure — it takes the names rather than the registry — so the whole rule is
/// decidable without a runtime, and so a caller can prove what it refuses.
///
/// # Why an unknown name is refused rather than dropped or kept
///
/// A ceiling entry naming a capability that does not exist **grants nothing
/// while looking like a grant**: the dispatch check tests set membership against
/// the exact policy name, so the entry is never compared to anything, the act is
/// denied as outside the ceiling, and the owner reading the roster sees the tool
/// listed and concludes it was permitted. Dropping the entry silently is the
/// same failure with the evidence removed. Refusing, and naming which entries,
/// is the only answer an owner can act on.
///
/// Matching is exact, including case, because that is how the dispatch check
/// matches. A near miss is reported as `did_you_mean` and never substituted.
pub fn refuse_unenforceable_ceiling(
    dispatchable: &BTreeSet<String>,
    ceiling: &BTreeSet<String>,
) -> Vec<CeilingRefusal> {
    ceiling
        .iter()
        .filter(|name| !dispatchable.contains(*name))
        .map(|name| CeilingRefusal {
            capability: name.clone(),
            reason: if is_delegation_dispatch_policy_name(name) {
                CeilingRefusalReason::BoundedByTeamNotByCeiling
            } else {
                CeilingRefusalReason::NoSuchCapability
            },
            did_you_mean: dispatchable
                .iter()
                .find(|known| known.eq_ignore_ascii_case(name))
                .cloned(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Minting — the write path
// ---------------------------------------------------------------------------

/// A request to bound a piece of work, stated the way the rest of the system
/// states work.
///
/// [`WorkContext`] rather than a program id and a list of tool names. An
/// engagement is one shape a bounded piece of work takes; the work context is
/// the shape every flow already holds, and
/// [`EngagementStore::grant_for_work`] lowers one onto the other. That is what
/// keeps this route from becoming the only way to express *"this execution is
/// doing bounded work for someone"* — a support triage or a vendor review binds
/// through the same call, naming its own program, with nothing in the executor
/// to edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngagementGrant {
    /// The work, and what it needs. The needs become the ceiling.
    pub work: WorkContext,
    /// Owner-readable counterparty label. Resolved elsewhere by exact match
    /// against the counterparty register, so it is recorded exactly as written.
    pub counterparty: String,
    /// Agent ids a scoped execution may delegate to. Empty is a real answer —
    /// no delegation — and is what an absent field means.
    pub team: BTreeSet<String>,
    /// **Mandatory.** There is no default and no fallback; see
    /// [`check_grantable_window`].
    pub expires_at_ms: i64,
}

/// Whether a window may be granted at all, before the store is asked.
///
/// **An engagement with no expiry is not creatable.** The type carries an
/// `i64`, so "no expiry" arrives either as an absent field — a deserialisation
/// error, refused before this runs — or as an instant that has already passed,
/// refused here. Neither is defaulted: standing authority that never lapses is
/// the thing the whole engagement carrier exists to bound, and an owner who was
/// given a window they never chose would believe they had chosen it.
///
/// Expiry is inclusive (`now_ms >= expires_at_ms` is expired), so an engagement
/// granted at its own expiry authorises nothing and would sit in the roster
/// reading as a grant from birth.
pub fn check_grantable_window(expires_at_ms: i64, now_ms: i64) -> Result<(), String> {
    if expires_at_ms <= now_ms {
        return Err(format!(
            "`expires_at_ms` ({expires_at_ms}) is already reached at {now_ms}: expiry is \
             inclusive, so this engagement would authorise nothing from the moment it was \
             created. State a window rather than leaving one to be chosen for you"
        ));
    }
    Ok(())
}

/// What a second create of the same work, for the same counterparty, means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateReplay {
    /// Nothing live matches; this is a new engagement.
    Fresh,
    /// An identical live engagement already exists — resume it rather than
    /// minting a second authority that says the same thing.
    Resumes { engagement_id: String },
    /// A live engagement bounds the same work for the same counterparty on
    /// different terms.
    Conflicts {
        engagement_id: String,
        differences: Vec<String>,
    },
}

/// Decide a create against what is already live in the scope.
///
/// The store mints a fresh ULID on every call, so the carrier's own
/// replay-resumes property is unreachable from HTTP: a retried POST would mint a
/// second authority for the same work, and the roster would hold two engagements
/// an owner has to revoke separately. Sameness is therefore decided on the
/// convention the carrier already states — **one engagement per (program,
/// counterparty)** — and then the terms are compared:
///
/// - identical terms **resume** the live engagement, so a retry is free and the
///   id everything is bound to does not move;
/// - changed terms are a **conflict**, never a silent resume. Returning the old
///   engagement for a new ceiling would tell the owner they had widened
///   authority they had not;
/// - only engagements that authorise **now** are considered. A revoked or
///   expired one is terminal, and creating past it mints a new engagement rather
///   than resurrecting the dead one.
pub fn classify_create_replay(
    rows: &[EngagementRow],
    program_id: &str,
    counterparty: &str,
    tool_ceiling: &BTreeSet<String>,
    team: &BTreeSet<String>,
    expires_at_ms: i64,
) -> CreateReplay {
    for row in rows {
        if !row.standing.authorises_anything() {
            continue;
        }
        if row.program_id != program_id || row.counterparty != counterparty {
            continue;
        }
        let mut differences = Vec::new();
        if row.recorded_tool_ceiling != tool_ceiling.iter().cloned().collect::<Vec<_>>() {
            differences.push("tool_ceiling".to_string());
        }
        if row.recorded_team != team.iter().cloned().collect::<Vec<_>>() {
            differences.push("team".to_string());
        }
        if row.expires_at_ms != expires_at_ms {
            differences.push("expires_at_ms".to_string());
        }
        return if differences.is_empty() {
            CreateReplay::Resumes {
                engagement_id: row.engagement_id.clone(),
            }
        } else {
            CreateReplay::Conflicts {
                engagement_id: row.engagement_id.clone(),
                differences,
            }
        };
    }
    CreateReplay::Fresh
}

/// Why an engagement was not created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRefusal {
    /// The request cannot be granted as written.
    NotGrantable(String),
    /// A live engagement already bounds this work on other terms.
    Conflicts {
        engagement_id: String,
        differences: Vec<String>,
    },
    /// The roster could not be written.
    Failed(String),
}

/// What a create did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    /// True when an identical live engagement already existed and was returned
    /// instead of a second one being minted.
    pub resumed: bool,
    pub row: EngagementRow,
}

/// Mint an engagement for a piece of work — **the write path**.
///
/// The production caller of [`EngagementStore::create`], reached through
/// [`EngagementStore::grant_for_work`] so the request stays work-shaped rather
/// than engagement-shaped all the way down.
///
/// Ordering is load-bearing, and it is the same ordering the envelope grant
/// uses: the replay read runs **before** the write, so a retried request resumes
/// rather than minting a second authority, and the window check runs before
/// either, so an ungrantable request never reaches the roster at all.
///
/// The ceiling is *not* checked against the capability registry here — that
/// check needs the registry, which is a process fixture rather than a store, and
/// it runs in the handler before this is called. What arrives here has already
/// been proved dispatchable.
pub async fn create_engagement_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    grant: &EngagementGrant,
    now_ms: i64,
) -> Result<CreateOutcome, CreateRefusal> {
    check_grantable_window(grant.expires_at_ms, now_ms).map_err(CreateRefusal::NotGrantable)?;
    let program_id = program_id_for_work(&grant.work.kind)
        .map_err(|error| CreateRefusal::NotGrantable(error.to_string()))?
        .to_string();

    // The ceiling as the store will record it, so the replay comparison is
    // against the same set the write would produce rather than against the raw
    // request.
    let tool_ceiling: BTreeSet<String> = grant
        .work
        .needs_capabilities
        .iter()
        .chain(grant.work.needs_playbooks.iter())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();

    let rows = engagement_rows_with_store(store, principal, workspace, now_ms).await;
    match classify_create_replay(
        &rows,
        &program_id,
        &grant.counterparty,
        &tool_ceiling,
        &grant.team,
        grant.expires_at_ms,
    ) {
        CreateReplay::Conflicts {
            engagement_id,
            differences,
        } => Err(CreateRefusal::Conflicts {
            engagement_id,
            differences,
        }),
        CreateReplay::Resumes { engagement_id } => {
            let Some(row) =
                engagement_row_with_store(store, principal, workspace, &engagement_id, now_ms)
                    .await
            else {
                return Err(CreateRefusal::Failed(format!(
                    "engagement `{engagement_id}` was live a moment ago and is no longer in this \
                     scope"
                )));
            };
            Ok(CreateOutcome { resumed: true, row })
        },
        CreateReplay::Fresh => {
            let minted = store
                .grant_for_work(
                    principal,
                    workspace,
                    &grant.work,
                    &grant.counterparty,
                    grant.team.clone(),
                    now_ms,
                    grant.expires_at_ms,
                )
                .await
                .map_err(|error| match error.kind() {
                    // The store's own refusals are things the caller can fix.
                    std::io::ErrorKind::InvalidInput => {
                        CreateRefusal::NotGrantable(error.to_string())
                    },
                    _ => CreateRefusal::Failed(error.to_string()),
                })?;
            // Read back through the same projection every other route answers
            // with, rather than serialising the record the write returned.
            let Some(row) = engagement_row_with_store(
                store,
                principal,
                workspace,
                &minted.engagement_id,
                now_ms,
            )
            .await
            else {
                return Err(CreateRefusal::Failed(format!(
                    "engagement `{}` was written and is not readable in the scope it was written \
                     to",
                    minted.engagement_id
                )));
            };
            Ok(CreateOutcome {
                resumed: false,
                row,
            })
        },
    }
}

// ---------------------------------------------------------------------------
// Renewal
// ---------------------------------------------------------------------------

/// Why a renewal did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenewRefusal {
    /// No engagement with this id belongs to the caller's scope.
    NotInScope,
    /// The store refused the new window.
    NotRenewable(String),
    /// The roster could not be written.
    Failed(String),
}

/// What a renewal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewOutcome {
    /// False when the engagement already expired at exactly this instant — an
    /// identical replay, which writes nothing and moves no revision.
    pub renewed: bool,
    pub row: EngagementRow,
}

/// Move a live engagement's window forward, keeping its id.
///
/// Recreating instead would mint a new ULID, and every execution, pause blob,
/// envelope and disclosure already carrying the old id would go on naming a
/// lapsed authority with nothing to repoint them.
///
/// The scope check is not decoration, for the same reason it is not on
/// [`revoke_engagement_with_store`]: `EngagementStore::extend_expiry` finds by id
/// across the whole roster with no scope filter, so without this read one tenant
/// could extend another's authority by guessing an id — and extending is the one
/// mutation that *adds* authority rather than removing it.
pub async fn renew_engagement_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    engagement_id: &str,
    expires_at_ms: i64,
    now_ms: i64,
) -> Result<RenewOutcome, RenewRefusal> {
    if engagement_row_with_store(store, principal, workspace, engagement_id, now_ms)
        .await
        .is_none()
    {
        return Err(RenewRefusal::NotInScope);
    }

    let renewed = store
        .extend_expiry(engagement_id, expires_at_ms, now_ms)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotFound => {
                RenewRefusal::NotRenewable(error.to_string())
            },
            _ => RenewRefusal::Failed(error.to_string()),
        })?;

    let Some(row) =
        engagement_row_with_store(store, principal, workspace, engagement_id, now_ms).await
    else {
        return Err(RenewRefusal::Failed(
            "the engagement vanished from the roster immediately after being renewed".to_string(),
        ));
    };
    Ok(RenewOutcome { renewed, row })
}

// ---------------------------------------------------------------------------
// Handlers — thin
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct EngagementScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EngagementSurfaceQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Exact counterparty label. Omitted, the read covers every counterparty in
    /// scope.
    #[serde(default)]
    pub counterparty: Option<String>,
    /// Exact program id. Narrows which engagements are counted, so the unions
    /// then describe that program's slice rather than the counterparty's whole
    /// surface.
    #[serde(default)]
    pub program_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EngagementAudienceQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Which kind of relationship the caller is reading this organisation as —
    /// any `AudienceKind` label. Omitted, it is `engagement`; an unrecognised
    /// value is **refused**, never defaulted, because the kind is half the
    /// audience key.
    #[serde(default)]
    pub audience_kind: Option<String>,
}

/// The create body.
///
/// `work_context`, `counterparty`, `tool_ceiling` and `expires_at_ms` carry no
/// serde default: an absent one is a deserialisation error, which is the refusal
/// this route is for. The caller states who the counterparty is, what the
/// ceiling is and when it lapses, or nothing is created.
///
/// `expires_at_ms` in particular is **never** defaulted. An engagement with no
/// expiry is standing authority that never lapses, which is the thing the whole
/// carrier exists to bound, and an owner handed a window they never chose would
/// believe they had chosen it.
///
/// `work_context` is the generic carrier — `{"kind": "program", "id": "..."}` —
/// so a support triage, a recruiting loop or a vendor review names its own
/// program and binds through this same route. There is deliberately no
/// `program_id` field: one vocabulary, and the one the rest of the system
/// already speaks.
///
/// There is no `granted_by`. The grantor is the authenticated principal; a
/// self-declared one is a signature anybody can write.
#[derive(Debug, Deserialize)]
pub struct CreateEngagementRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub work_context: WorkContextKind,
    pub counterparty: String,
    /// Tools an execution bound to this engagement may use. Every name is
    /// checked against what this process can actually dispatch before anything
    /// is written; an explicitly empty list is a real answer — an engagement
    /// that authorises no tools — while an absent one is refused.
    pub tool_ceiling: BTreeSet<String>,
    /// Agent ids a bound execution may delegate to. Absent means **none**: the
    /// delegation check is a membership test, and an empty set refuses every
    /// target rather than passing vacuously.
    #[serde(default)]
    pub team: BTreeSet<String>,
    /// Procedure skills the work needs, folded into the ceiling beside the tools
    /// because capability resolution treats them as one namespace.
    #[serde(default)]
    pub playbooks: BTreeSet<String>,
    pub expires_at_ms: i64,
}

/// The renewal body. `expires_at_ms` is required for the same reason it is on
/// the create: a renewal to an unstated window is a window nobody chose.
#[derive(Debug, Deserialize)]
pub struct RenewEngagementRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub expires_at_ms: i64,
}

/// The store, or a refusal.
///
/// **Never an empty roster.** An owner told "no engagements" by a process that
/// simply has no store installed would read it as "nothing may act on my
/// behalf", which is the most reassuring wrong answer available. The carrier
/// makes the same call at the dispatch boundary: no store means denied, not
/// unrestricted.
fn resolve_store() -> Result<std::sync::Arc<EngagementStore>, HttpResponse> {
    global_engagement_store().ok_or_else(|| {
        api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "engagement_store_unavailable",
            "no engagement store is installed in this process, so what is authorised cannot be \
             read — and unreadable is not `nothing`",
            None,
        )
    })
}

fn bad_request(error: impl Into<String>) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, None)
}

fn not_found(engagement_id: &str) -> HttpResponse {
    api_error_response(
        StatusCode::NOT_FOUND,
        "engagement_not_found",
        format!("no engagement `{engagement_id}` in this scope"),
        None,
    )
}

/// `GET /api/magician/v2/engagements`
pub async fn list_engagements_handler(
    req: HttpRequest,
    query: web::Query<EngagementScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let now_ms = Utc::now().timestamp_millis();
    let rows = engagement_rows_with_store(&store, &principal, &workspace, now_ms).await;
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": now_ms,
        "counts": standing_counts(&rows),
        "engagements": rows,
    }))
}

/// `GET /api/magician/v2/engagements/surface?counterparty=&program_id=`
///
/// The §4.2b cross-engagement read. Registered before the `{engagement_id}` read
/// so the literal wins the match.
pub async fn engagement_surface_handler(
    req: HttpRequest,
    query: web::Query<EngagementSurfaceQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let counterparty = query
        .counterparty
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let program_id = query
        .program_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let now_ms = Utc::now().timestamp_millis();
    let rows = engagement_rows_with_store(&store, &principal, &workspace, now_ms).await;
    let surfaces = counterparty_surfaces(rows, counterparty, program_id);
    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": now_ms,
        "filters": { "counterparty": counterparty, "program_id": program_id },
        "counterparty_count": surfaces.len(),
        "counterparties": surfaces,
    }))
}

/// `GET /api/magician/v2/engagements/{engagement_id}`
pub async fn get_engagement_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EngagementScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let engagement_id = path.into_inner();
    if let Err(message) = guard_path_id("an engagement id", &engagement_id) {
        return bad_request(message);
    }
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let now_ms = Utc::now().timestamp_millis();
    match engagement_row_with_store(&store, &principal, &workspace, &engagement_id, now_ms).await {
        Some(row) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of_ms": now_ms,
            "engagement": row,
        })),
        None => not_found(&engagement_id),
    }
}

/// `POST /api/magician/v2/engagements/{engagement_id}/revoke`
///
/// Effective on the next consequential dispatch of any execution carrying this
/// engagement, including one already running: the dispatch check re-reads the
/// store every time rather than trusting the revision an execution carries.
pub async fn revoke_engagement_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EngagementScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let engagement_id = path.into_inner();
    if let Err(message) = guard_path_id("an engagement id", &engagement_id) {
        return bad_request(message);
    }
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let now_ms = Utc::now().timestamp_millis();
    match revoke_engagement_with_store(&store, &principal, &workspace, &engagement_id, now_ms).await
    {
        Ok(outcome) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of_ms": now_ms,
            "was_already_revoked": outcome.was_already_revoked,
            "engagement": outcome.row,
        })),
        Err(RevokeRefusal::NotInScope) => not_found(&engagement_id),
        Err(RevokeRefusal::Failed(detail)) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "engagement_revocation_failed",
            "the engagement roster could not be written, so the engagement may still authorise \
             acts — treat it as live until this succeeds",
            Some(json!({ "detail": detail })),
        ),
    }
}

/// `GET /api/magician/v2/engagements/{engagement_id}/audience?audience_kind=`
///
/// **Which identities may this engagement's envelope cover?**
///
/// The engagement record carries an owner-typed counterparty *label*; the
/// counterparty register holds organisations and the addresses that are them.
/// This route joins the two through
/// `magician::magician_v2::counterparties::audience_for_label` — the resolution
/// lives in the register's module, not here, so the dependency points from the
/// OPC-specific side at the generic one and never back.
///
/// # Three fail-closed answers, kept apart
///
/// - **No register installed** — 503. Unreadable is not "nobody"; answering
///   with an empty audience would tell an owner their counterparty is
///   unreachable when the process simply cannot read the book.
/// - **The label matches no counterparty** — 200 with an empty audience and a
///   `lead`. That is not an error: an engagement may legitimately name an
///   organisation nobody has filed yet, and the fail-closed reading of it is an
///   empty identity set, never a permissive one.
/// - **The engagement authorises nothing** — 200 with `covered_identities:
///   null`. Withheld rather than emptied, so a revoked engagement cannot render
///   as a live one that happens to reach nobody.
///
/// # The kind is the caller's
///
/// `audience_kind` accepts any [`AudienceKind`] and defaults to `engagement`.
/// An unrecognised value is **refused**, never defaulted: a support flow, a
/// vendor file and a review panel reading the same organisation must not have
/// their rosters folded onto one key by a typo. Changing the kind changes the
/// key, never who is admitted.
pub async fn engagement_audience_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EngagementAudienceQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let engagement_id = path.into_inner();
    if let Err(message) = guard_path_id("an engagement id", &engagement_id) {
        return bad_request(message);
    }
    let requested_kind = query
        .audience_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("engagement");
    let Some(audience_kind) = AudienceKind::parse(requested_kind) else {
        return bad_request(format!(
            "`{requested_kind}` is not an audience kind. A near miss is refused rather than \
             defaulted: filing one relationship's roster under another's key is how two \
             different relationships with one organisation come to share an audience"
        ));
    };
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let now_ms = Utc::now().timestamp_millis();
    // `list` is the scope-filtered read — the whole security property of this
    // surface. An engagement in another tenant's roster is a 404 here rather
    // than a label this scope gets to resolve.
    let Some(record) = store
        .list(&principal, &workspace)
        .await
        .into_iter()
        .find(|record| record.engagement_id == engagement_id)
    else {
        return not_found(&engagement_id);
    };
    let live = store
        .live_authority(&principal, &workspace, &engagement_id, now_ms)
        .await;

    let scope = CounterpartyScope::new(principal.as_str(), workspace.as_str());
    // The same derivation the register uses, so an engagement whose stored
    // label could never be filed is named as that rather than as an unreadable
    // register. Nothing here validated the label when the engagement was
    // minted, so this state is reachable.
    if let Err(error) = counterparty_id_for(&scope, &record.counterparty) {
        return api_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "engagement_counterparty_label_unusable",
            "this engagement's counterparty label cannot name an organisation, so who it may \
             reach is unanswerable — which is not the same as nobody",
            Some(json!({ "counterparty": record.counterparty, "detail": error.to_string() })),
        );
    }

    let Some(register) = global_counterparty_store() else {
        return api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "counterparty_register_unavailable",
            "no counterparty register is installed in this process, so who this engagement may \
             reach cannot be read — and unreadable is never `nobody`",
            None,
        );
    };
    let label_standing = match resolve_label(&register, &scope, &record.counterparty) {
        Ok(standing) => standing,
        Err(error) => return register_unreadable(&error),
    };
    let audience = match audience_for_label(&register, &scope, &record.counterparty, audience_kind)
    {
        Ok(audience) => audience,
        Err(error) => return register_unreadable(&error),
    };

    HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "as_of_ms": now_ms,
        "audience": engagement_audience_row(&record, &live, &label_standing, &audience),
    }))
}

/// The register answered with an error. Never folded into an empty audience.
fn register_unreadable(error: &anyhow::Error) -> HttpResponse {
    api_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "counterparty_register_unreadable",
        "the counterparty register could not be read, so who this engagement may reach is \
         unknown — and unknown is never `nobody`",
        Some(json!({ "detail": error.to_string() })),
    )
}

/// The capability registry, or a refusal.
///
/// **Never an unchecked grant.** A process that cannot say which capabilities
/// exist cannot tell a ceiling that grants something from one that grants
/// nothing, and minting anyway would write exactly the record this validation
/// exists to prevent — a ceiling of names no dispatch will ever match,
/// indistinguishable on the owner surface from one that works. Unreadable is not
/// permission, here as at the dispatch boundary.
fn resolve_registry(
    registry: Option<web::Data<CapabilityRegistry>>,
) -> Result<web::Data<CapabilityRegistry>, HttpResponse> {
    registry.ok_or_else(|| {
        api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "capability_registry_unavailable",
            "no capability registry is installed in this process, so a tool ceiling cannot be \
             checked against what can actually be dispatched — and an unchecked ceiling is how a \
             grant that grants nothing gets written",
            None,
        )
    })
}

/// `POST /api/magician/v2/engagements`
///
/// Bound a piece of work: who the counterparty is, what may be done, and when it
/// lapses. The write path the carrier shipped without — until this route,
/// `EngagementStore::create` had no caller outside its own tests, so no
/// execution could ever carry an authority and every downstream bound read as
/// "unbound".
///
/// Ordering, all of it load-bearing:
///
/// 1. scope, before anything else is considered;
/// 2. the caller strings that feed id derivations, at the only door they come
///    through — including the counterparty label, checked with the register's
///    own derivation so a label that could never be filed is refused at mint
///    rather than discovered by the audience read afterwards;
/// 3. the ceiling, against the capability registry — refused, with the offending
///    names, rather than written as a grant that grants nothing;
/// 4. the window, refused rather than defaulted;
/// 5. the replay read, then the write.
pub async fn create_engagement_handler(
    req: HttpRequest,
    body: web::Json<CreateEngagementRequest>,
    registry: Option<web::Data<CapabilityRegistry>>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    // Every caller string that ends up feeding a derived id, guarded at the only
    // door one comes through. The work id is read through
    // `WorkContextKind::id()` rather than by matching the arms, so a third kind
    // of work is guarded the day it is added rather than the day somebody
    // notices it was not.
    for (label, value) in [
        ("the principal", principal.as_str()),
        ("the workspace", workspace.as_str()),
        ("a work context id", body.work_context.id()),
        ("a counterparty label", body.counterparty.as_str()),
    ] {
        if let Err(message) = guard_derivation_component(label, value) {
            return bad_request(message);
        }
    }
    for agent_id in &body.team {
        if let Err(message) = guard_derivation_component("a team agent id", agent_id) {
            return bad_request(message);
        }
    }
    // The register's own derivation, not an equivalent of it. The audience read
    // documents that nothing validated the label at mint time and that an
    // unfilable label is therefore reachable; asking here with the same function
    // is what makes that state unreachable for anything this route writes.
    if let Err(error) = counterparty_id_for(
        &CounterpartyScope::new(&principal, &workspace),
        &body.counterparty,
    ) {
        return bad_request(format!(
            "this counterparty label cannot name an organisation, so the engagement would be \
             minted for somebody nothing could ever resolve: {error}"
        ));
    }

    let registry = match resolve_registry(registry) {
        Ok(registry) => registry,
        Err(response) => return response,
    };
    // Refused, not dropped. A blank entry is compared equal to nothing, so
    // silently discarding it would leave the owner believing they had listed one
    // more capability than the roster records.
    if body
        .tool_ceiling
        .iter()
        .chain(body.playbooks.iter())
        .any(|name| name.trim().is_empty())
    {
        return bad_request(
            "a ceiling entry must name something: a blank one matches no dispatch, so it would \
             sit on the roster looking like one more thing this engagement permits",
        );
    }
    let requested: BTreeSet<String> = body
        .tool_ceiling
        .iter()
        .chain(body.playbooks.iter())
        .map(|name| name.trim().to_string())
        .collect();
    let dispatchable = dispatchable_capability_names(registry.get_ref());
    let refusals = refuse_unenforceable_ceiling(&dispatchable, &requested);
    if !refusals.is_empty() {
        let named: Vec<&str> = refusals
            .iter()
            .map(|refusal| refusal.capability.as_str())
            .collect();
        return api_error_response(
            StatusCode::BAD_REQUEST,
            "engagement_ceiling_unenforceable",
            format!(
                "this ceiling names {} thing(s) no dispatch in this process will ever compare it \
                 against: {}. Nothing was created — an entry like that grants nothing while \
                 reading on the roster exactly like one that works",
                refusals.len(),
                named.join(", ")
            ),
            Some(json!({
                "refused": refusals,
                "dispatchable_count": dispatchable.len(),
            })),
        );
    }

    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let grant = EngagementGrant {
        work: WorkContext {
            kind: body.work_context.clone(),
            // Recorded exactly as checked. `WorkContext::needing` lower-cases
            // what it is given, and a lower-cased name for a mixed-case tool is
            // precisely the grant-that-grants-nothing the check above refused.
            needs_capabilities: body.tool_ceiling.iter().cloned().collect(),
            needs_playbooks: body.playbooks.iter().cloned().collect(),
        },
        counterparty: body.counterparty.clone(),
        team: body.team.clone(),
        expires_at_ms: body.expires_at_ms,
    };

    let now_ms = Utc::now().timestamp_millis();
    match create_engagement_with_store(&store, &principal, &workspace, &grant, now_ms).await {
        Ok(outcome) => {
            let payload = json!({
                "principal": principal,
                "workspace": workspace,
                "as_of_ms": now_ms,
                "resumed": outcome.resumed,
                "engagement": outcome.row,
            });
            if outcome.resumed {
                HttpResponse::Ok().json(payload)
            } else {
                HttpResponse::Created().json(payload)
            }
        },
        Err(CreateRefusal::NotGrantable(message)) => bad_request(message),
        Err(CreateRefusal::Conflicts {
            engagement_id,
            differences,
        }) => api_error_response(
            StatusCode::CONFLICT,
            "engagement_terms_changed",
            format!(
                "a live engagement already bounds this work for this counterparty on different \
                 terms; returning it would tell you that you had granted something you had not, \
                 and minting a second would leave two authorities to withdraw separately. Narrow \
                 or revoke `{engagement_id}` first"
            ),
            Some(json!({
                "engagement_id": engagement_id,
                "differs_on": differences,
            })),
        ),
        Err(CreateRefusal::Failed(detail)) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "engagement_creation_failed",
            "the engagement roster could not be written, so nothing was granted — treat this as \
             no authority until it succeeds",
            Some(json!({ "detail": detail })),
        ),
    }
}

/// `PATCH /api/magician/v2/engagements/{engagement_id}`
///
/// Renew: move the window forward on the **same** engagement.
///
/// Creating a replacement instead would mint a new id, and every execution,
/// pause blob, envelope and disclosure already bound to the old one would go on
/// naming an authority that had lapsed — nothing repoints them, because nothing
/// was ever copied anywhere.
///
/// Forward-only, and never onto a terminal state: a revoked engagement and one
/// the clock has already closed both refuse. Renewal keeps a live piece of work
/// alive; it does not bring one back.
pub async fn renew_engagement_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenewEngagementRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let engagement_id = path.into_inner();
    if let Err(message) = guard_path_id("an engagement id", &engagement_id) {
        return bad_request(message);
    }
    let store = match resolve_store() {
        Ok(store) => store,
        Err(response) => return response,
    };

    let now_ms = Utc::now().timestamp_millis();
    if let Err(message) = check_grantable_window(body.expires_at_ms, now_ms) {
        return bad_request(message);
    }
    match renew_engagement_with_store(
        &store,
        &principal,
        &workspace,
        &engagement_id,
        body.expires_at_ms,
        now_ms,
    )
    .await
    {
        Ok(outcome) => HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "as_of_ms": now_ms,
            "renewed": outcome.renewed,
            "engagement": outcome.row,
        })),
        Err(RenewRefusal::NotInScope) => not_found(&engagement_id),
        Err(RenewRefusal::NotRenewable(message)) => bad_request(message),
        Err(RenewRefusal::Failed(detail)) => api_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "engagement_renewal_failed",
            "the engagement roster could not be written, so the window did not move — treat the \
             engagement as expiring when it already said it would",
            Some(json!({ "detail": detail })),
        ),
    }
}

/// Route registration. Mounted under `web::scope("/api/magician/v2")`.
///
/// `/engagements/surface` is registered **before** `/engagements/{id}`: actix
/// matches routes in registration order, and a dynamic segment registered first
/// would swallow the literal and answer "no engagement `surface`" forever.
pub fn configure_engagement_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/engagements", web::get().to(list_engagements_handler))
        // The write path. A second `.route` on the same literal is how this
        // crate registers a second method: `ServiceConfig::route` moves the
        // method guard onto the resource, so a non-matching method falls
        // through to the next registration rather than answering 405 from the
        // first.
        .route("/engagements", web::post().to(create_engagement_handler))
        .route(
            "/engagements/surface",
            web::get().to(engagement_surface_handler),
        )
        .route(
            "/engagements/{engagement_id}/revoke",
            web::post().to(revoke_engagement_handler),
        )
        .route(
            "/engagements/{engagement_id}/audience",
            web::get().to(engagement_audience_handler),
        )
        .route(
            "/engagements/{engagement_id}",
            web::get().to(get_engagement_handler),
        )
        // Renewal, not re-creation: the id an execution already carries must
        // not move.
        .route(
            "/engagements/{engagement_id}",
            web::patch().to(renew_engagement_handler),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;
    use magician::magician_v2::engagements::{
        authorize_engagement_delegation_with_store, authorize_engagement_dispatch_with_store,
        EngagementAuthorityRef,
    };

    const CREATED_MS: i64 = 1_000;
    const EXPIRES_MS: i64 = 100_000;

    fn names(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    async fn store_with(
        dir: &tempfile::TempDir,
        specs: &[(&str, &str, &[&str])],
    ) -> (EngagementStore, Vec<EngagementAuthority>) {
        let store = EngagementStore::open(dir.path())
            .await
            .expect("open roster");
        let mut created = Vec::new();
        for (index, &(program, counterparty, tools)) in specs.iter().enumerate() {
            created.push(
                store
                    .create(
                        "alpha",
                        "prod",
                        program,
                        counterparty,
                        names(tools),
                        names(&["worker-agent"]),
                        CREATED_MS + index as i64,
                        EXPIRES_MS,
                    )
                    .await
                    .expect("create"),
            );
        }
        (store, created)
    }

    // -- derived standing ---------------------------------------------------

    /// A revoked engagement shows NO live ceiling — `None`, not an empty list
    /// and not the recorded set. Rendering the recorded ceiling would tell the
    /// owner a dead engagement still permits three tools; rendering `[]` would
    /// make "dead" indistinguishable from "live with nothing permitted".
    #[tokio::test]
    async fn a_revoked_engagement_reports_no_live_ceiling_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) =
            store_with(&dir, &[("outreach", "Acme", &["research", "email"])]).await;
        let id = created[0].engagement_id.clone();
        store.revoke(&id, 2_000).await.expect("revoke");

        let row = engagement_row_with_store(&store, "alpha", "prod", &id, 2_001)
            .await
            .expect("row");
        assert_eq!(row.standing, EngagementStanding::Revoked);
        assert!(!row.authorises_now);
        assert_eq!(row.live_tool_ceiling, None);
        assert_eq!(row.live_team, None);
        assert_eq!(row.live_tool_count, None);
        assert_eq!(row.denial, Some(AuthorityDenial::Revoked));
        assert_eq!(
            row.recorded_tool_ceiling,
            vec!["email".to_string(), "research".to_string()],
            "the record is still shown, labelled as the record"
        );
        assert_eq!(row.revoked_at_ms, Some(2_000));
        assert_eq!(row.authority_revision, 2, "revocation bumped the revision");
    }

    /// Expiry is INCLUSIVE and read from the clock, not from a field. At exactly
    /// `expires_at_ms` the engagement is already over; one millisecond earlier
    /// it still authorises. An off-by-one here is a millisecond of authority
    /// nobody granted.
    #[tokio::test]
    async fn expiry_is_inclusive_and_comes_from_the_clock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        let id = created[0].engagement_id.clone();

        let before = engagement_row_with_store(&store, "alpha", "prod", &id, EXPIRES_MS - 1)
            .await
            .expect("row");
        assert_eq!(before.standing, EngagementStanding::Live);
        assert_eq!(before.live_tool_ceiling, Some(vec!["research".to_string()]));
        assert_eq!(before.live_tool_count, Some(1));

        let at_the_instant = engagement_row_with_store(&store, "alpha", "prod", &id, EXPIRES_MS)
            .await
            .expect("row");
        assert_eq!(at_the_instant.standing, EngagementStanding::Expired);
        assert!(!at_the_instant.authorises_now);
        assert_eq!(at_the_instant.live_tool_ceiling, None);
        assert_eq!(
            at_the_instant.revoked_at_ms, None,
            "nothing was written; the clock alone ended it"
        );
    }

    /// Every non-grant answer is a non-authorising standing. Pins that no denial
    /// — including "the store could not answer" — can ever be projected as
    /// authority, which is §4.2c row 8 carried onto the read surface.
    #[test]
    fn no_denial_is_ever_projected_as_authority() {
        for denial in [
            AuthorityDenial::UnknownEngagement,
            AuthorityDenial::Revoked,
            AuthorityDenial::Expired,
            AuthorityDenial::ToolOutsideCeiling {
                tool: "raw_email".to_string(),
            },
            AuthorityDenial::TargetOutsideTeam {
                agent_id: "outsider".to_string(),
            },
            AuthorityDenial::StoreUnavailable {
                detail: "no store".to_string(),
            },
        ] {
            let standing = EngagementStanding::from_denial(&denial);
            assert!(
                !standing.authorises_anything(),
                "{denial:?} projected as authority via `{}`",
                standing.as_str()
            );
        }
        assert!(EngagementStanding::Live.authorises_anything());
        assert_eq!(
            EngagementStanding::from_denial(&AuthorityDenial::StoreUnavailable {
                detail: "gone".to_string()
            }),
            EngagementStanding::Unavailable,
            "an unavailable store is its own standing, never folded into `revoked`"
        );
    }

    // -- scope --------------------------------------------------------------

    /// Another tenant cannot read one engagement, and cannot revoke it either.
    /// `EngagementStore::revoke` finds by id across the WHOLE roster with no
    /// scope filter, so without the surface's own check a guessed id would
    /// withdraw someone else's authority.
    #[tokio::test]
    async fn another_principal_can_neither_read_nor_revoke_the_engagement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        let id = created[0].engagement_id.clone();

        assert_eq!(
            engagement_row_with_store(&store, "beta", "prod", &id, 2_000).await,
            None
        );
        assert_eq!(
            engagement_rows_with_store(&store, "beta", "prod", 2_000).await,
            Vec::new()
        );
        assert_eq!(
            revoke_engagement_with_store(&store, "beta", "prod", &id, 2_000).await,
            Err(RevokeRefusal::NotInScope)
        );

        // The owner's engagement is untouched by the attempt.
        let mine = engagement_row_with_store(&store, "alpha", "prod", &id, 2_000)
            .await
            .expect("row");
        assert_eq!(mine.standing, EngagementStanding::Live);
        assert_eq!(mine.revoked_at_ms, None);
        assert_eq!(mine.authority_revision, 1);
    }

    // -- revocation ---------------------------------------------------------

    /// Revoking through this surface denies the very next dispatch of an
    /// execution that is already running and still carrying the pre-revocation
    /// revision. This is the whole point of the route: if it took effect only on
    /// the next run, an owner watching something go wrong could not stop it.
    #[tokio::test]
    async fn revoking_through_the_surface_denies_the_next_dispatch_mid_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        let engagement = &created[0];
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };

        // The run is alive and dispatching fine.
        let grant = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 2_000,
        )
        .await
        .expect("in ceiling");
        assert!(!grant.revision_changed);

        let outcome =
            revoke_engagement_with_store(&store, "alpha", "prod", &engagement.engagement_id, 2_500)
                .await
                .expect("revoke");
        assert!(!outcome.was_already_revoked);
        assert_eq!(outcome.row.standing, EngagementStanding::Revoked);

        // The same execution, same carried revision, next dispatch: denied.
        let denial = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 2_600,
        )
        .await
        .expect_err("revocation lands mid-run");
        assert_eq!(denial, AuthorityDenial::Revoked);
    }

    /// Revocation is forward-only and terminal. A replay reports the same state
    /// rather than erroring, and — critically — does not move `revoked_at_ms` or
    /// bump the revision again, so the audit keeps saying when authority
    /// actually ended.
    #[tokio::test]
    async fn revoking_twice_is_a_replay_that_never_moves_the_instant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        let id = created[0].engagement_id.clone();

        let first = revoke_engagement_with_store(&store, "alpha", "prod", &id, 2_500)
            .await
            .expect("first revoke");
        assert!(!first.was_already_revoked);
        assert_eq!(first.row.revoked_at_ms, Some(2_500));
        assert_eq!(first.row.authority_revision, 2);

        let second = revoke_engagement_with_store(&store, "alpha", "prod", &id, 9_000)
            .await
            .expect("replay");
        assert!(second.was_already_revoked);
        assert_eq!(
            second.row.revoked_at_ms,
            Some(2_500),
            "a replay must not restamp when authority ended"
        );
        assert_eq!(
            second.row.authority_revision, 2,
            "a replay must not bump the revision and invalidate live snapshots for nothing"
        );
        assert_eq!(second.row.standing, EngagementStanding::Revoked);
    }

    /// An expired engagement stays expired when revoked — the terminal state
    /// does not become "live" by being written to, and the row still refuses to
    /// report a ceiling.
    #[tokio::test]
    async fn revoking_an_expired_engagement_does_not_resurrect_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        let id = created[0].engagement_id.clone();

        let outcome = revoke_engagement_with_store(&store, "alpha", "prod", &id, EXPIRES_MS + 1)
            .await
            .expect("revoke past expiry");
        assert_eq!(outcome.row.standing, EngagementStanding::Revoked);
        assert!(!outcome.row.authorises_now);
        assert_eq!(outcome.row.live_tool_ceiling, None);
    }

    // -- cross-engagement read ----------------------------------------------

    /// One counterparty's whole surface spans programs, and the live ceiling
    /// union covers LIVE engagements only. A union that folded in the revoked
    /// engagement's `email` would tell the owner they may still email a
    /// counterparty whose email authority they withdrew.
    #[tokio::test]
    async fn a_counterpartys_surface_unions_only_what_is_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(
            &dir,
            &[
                ("outreach", "Acme", &["research", "email"]),
                ("support", "Acme", &["research", "tickets"]),
                ("outreach", "Globex", &["research"]),
            ],
        )
        .await;
        store
            .revoke(&created[0].engagement_id, 2_000)
            .await
            .expect("revoke the outreach engagement");

        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_001).await;
        let surfaces = counterparty_surfaces(rows, Some("Acme"), None);

        assert_eq!(surfaces.len(), 1);
        let acme = &surfaces[0];
        assert_eq!(acme.counterparty, "Acme");
        assert_eq!(acme.engagement_count, 2);
        assert_eq!(acme.live_count, 1);
        assert_eq!(acme.revoked_count, 1);
        assert_eq!(acme.expired_count, 0);
        assert_eq!(acme.indeterminate_count, 0);
        assert_eq!(
            acme.programs,
            vec!["outreach".to_string(), "support".to_string()]
        );
        assert_eq!(
            acme.live_tool_ceiling_union,
            Some(vec!["research".to_string(), "tickets".to_string()]),
            "`email` came only from the revoked engagement and must not appear"
        );
        assert_eq!(acme.live_team_union, Some(vec!["worker-agent".to_string()]));
    }

    /// With nothing live, the union is `None` rather than `[]`. An empty list
    /// reads as "authorised, and the ceiling is empty"; absence is the honest
    /// "nothing authorises anything here right now".
    #[tokio::test]
    async fn a_counterparty_with_nothing_live_has_no_union_rather_than_an_empty_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(&dir, &[("outreach", "Acme", &["research"])]).await;
        store
            .revoke(&created[0].engagement_id, 2_000)
            .await
            .expect("revoke");

        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_001).await;
        let surfaces = counterparty_surfaces(rows, None, None);
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].live_count, 0);
        assert_eq!(surfaces[0].live_tool_ceiling_union, None);
        assert_eq!(surfaces[0].live_team_union, None);
    }

    /// The counterparty filter is an EXACT match. A near miss returns nothing
    /// rather than the nearest organisation: showing one party's authority under
    /// another's name is the failure the counterparty module refuses fuzzy
    /// resolution to avoid, and it looks like a correct answer.
    #[tokio::test]
    async fn the_counterparty_filter_never_guesses_at_a_near_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, _) = store_with(
            &dir,
            &[
                ("outreach", "Acme", &["research"]),
                ("outreach", "Acme Capital", &["email"]),
            ],
        )
        .await;
        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_000).await;

        assert_eq!(
            counterparty_surfaces(rows.clone(), Some("acme"), None),
            Vec::new(),
            "a case difference is a different label, not a close-enough one"
        );
        assert_eq!(
            counterparty_surfaces(rows.clone(), Some("Acm"), None).len(),
            0
        );

        let exact = counterparty_surfaces(rows, Some("Acme"), None);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].engagement_count, 1);
        assert_eq!(
            exact[0].live_tool_ceiling_union,
            Some(vec!["research".to_string()]),
            "`Acme Capital` is a different organisation and contributes nothing"
        );
    }

    /// The program filter narrows the counterparty's rollup to that program's
    /// engagements, and the counts and union follow the narrowing rather than
    /// describing the unfiltered surface.
    #[tokio::test]
    async fn the_program_filter_narrows_the_rollup_and_its_union() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, _) = store_with(
            &dir,
            &[
                ("outreach", "Acme", &["research", "email"]),
                ("support", "Acme", &["tickets"]),
            ],
        )
        .await;
        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_000).await;

        let whole = counterparty_surfaces(rows.clone(), Some("Acme"), None);
        assert_eq!(whole[0].engagement_count, 2);
        assert_eq!(
            whole[0].live_tool_ceiling_union,
            Some(vec![
                "email".to_string(),
                "research".to_string(),
                "tickets".to_string()
            ])
        );

        let support_only = counterparty_surfaces(rows, Some("Acme"), Some("support"));
        assert_eq!(support_only.len(), 1);
        assert_eq!(support_only[0].engagement_count, 1);
        assert_eq!(support_only[0].programs, vec!["support".to_string()]);
        assert_eq!(
            support_only[0].live_tool_ceiling_union,
            Some(vec!["tickets".to_string()])
        );
    }

    /// The listing tallies COUNTS by standing. A rate would be a number the
    /// owner cannot reconcile against the rows printed beneath it.
    #[tokio::test]
    async fn the_listing_reports_counts_by_standing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) = store_with(
            &dir,
            &[
                ("outreach", "Acme", &["research"]),
                ("support", "Globex", &["tickets"]),
            ],
        )
        .await;
        store
            .revoke(&created[0].engagement_id, 2_000)
            .await
            .expect("revoke");

        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_001).await;
        let counts = standing_counts(&rows);
        assert_eq!(counts["total"], 2);
        assert_eq!(counts["live"], 1);
        assert_eq!(counts["revoked"], 1);
        assert_eq!(counts["expired"], 0);
        assert_eq!(counts["unknown"], 0);
        assert_eq!(counts["unavailable"], 0);
    }

    // -- routes -------------------------------------------------------------

    /// With no store installed the surface answers 503, never an empty roster.
    /// "You have no engagements" from a process that cannot read the roster is
    /// the most reassuring wrong answer this route could give.
    #[actix_web::test]
    async fn an_uninstalled_store_refuses_rather_than_reporting_an_empty_roster() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        for uri in ["/engagements", "/engagements/surface"] {
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
                "{uri} must not answer an empty list when the roster cannot be read"
            );
        }
    }

    /// Scope is required before anything else is considered. A listing that
    /// defaulted the principal would hand one owner's engagements to whoever
    /// asked without one.
    #[actix_web::test]
    async fn a_read_without_scope_is_refused() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements")
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 400);
    }

    /// `/engagements/surface` reaches the surface read rather than being
    /// swallowed by `/engagements/{engagement_id}`. Pinned by the status: the
    /// dynamic route would have answered 404 for an engagement called
    /// "surface", while the literal reaches the store check and answers 503.
    #[actix_web::test]
    async fn the_surface_route_is_not_shadowed_by_the_id_route() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements/surface")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 503);
    }

    /// An id that could not have been minted here is refused at the door,
    /// before any store lookup, so a hostile path component never reaches the
    /// roster or a filename derived from one.
    #[actix_web::test]
    async fn a_hostile_engagement_id_is_refused_before_the_store_is_touched() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/engagements/an%20id%20with%20spaces/revoke")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "refused as malformed, not answered 503 after a store lookup"
        );
    }
    // -- who an engagement may reach ----------------------------------------

    use magician::magician_v2::audience::AudienceRef;
    use magician::magician_v2::counterparties::{CounterpartyLead, CounterpartyRef};

    fn registered(counterparty_id: &str) -> LabelStanding {
        LabelStanding::Registered(CounterpartyRef::new(counterparty_id))
    }

    fn roster(counterparty_id: &str, kind: AudienceKind, identities: &[&str]) -> Audience {
        Audience::new(
            AudienceRef::new(kind, counterparty_id),
            identities.iter().map(|value| value.to_string()).collect(),
        )
    }

    /// **A dead engagement withholds its audience rather than emptying it.**
    ///
    /// Pins the collapse that would make a revoked engagement render as a live
    /// one that happens to reach nobody: `Some([])` is a checkable answer
    /// ("authorised, and nobody is proved"), `None` is "nothing is authorised".
    /// A reader that saw `[]` for a revoked engagement would conclude the
    /// withdrawal had already taken effect on the recipients, which is a
    /// different claim from the one the carrier makes.
    #[tokio::test]
    async fn a_dead_engagement_withholds_its_audience_rather_than_emptying_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) =
            store_with(&dir, &[("outreach", "Acme Ltd", &["restricted_email"])]).await;
        let id = created[0].engagement_id.clone();
        store.revoke(&id, 2_000).await.expect("revoke");
        let live = store.live_authority("alpha", "prod", &id, 2_001).await;

        let audience = roster("cp-acme", AudienceKind::Engagement, &["ops@acme.com"]);
        let row = engagement_audience_row(&created[0], &live, &registered("cp-acme"), &audience);

        assert_eq!(row.standing, EngagementStanding::Revoked);
        assert!(!row.authorises_now);
        assert_eq!(row.covered_identities, None);
        assert_eq!(row.covered_identity_count, None);
        // The register's answer is still reported — a fact about the book, the
        // same way the recorded ceiling is shown beside a dead engagement.
        assert_eq!(row.counterparty, "Acme Ltd");
        assert_eq!(row.counterparty_standing, "registered");
        assert_eq!(row.counterparty_id, "cp-acme");
        assert_eq!(row.audience_key, "engagement:cp-acme");
        assert_eq!(row.lead, None);
    }

    /// **A live engagement reports exactly the addresses the register proved,
    /// under the key the caller asked for.**
    ///
    /// Pins two failures at once: an audience rendered from the label instead
    /// of from the resolved organisation (so the key would move the moment
    /// somebody renamed the counterparty), and a count that disagrees with the
    /// list beside it.
    #[tokio::test]
    async fn a_live_engagement_reports_the_proved_addresses_under_the_requested_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) =
            store_with(&dir, &[("outreach", "Acme Ltd", &["restricted_email"])]).await;
        let live = store
            .live_authority("alpha", "prod", &created[0].engagement_id, CREATED_MS + 1)
            .await;
        assert!(
            live.is_ok(),
            "the fixture must be live for this to prove anything"
        );

        let audience = roster(
            "cp-acme",
            AudienceKind::Account,
            &["billing@acme.com", "ops@acme.com"],
        );
        let row = engagement_audience_row(&created[0], &live, &registered("cp-acme"), &audience);

        assert_eq!(row.standing, EngagementStanding::Live);
        assert!(row.authorises_now);
        assert_eq!(
            row.covered_identities,
            Some(vec![
                "billing@acme.com".to_string(),
                "ops@acme.com".to_string()
            ])
        );
        assert_eq!(row.covered_identity_count, Some(2));
        assert_eq!(row.audience_kind, "account");
        assert_eq!(
            row.audience_key, "account:cp-acme",
            "the kind is half the key, so a second flow reading the same organisation does not \
             share this roster"
        );
        assert_eq!(row.program_id, "outreach");
        assert_eq!(row.lead, None);
    }

    /// **A label matching no counterparty is a lead with an EMPTY audience, and
    /// a 200.**
    ///
    /// Pins the two wrong answers. Refusing the read would make an engagement
    /// naming an organisation nobody has filed yet look broken, when it is
    /// ordinary. Answering with anything non-empty — the label itself, a
    /// domain-matched candidate, the other counterparties in scope — would let
    /// an unresolvable engagement cover somebody. `Some([])` says: this
    /// engagement authorises acts, and there is nobody it may reach.
    #[tokio::test]
    async fn an_unmatched_label_is_a_lead_with_an_empty_audience_rather_than_a_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) =
            store_with(&dir, &[("outreach", "Globex Inc", &["restricted_email"])]).await;
        let live = store
            .live_authority("alpha", "prod", &created[0].engagement_id, CREATED_MS + 1)
            .await;

        let standing = LabelStanding::Unregistered(CounterpartyLead {
            label: "Globex Inc".to_string(),
            would_be_counterparty_id: "cp-would-be".to_string(),
        });
        let audience = roster("cp-would-be", AudienceKind::Engagement, &[]);
        let row = engagement_audience_row(&created[0], &live, &standing, &audience);

        assert!(row.authorises_now);
        assert_eq!(row.counterparty_standing, "unregistered");
        assert_eq!(
            row.covered_identities,
            Some(Vec::<String>::new()),
            "live and reaching nobody is a checkable answer, not an absence"
        );
        assert_eq!(row.covered_identity_count, Some(0));
        assert_eq!(row.counterparty_id, "cp-would-be");
        assert_eq!(row.audience_key, "engagement:cp-would-be");
        let lead = row.lead.expect("a lead an owner can act on");
        assert_eq!(lead.label, "Globex Inc");
        assert_eq!(
            lead.would_be_counterparty_id, "cp-would-be",
            "the lead names the row recording it would create, so the follow-through lands on \
             the id this read was already answering under"
        );
    }

    /// **An unrecognised audience kind is refused at the door.**
    ///
    /// Pins the default that would fold a support account's roster, a panel's
    /// or a programme's onto `engagement:<id>`. The refusal happens before any
    /// store is consulted, which is why this answers 400 in a process with no
    /// store installed rather than 503.
    #[actix_web::test]
    async fn the_audience_route_refuses_an_unknown_kind_rather_than_defaulting_it() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        for kind in ["engagements", "cohort", "public", "eng"] {
            let response = actix_test::call_service(
                &app,
                actix_test::TestRequest::get()
                    .uri(&format!("/engagements/eng-1/audience?audience_kind={kind}"))
                    .insert_header(("X-Principal", "alpha"))
                    .insert_header(("X-Workspace", "prod"))
                    .to_request(),
            )
            .await;
            assert_eq!(
                response.status().as_u16(),
                400,
                "`{kind}` must be refused, not defaulted to engagement"
            );
        }
    }

    /// **With no engagement store installed the audience route refuses.**
    ///
    /// Pins the most reassuring wrong answer this route could give: "this
    /// engagement reaches nobody", from a process that cannot read the roster
    /// at all. Unreadable is never `nobody`. The 503 also proves the route is
    /// reachable and is not shadowed by `/engagements/{engagement_id}`, which
    /// cannot match a three-segment path.
    #[actix_web::test]
    async fn the_audience_route_refuses_rather_than_reporting_that_nobody_is_reachable() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements/eng-1/audience")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status().as_u16(), 503);

        // And without scope it is refused before anything else is considered.
        let unscoped = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements/eng-1/audience")
                .to_request(),
        )
        .await;
        assert_eq!(unscoped.status().as_u16(), 400);

        // A path component that could not have been minted here is refused
        // before any store or register is touched.
        let hostile = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements/an%20id%20with%20spaces/audience")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        assert_eq!(hostile.status().as_u16(), 400);
    }

    // -- the write path -----------------------------------------------------

    async fn empty_store(dir: &tempfile::TempDir) -> EngagementStore {
        EngagementStore::open(dir.path())
            .await
            .expect("open roster")
    }

    fn grant_for(
        program: &str,
        counterparty: &str,
        ceiling: &[&str],
        team: &[&str],
        expires_at_ms: i64,
    ) -> EngagementGrant {
        EngagementGrant {
            work: WorkContext {
                kind: WorkContextKind::Program(program.to_string()),
                needs_capabilities: ceiling.iter().map(|name| name.to_string()).collect(),
                needs_playbooks: Vec::new(),
            },
            counterparty: counterparty.to_string(),
            team: names(team),
            expires_at_ms,
        }
    }

    /// Creating an engagement produces authority a dispatch actually enforces:
    /// the named ceiling permits, everything else is denied, and the named team
    /// bounds delegation.
    ///
    /// This is the whole point of the write path. Pins a create that files a
    /// record the dispatch boundary does not honour — the engagement would
    /// appear on the roster, the owner would believe the work was bounded, and
    /// either every act would be denied or none would.
    #[tokio::test]
    async fn creating_an_engagement_produces_authority_the_dispatch_boundary_enforces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for(
            "support-triage",
            "Globex",
            &["research", "escalation_ladder"],
            &["support-agent"],
            EXPIRES_MS,
        );

        let outcome = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect("created");
        assert!(!outcome.resumed);
        assert_eq!(outcome.row.standing, EngagementStanding::Live);
        assert!(outcome.row.authorises_now);
        assert_eq!(outcome.row.program_id, "support-triage");
        assert_eq!(outcome.row.counterparty, "Globex");
        assert_eq!(outcome.row.expires_at_ms, EXPIRES_MS);
        assert_eq!(outcome.row.authority_revision, 1);
        assert_eq!(
            outcome.row.recorded_tool_ceiling,
            vec!["escalation_ladder".to_string(), "research".to_string()]
        );
        assert_eq!(outcome.row.live_tool_count, Some(2));
        assert_eq!(
            outcome.row.live_team,
            Some(vec!["support-agent".to_string()])
        );

        // The entry exists — now prove the boundary reads it.
        let carried = EngagementAuthorityRef {
            engagement_id: outcome.row.engagement_id.clone(),
            authority_revision: outcome.row.authority_revision,
        };
        assert!(authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 2_000
        )
        .await
        .is_ok());
        assert_eq!(
            authorize_engagement_dispatch_with_store(
                &store,
                &carried,
                "alpha",
                "prod",
                "raw_email",
                2_000
            )
            .await
            .expect_err("outside the ceiling"),
            AuthorityDenial::ToolOutsideCeiling {
                tool: "raw_email".to_string()
            }
        );
        assert!(authorize_engagement_delegation_with_store(
            &store,
            &carried,
            "alpha",
            "prod",
            "support-agent",
            2_000
        )
        .await
        .is_ok());
        assert_eq!(
            authorize_engagement_delegation_with_store(
                &store,
                &carried,
                "alpha",
                "prod",
                "outsider-agent",
                2_000
            )
            .await
            .expect_err("outside the team"),
            AuthorityDenial::TargetOutsideTeam {
                agent_id: "outsider-agent".to_string()
            }
        );
    }

    /// The same route mints authority for a second kind of work — a vendor
    /// review — with nothing about it named anywhere in this module.
    ///
    /// Pins the surface hardening into a fundraising feature. If the create path
    /// were engagement-specific, a second flow would need its own route or an
    /// edit to the executor, and the generic `program` arm of `WorkContextKind`
    /// would stay unreachable in production the way it was before this route
    /// existed.
    #[tokio::test]
    async fn a_second_kind_of_work_binds_through_the_same_route() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        for (program, counterparty) in [
            ("support-triage", "Globex"),
            ("recruiting", "Initech"),
            ("vendor-management", "Umbrella"),
        ] {
            let grant = grant_for(program, counterparty, &["research"], &[], EXPIRES_MS);
            let outcome = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
                .await
                .expect("created");
            assert_eq!(outcome.row.program_id, program);
            assert_eq!(outcome.row.standing, EngagementStanding::Live);
        }
        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_000).await;
        assert_eq!(rows.len(), 3);
        assert_eq!(standing_counts(&rows)["live"], 3);
    }

    /// A work context naming an engagement is refused, and nothing is written.
    ///
    /// Pins the quiet conflation: the record's only work field is `program_id`,
    /// so an engagement id written into it would be listed and filtered as a
    /// program on the owner surface and would merge with any program sharing the
    /// id.
    #[tokio::test]
    async fn a_work_context_naming_an_engagement_is_refused_at_create() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let mut grant = grant_for("ignored", "Globex", &["research"], &[], EXPIRES_MS);
        grant.work.kind = WorkContextKind::Engagement("eng-parent".to_string());

        let refusal = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect_err("refused");
        assert!(
            matches!(refusal, CreateRefusal::NotGrantable(_)),
            "{refusal:?}"
        );
        assert_eq!(
            engagement_rows_with_store(&store, "alpha", "prod", 2_000).await,
            Vec::new(),
            "a refused create must not have written"
        );
    }

    /// An engagement with no live window is not creatable, and the boundary is
    /// exact: `expires_at_ms == now_ms` is already over.
    ///
    /// Pins the standing authority this whole carrier exists to prevent, and its
    /// quieter twin — an engagement minted at or after its own expiry, which
    /// authorises nothing while sitting on the roster looking like a grant.
    /// Neither is defaulted into a window the owner never chose.
    #[tokio::test]
    async fn an_engagement_with_no_live_window_is_not_creatable() {
        assert!(check_grantable_window(1_001, 1_000).is_ok());
        for already_over in [1_000_i64, 999, 0, -1] {
            assert!(
                check_grantable_window(already_over, 1_000).is_err(),
                "expiry {already_over} at now 1000 is not a window"
            );
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for("support-triage", "Globex", &["research"], &[], CREATED_MS);
        let refusal = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect_err("refused");
        assert!(
            matches!(refusal, CreateRefusal::NotGrantable(_)),
            "{refusal:?}"
        );
        assert_eq!(
            engagement_rows_with_store(&store, "alpha", "prod", CREATED_MS).await,
            Vec::new()
        );
    }

    // -- the ceiling check --------------------------------------------------

    /// A ceiling naming a capability nothing dispatches is refused, and every
    /// offending name is reported.
    ///
    /// Pins the failure this system has already had once: such an entry is never
    /// compared against anything, so it grants nothing while reading on the
    /// roster exactly like a grant that works. Matching is exact including case,
    /// because the dispatch check matches that way — a case-only miss is
    /// reported as `did_you_mean` and never silently substituted, since
    /// correcting a grant is granting something the owner did not write.
    #[test]
    fn a_ceiling_naming_a_capability_that_does_not_exist_is_refused_by_name() {
        let dispatchable = names(&["research", "websearch", "files", "http"]);
        let refusals = refuse_unenforceable_ceiling(
            &dispatchable,
            &names(&["research", "webserch", "WebSearch"]),
        );
        assert_eq!(
            refusals,
            vec![
                CeilingRefusal {
                    capability: "WebSearch".to_string(),
                    reason: CeilingRefusalReason::NoSuchCapability,
                    did_you_mean: Some("websearch".to_string()),
                },
                CeilingRefusal {
                    capability: "webserch".to_string(),
                    reason: CeilingRefusalReason::NoSuchCapability,
                    did_you_mean: None,
                },
            ],
            "`research` is dispatchable and must not be refused; the other two must both be named"
        );
        assert!(
            refuse_unenforceable_ceiling(&dispatchable, &names(&["research", "files"])).is_empty()
        );
        assert!(
            refuse_unenforceable_ceiling(&dispatchable, &BTreeSet::new()).is_empty(),
            "an explicitly empty ceiling is a real answer: an engagement that authorises no tools"
        );
    }

    /// A delegation name in a capability ceiling is refused as the wrong bound,
    /// not as a typo.
    ///
    /// Pins two failures at once. Accepting it would record an entry the
    /// delegation check never consults, so it bounds nothing while reading as a
    /// grant. Reporting it as an unknown capability would send the owner to fix
    /// a spelling that was right, when what they need is `team`.
    #[test]
    fn a_delegation_name_in_a_ceiling_is_refused_as_the_wrong_bound() {
        let refusals = refuse_unenforceable_ceiling(
            &names(&["research"]),
            &names(&["delegate_to_agent", "handover_to_agent"]),
        );
        assert_eq!(
            refusals
                .iter()
                .map(|refusal| (refusal.capability.as_str(), refusal.reason))
                .collect::<Vec<_>>(),
            vec![
                (
                    "delegate_to_agent",
                    CeilingRefusalReason::BoundedByTeamNotByCeiling
                ),
                (
                    "handover_to_agent",
                    CeilingRefusalReason::BoundedByTeamNotByCeiling
                ),
            ]
        );
    }

    /// An empty registry still names the built-in lanes and sub-goal spawning.
    ///
    /// Pins a dispatchable set built only from registered providers: `files`,
    /// `http`, `shell`, `duckdb` and `spawn_sub_goal` are policy names the
    /// executor emits and no pack registers, so a ceiling could never name them
    /// and an owner could never permit a file write inside bounded work.
    #[test]
    fn the_dispatchable_set_always_names_the_non_pack_policy_names() {
        assert_eq!(
            dispatchable_capability_names(&CapabilityRegistry::new()),
            names(&["duckdb", "files", "http", "shell", "spawn_sub_goal"]),
            "an empty registry is not an empty answer, and it is not a wide one either"
        );
    }

    // -- replay -------------------------------------------------------------

    /// An identical create resumes the live engagement; a changed one is a
    /// conflict naming what differs.
    ///
    /// Pins the duplicate authority a retried POST would mint. The store stamps
    /// a fresh ULID every call, so without this the roster would hold two
    /// engagements for one piece of work and an owner revoking the one they were
    /// shown would leave the other standing. Returning the old engagement for
    /// changed terms is the opposite failure: the owner is told they widened a
    /// ceiling they did not.
    #[tokio::test]
    async fn an_identical_create_resumes_and_a_changed_one_conflicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for("support-triage", "Globex", &["research"], &[], EXPIRES_MS);

        let first = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect("created");
        assert!(!first.resumed);

        let replay =
            create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS + 500)
                .await
                .expect("resumed");
        assert!(replay.resumed);
        assert_eq!(replay.row.engagement_id, first.row.engagement_id);
        assert_eq!(
            replay.row.authority_revision, 1,
            "a replay must not bump the revision and invalidate live snapshots for nothing"
        );
        assert_eq!(
            engagement_rows_with_store(&store, "alpha", "prod", 2_000)
                .await
                .len(),
            1,
            "a retried create must not mint a second authority"
        );

        let widened = grant_for(
            "support-triage",
            "Globex",
            &["research", "raw_email"],
            &[],
            EXPIRES_MS,
        );
        let conflict =
            create_engagement_with_store(&store, "alpha", "prod", &widened, CREATED_MS + 600)
                .await
                .expect_err("conflict");
        assert_eq!(
            conflict,
            CreateRefusal::Conflicts {
                engagement_id: first.row.engagement_id.clone(),
                differences: vec!["tool_ceiling".to_string()],
            }
        );
        assert_eq!(
            engagement_rows_with_store(&store, "alpha", "prod", 2_000).await[0]
                .recorded_tool_ceiling,
            vec!["research".to_string()],
            "a refused create must not have widened the live ceiling"
        );
    }

    /// A revoked engagement is never resumed by a later create; the new one gets
    /// its own id.
    ///
    /// Pins resurrection through the write path. Terminal states never come
    /// back: if a create resumed the revoked engagement it would report as live
    /// what the dispatch boundary still denies, and the owner's withdrawal would
    /// read as undone.
    #[tokio::test]
    async fn a_revoked_engagement_is_never_resumed_by_a_later_create() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for("support-triage", "Globex", &["research"], &[], EXPIRES_MS);

        let first = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect("created");
        revoke_engagement_with_store(&store, "alpha", "prod", &first.row.engagement_id, 2_000)
            .await
            .expect("revoked");

        let second = create_engagement_with_store(&store, "alpha", "prod", &grant, 2_500)
            .await
            .expect("created afresh");
        assert!(!second.resumed);
        assert_ne!(second.row.engagement_id, first.row.engagement_id);
        assert_eq!(second.row.standing, EngagementStanding::Live);

        let rows = engagement_rows_with_store(&store, "alpha", "prod", 2_600).await;
        assert_eq!(rows.len(), 2);
        let counts = standing_counts(&rows);
        assert_eq!(counts["live"], 1);
        assert_eq!(counts["revoked"], 1);
    }

    // -- renewal ------------------------------------------------------------

    /// Renewal moves the window forward on the same id, so an execution already
    /// carrying it keeps dispatching.
    ///
    /// Pins the orphaning re-grant. Creating a replacement gives a new ULID, and
    /// every execution, pause blob, envelope and disclosure carrying the old id
    /// goes on naming an authority that has lapsed — nothing repoints them,
    /// because nothing was ever copied anywhere.
    #[tokio::test]
    async fn renewal_keeps_the_id_so_nothing_bound_to_it_is_orphaned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for("support-triage", "Globex", &["research"], &[], EXPIRES_MS);
        let created = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect("created");
        let carried = EngagementAuthorityRef {
            engagement_id: created.row.engagement_id.clone(),
            authority_revision: created.row.authority_revision,
        };

        // Past the window, the execution's next dispatch is denied.
        assert_eq!(
            authorize_engagement_dispatch_with_store(
                &store,
                &carried,
                "alpha",
                "prod",
                "research",
                EXPIRES_MS + 1
            )
            .await
            .expect_err("lapsed"),
            AuthorityDenial::Expired
        );

        let renewed = renew_engagement_with_store(
            &store,
            "alpha",
            "prod",
            &created.row.engagement_id,
            EXPIRES_MS * 2,
            2_000,
        )
        .await
        .expect("renewed");
        assert!(renewed.renewed);
        assert_eq!(renewed.row.engagement_id, created.row.engagement_id);
        assert_eq!(renewed.row.expires_at_ms, EXPIRES_MS * 2);
        assert_eq!(renewed.row.authority_revision, 2);
        assert_eq!(
            renewed.row.recorded_tool_ceiling,
            vec!["research".to_string()],
            "renewal moves the window and nothing else"
        );
        assert_eq!(
            engagement_rows_with_store(&store, "alpha", "prod", 2_100)
                .await
                .len(),
            1,
            "renewal is not a second engagement"
        );

        // The same carried ref — the one the running execution holds — now
        // dispatches past the old window.
        let dispatched = authorize_engagement_dispatch_with_store(
            &store,
            &carried,
            "alpha",
            "prod",
            "research",
            EXPIRES_MS + 1,
        )
        .await
        .expect("renewed authority");
        assert!(
            dispatched.revision_changed,
            "carried revision 1 vs live revision 2"
        );

        // An identical renewal is a free replay.
        let replay = renew_engagement_with_store(
            &store,
            "alpha",
            "prod",
            &created.row.engagement_id,
            EXPIRES_MS * 2,
            3_000,
        )
        .await
        .expect("replay");
        assert!(!replay.renewed);
        assert_eq!(replay.row.authority_revision, 2);
    }

    /// A lapsed engagement refuses renewal, and the refusal writes nothing.
    ///
    /// Pins resurrection by the clock. Expiry is terminal without an owner act,
    /// so moving a window that has already closed would re-authorise acts that
    /// were being denied a moment earlier, with nothing in the record to say
    /// authority had ever stopped.
    #[tokio::test]
    async fn a_lapsed_engagement_refuses_renewal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, created) =
            store_with(&dir, &[("support-triage", "Globex", &["research"])]).await;
        let id = created[0].engagement_id.clone();

        let refusal =
            renew_engagement_with_store(&store, "alpha", "prod", &id, EXPIRES_MS * 2, EXPIRES_MS)
                .await
                .expect_err("lapsed");
        assert!(
            matches!(refusal, RenewRefusal::NotRenewable(_)),
            "{refusal:?}"
        );

        let row = engagement_row_with_store(&store, "alpha", "prod", &id, EXPIRES_MS)
            .await
            .expect("row");
        assert_eq!(row.expires_at_ms, EXPIRES_MS, "the window did not move");
        assert_eq!(row.authority_revision, 1);
        assert_eq!(row.standing, EngagementStanding::Expired);
    }

    /// Another principal can neither renew someone else's engagement nor have
    /// their create resume it.
    ///
    /// Pins the widening mutation. `EngagementStore::extend_expiry` finds by id
    /// across the whole roster with no scope filter, and unlike revoke it *adds*
    /// authority — a guessed id would extend a window in a roster the caller
    /// cannot even read. The create half pins the replay read leaking across
    /// scopes: resuming another tenant's engagement would hand its id, ceiling
    /// and team to whoever asked.
    #[tokio::test]
    async fn another_principal_can_neither_renew_nor_resume_someone_elses_engagement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = empty_store(&dir).await;
        let grant = grant_for("support-triage", "Globex", &["research"], &[], EXPIRES_MS);
        let mine = create_engagement_with_store(&store, "alpha", "prod", &grant, CREATED_MS)
            .await
            .expect("created");

        assert_eq!(
            renew_engagement_with_store(
                &store,
                "beta",
                "prod",
                &mine.row.engagement_id,
                EXPIRES_MS * 2,
                2_000,
            )
            .await,
            Err(RenewRefusal::NotInScope)
        );

        let theirs = create_engagement_with_store(&store, "beta", "prod", &grant, 2_000)
            .await
            .expect("created in their own scope");
        assert!(
            !theirs.resumed,
            "another scope's engagement must not be resumed"
        );
        assert_ne!(theirs.row.engagement_id, mine.row.engagement_id);

        let untouched =
            engagement_row_with_store(&store, "alpha", "prod", &mine.row.engagement_id, 2_100)
                .await
                .expect("row");
        assert_eq!(untouched.expires_at_ms, EXPIRES_MS);
        assert_eq!(untouched.authority_revision, 1);
    }

    // -- routes -------------------------------------------------------------

    fn create_body() -> serde_json::Value {
        json!({
            "work_context": { "kind": "program", "id": "support-triage" },
            "counterparty": "Globex",
            "tool_ceiling": ["research"],
            "team": ["support-agent"],
            "expires_at_ms": EXPIRES_MS,
        })
    }

    /// `POST /engagements` reaches the create handler rather than the listing
    /// registered on the same literal, and refuses when the registry is absent.
    ///
    /// Pins two failures at once. A second `.route` on one path that answered
    /// 405 from the first registration would make the whole write path
    /// unreachable. And a create that proceeded without a registry would skip
    /// the ceiling check entirely — unknown names would be written as a grant,
    /// which is the exact failure the check exists for. The distinct error codes
    /// are what prove which handler answered.
    #[actix_web::test]
    async fn the_create_route_is_reachable_and_refuses_without_a_capability_registry() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;

        let created = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/engagements")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(create_body())
                .to_request(),
        )
        .await;
        assert_eq!(created.status().as_u16(), 503);
        let body: serde_json::Value = actix_test::read_body_json(created).await;
        assert_eq!(
            body["code"], "capability_registry_unavailable",
            "the POST reached the create handler, not the listing on the same path"
        );

        let listed = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/engagements")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .to_request(),
        )
        .await;
        let listed_body: serde_json::Value = actix_test::read_body_json(listed).await;
        assert_eq!(listed_body["code"], "engagement_store_unavailable");
    }

    /// A create with no `expires_at_ms` is refused by the body contract itself.
    ///
    /// Pins the standing grant. The field carries no serde default precisely so
    /// an absent expiry cannot be filled in with one the owner never saw —
    /// authority that never lapses is the thing this carrier exists to bound.
    #[actix_web::test]
    async fn a_create_with_no_expiry_is_refused() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;
        let mut body = create_body();
        assert!(body
            .as_object_mut()
            .expect("object")
            .remove("expires_at_ms")
            .is_some());

        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/engagements")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(body)
                .to_request(),
        )
        .await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "an expiry-less engagement is refused, never defaulted"
        );
    }

    /// A create without scope, or with a counterparty label carrying the field
    /// separator, is refused before the registry or the store is touched.
    ///
    /// Pins two doors. A create that defaulted the principal would file one
    /// owner's authority under whoever asked. And the label is hashed into a
    /// counterparty id, so one carrying U+001F could move the boundary between
    /// two components and derive one id for two organisations — the refusal has
    /// to happen before anything downstream sees the string.
    #[actix_web::test]
    async fn a_create_is_refused_without_scope_and_with_a_hostile_counterparty_label() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;

        let unscoped = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/engagements")
                .set_json(create_body())
                .to_request(),
        )
        .await;
        assert_eq!(unscoped.status().as_u16(), 400);

        let mut body = create_body();
        body["counterparty"] = json!("Globex\u{1f}Holdings");
        let hostile = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/engagements")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(body)
                .to_request(),
        )
        .await;
        assert_eq!(
            hostile.status().as_u16(),
            400,
            "refused as malformed, not answered 503 after a registry lookup"
        );
        let hostile_body: serde_json::Value = actix_test::read_body_json(hostile).await;
        assert_eq!(hostile_body["code"], "invalid_request");
    }

    /// `PATCH /engagements/{id}` reaches the renewal handler rather than the
    /// `GET` registered on the same path.
    ///
    /// Pinned by the status: the renewal extracts a body, so a request missing
    /// `expires_at_ms` answers 400, while the `GET` on this path takes no body
    /// and would have answered 503 at the store check. A 404 would mean the
    /// method never matched at all and renewal was unreachable — leaving
    /// re-creation, which mints a new id and orphans everything bound to the
    /// old one, as the only way to extend a window.
    #[actix_web::test]
    async fn the_renewal_route_is_reachable_and_not_shadowed_by_the_id_read() {
        let app =
            actix_test::init_service(actix_web::App::new().configure(configure_engagement_routes))
                .await;

        let no_window = actix_test::call_service(
            &app,
            actix_test::TestRequest::patch()
                .uri("/engagements/eng-1")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(json!({}))
                .to_request(),
        )
        .await;
        assert_eq!(no_window.status().as_u16(), 400);

        let with_window = actix_test::call_service(
            &app,
            actix_test::TestRequest::patch()
                .uri("/engagements/eng-1")
                .insert_header(("X-Principal", "alpha"))
                .insert_header(("X-Workspace", "prod"))
                .set_json(json!({ "expires_at_ms": 4_102_444_800_000_i64 }))
                .to_request(),
        )
        .await;
        assert_eq!(
            with_window.status().as_u16(),
            503,
            "past the body contract, the renewal stops at the missing store"
        );
        let body: serde_json::Value = actix_test::read_body_json(with_window).await;
        assert_eq!(body["code"], "engagement_store_unavailable");
    }
}
