//! The joins that give the five phases something real to run on.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md`.
//! Doc: `docs/components/magician/outcome-learning.md`.
//!
//! [`super::feeders`] converts records other subsystems keep into the shapes
//! the phases consume, and every one of its four conversions was **complete,
//! tested and called by nothing outside its own file**. A conversion nobody
//! calls is a fold over an empty collection, which is how a learning loop runs
//! for a year having compared nothing while every one of its tests passes.
//! This module is the caller.
//!
//! # Three joins, and the fourth that already has an owner
//!
//! | join | feeder | reached from |
//! |---|---|---|
//! | cohort comparison → a candidate an owner decides on | [`candidates_to_learning`] | [`worker::OutcomeProposalWorker::spawn`] |
//! | rooms and their access lane → the market read | [`room_attention_from_access`] | `GET /outcome-learning/market-read` |
//! | the assertion reverse index → stale and contradicted claims | [`claim_uses_from_index`] | `GET /outcome-learning/claim-health` |
//! | outward acts → acts awaiting an outcome | [`awaiting_outcomes`] | [`super::book::spawn_configured_maturity_sweep`], **not this module** |
//!
//! The fourth is deliberately absent here. [`super::sweep::sweep_matured_silences`]
//! already calls [`awaiting_outcomes`], [`super::book`] supplies the acts and
//! [`super::worker::MaturityWorker`] runs the cadence — so a second caller in
//! this file would be a second writer of the same append-only observations,
//! racing the first over a store that cannot un-record. What this module does
//! instead is **depend on whether that sweep is running**, through
//! [`MaturitySweepStanding`], and refuse to propose when it is not.
//!
//! [`awaiting_outcomes`]: super::feeders::awaiting_outcomes
//! [`candidates_to_learning`]: super::feeders::candidates_to_learning
//! [`claim_uses_from_index`]: super::feeders::claim_uses_from_index
//! [`room_attention_from_access`]: super::feeders::room_attention_from_access
//!
//! # Why a coordinator, and why it lives here
//!
//! The same reason `delivery_hygiene` and `obligation_sweeps` exist: the join
//! needs an owner, and neither side should learn the other. The difference is
//! where it sits. `super::feeders` **already** imports the data room, the share
//! ledger's access lane, the learning substrate and the outward-assertions
//! record — it is the seam by construction — and none of those four imports
//! `outcome_learning`. So a coordinator inside this subsystem adds no new
//! dependency edge at all, while a sibling module would add four.
//!
//! # Nothing here names a kind of relationship or a domain
//!
//! Rooms carry an [`AudienceRef`], and it is passed straight through: a
//! support account's room, a recruiting panel's pack and a supplier's diligence
//! set aggregate into one market read with no arm of
//! [`AudienceKind`](magician::magician_v2::audience::AudienceKind) named below. A
//! variant is whatever the deciding subsystem declared it to be — a wording, a
//! macro, a framing — and the candidate type a proposal lands under is the
//! caller's, not this file's.
//!
//! # Fail closed
//!
//! - **An unreadable store is a fault, never an empty answer.** Every read
//!   propagates. *"There is nothing to compare"* out of a disk fault is the
//!   most reassuring wrong answer this loop could give.
//! - **A decision already made cannot move.** A comparison whose candidate
//!   already exists is answered from what is recorded **before** anything is
//!   derived — the same ordering [`super::maturity::mature_silences`] had to be
//!   corrected into, where deriving ripeness first made a settled act report as
//!   still open the moment somebody widened a window.
//! - **A withheld comparison keeps its counts.** *"We have no evidence"* and
//!   *"we have three observations and the floor is five"* are different answers
//!   and only one of them invites a decision, so every refusal below carries
//!   the numbers it was measured against.
//! - **A report about no claims is refused, not clean.** [`claim_health`] over
//!   an empty claim list would be a vacuous pass over an empty collection — the
//!   bug class this whole programme keeps finding.
//! - **Counts, never rates.** No ratio is computed anywhere below.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::data_room::access_store::{AccessScope, AccessStore};
use crate::data_room::{AccessEvent, AttentionSignal, DataRoomScope, DataRoomStore};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::evidence::outward_assertions::{
    OutwardAssertionStore, OutwardAssertionUse, OutwardScope, CLAIM_AXIS,
};
use magician::magician_v2::learning::{LearningCandidateFilters, LearningScope, LearningStore};
use magician::magician_v2::share_links::{ShareLinkScope, ShareLinkStore};

use super::aggregate::{
    document_market_read, market_confidence_floor, never_opened_across, DocumentRead,
};
use super::feeders::{
    candidates_to_learning, claim_uses_from_index, room_attention_from_access, EvidenceFloor,
    LearningTarget, RefusedClaimUse, RoomShare, UnattributedAccess, UnresolvedSupersession,
};
use super::proposal::{propose, summarise_cohort, NotProposable};
use super::retirement::{
    contradictions, retirement_candidates, Contradiction, RetirementCandidate, RetirementPolicy,
};
use super::store::{OutcomeScope, OutcomeStore};

pub mod api;
pub mod worker;

#[cfg(test)]
mod tests;

/// The character every derived id in this subsystem joins its fields with.
///
/// Guarded here because this file derives a learning candidate id from caller
/// strings. A value carrying the separator moves the boundary between two
/// fields, so two different comparisons derive one id — and the second would
/// resume the first's candidate instead of being proposed at all.
const FIELD_SEP: char = '\u{1f}';

/// A caller string that reaches a derived id or an index path.
fn guard_component(what: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{what} must be named: a blank one addresses every record and none of them");
    }
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other, and a value carrying it could fold two \
             different records into one"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

// ── 1. Cohort comparisons → the learning substrate ──────────────────────────

/// Two versions of one variant, put side by side.
///
/// Which version is the **baseline** is the earlier one, and "earlier" is read
/// from the store's own first observation per cohort rather than from a version
/// lineage, because nothing anywhere records that a version superseded another.
/// See [`comparisons_in_scope`] for what that costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortComparison {
    pub variant_ref: String,
    pub baseline_version: String,
    pub candidate_version: String,
}

/// Whether a maturity sweep is actually recording this scope's silences.
///
/// # Why the proposal half must know
///
/// A cohort summarised from a store that only ever receives replies is the
/// most flattering dataset available and the least useful one: every
/// counterparty who ignored us is missing from it, so `engaged 4 of 4` is
/// indistinguishable from `engaged 4 of 40`. That is not a hypothetical — it
/// is the state [`super::worker`] documents as the failure the maturity phase
/// exists to prevent, arrived at by omission.
///
/// So the standing is carried rather than assumed, and a scope no sweep covers
/// has every comparison withheld **with its sample sizes** rather than
/// proposed. Deliberately two facts and not one: a sweep that is configured but
/// has never completed a tick has recorded nothing, and configuration alone
/// would report it as covered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaturitySweepStanding {
    swept: BTreeSet<(String, String)>,
    completed_a_tick: bool,
}

impl MaturitySweepStanding {
    /// The tenants a maturity sweep covers, and whether one of its ticks has
    /// finished.
    ///
    /// A narrow signature on purpose: it takes the two facts rather than the
    /// maturity worker's configuration or its health snapshot, so the shape of
    /// either can change without this module being edited.
    pub fn new(swept_scopes: Vec<(String, String)>, completed_a_tick: bool) -> Self {
        Self {
            swept: swept_scopes.into_iter().collect(),
            completed_a_tick,
        }
    }

    /// Nothing is swept. The honest default, and what a caller with no maturity
    /// worker must pass — never an empty [`new`](Self::new) with `true`, which
    /// would claim coverage of nothing.
    pub fn never_swept() -> Self {
        Self::default()
    }

    /// Whether this tenant's silences are being recorded.
    ///
    /// Both halves are required. A membership test against an empty set is
    /// false here rather than vacuously true, which is the direction that
    /// withholds a proposal rather than making one.
    pub fn covers(&self, principal: &str, workspace: &str) -> bool {
        self.completed_a_tick
            && self
                .swept
                .contains(&(principal.to_string(), workspace.to_string()))
    }
}

/// Why one comparison did not become a candidate.
///
/// Every arm carries numbers. A learning loop that answers *"nothing"* is
/// indistinguishable from a broken one, and the counts are what let an owner
/// tell *"we have not started"* from *"we have three and need five"*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComparisonWithheld {
    /// The comparison itself was refused, with the counts it was measured
    /// against. Reuses [`NotProposable`] rather than restating it: the refusal
    /// an owner reads should be the same whether it was made when the cohorts
    /// were compared or when the candidate was converted.
    NotProposable(NotProposable),
    /// A candidate for this exact comparison already exists. Nothing moved, and
    /// nothing about it was re-derived — a decision already made must not shift
    /// because a floor, a window or a candidate type changed in configuration.
    AlreadyProposed { candidate_id: String },
    /// No maturity sweep is recording this scope's silences, so both cohorts
    /// hold only the counterparties who answered.
    SilenceNeverSwept {
        baseline_usable: usize,
        candidate_usable: usize,
    },
}

/// One comparison that did not become a candidate, named so a caller can say
/// why the loop is quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldComparison {
    pub variant_ref: String,
    pub baseline_version: String,
    pub candidate_version: String,
    pub reason: ComparisonWithheld,
}

/// What one proposal pass did — counts and named refusals, never a rate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateSweep {
    /// Comparisons offered to this pass.
    pub considered: usize,
    /// Learning candidate ids this pass wrote.
    pub proposed: Vec<String>,
    /// Comparisons that produced nothing, each with its reason.
    pub withheld: Vec<WithheldComparison>,
}

impl CandidateSweep {
    /// Whether this pass wrote anything.
    ///
    /// *"Nothing to propose on these facts"*, never *"everything is decided"* —
    /// a pass over no comparisons at all is the vacuous case and reads the same
    /// as a pass over cohorts that were all too small.
    pub fn recorded_nothing(&self) -> bool {
        self.proposed.is_empty()
    }
}

/// The comparisons this scope's own records support.
///
/// Consecutive versions of each variant, ordered by the earliest observation in
/// each cohort: `(v1, v2)`, then `(v2, v3)`. A variant with one version yields
/// nothing, which is correct — there is nothing to compare it against, and a
/// cohort compared with itself is the self-confirmation §5 forbids.
///
/// # The gap this papers over, named rather than hidden
///
/// **Nothing anywhere records that one version superseded another.** The order
/// used here is the order the store first saw outcomes in, which is right for
/// a version rolled out after another and wrong for two versions run
/// concurrently as an A/B — there, whichever happened to be observed first
/// reads as the baseline. The comparison itself is symmetric enough that the
/// candidate is still true (it states both cohorts whole), but the words
/// "baseline" and "candidate" would be the wrong way round. Closing it needs a
/// version lineage on the declaration side; until one exists this is the
/// closest honest answer, and it is derived rather than typed so the loop
/// discovers its own comparisons instead of waiting for somebody to name them.
pub fn comparisons_in_scope(
    outcomes: &OutcomeStore,
    scope: &OutcomeScope,
) -> Result<Vec<CohortComparison>> {
    let cohorts = outcomes.recorded_cohorts(scope).with_context(|| {
        format!(
            "reading the cohorts of `{}`/`{}` to find what can be compared",
            scope.principal, scope.workspace
        )
    })?;

    // `recorded_cohorts` is already sorted by variant, then first observation,
    // then version — so grouping preserves that order and consecutive pairs are
    // consecutive versions.
    let mut by_variant: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for cohort in &cohorts {
        by_variant
            .entry(cohort.variant_ref.as_str())
            .or_default()
            .push(cohort.variant_version.as_str());
    }

    let mut out = Vec::new();
    for (variant_ref, versions) in by_variant {
        for pair in versions.windows(2) {
            out.push(CohortComparison {
                variant_ref: variant_ref.to_string(),
                baseline_version: pair[0].to_string(),
                candidate_version: pair[1].to_string(),
            });
        }
    }
    Ok(out)
}

/// The learning candidate id one comparison always derives.
///
/// Derived from the tenant and the three names, and from **nothing that moves**
/// — not the clock, not the sample size, not the floor. That is what makes a
/// second pass resume rather than propose again, and it is also what makes a
/// decision terminal: an owner who rejected this comparison does not get it
/// back because more observations arrived.
///
/// `lc_` prefixed and hex-bodied because the learning store refuses any other
/// shape for a caller-supplied id.
pub fn candidate_id_for(scope: &OutcomeScope, comparison: &CohortComparison) -> String {
    format!(
        "lc_oc_{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            comparison.variant_ref,
            comparison.baseline_version,
            comparison.candidate_version,
        ))
    )
}

/// Compare each cohort pair and file what survives as a learning candidate.
///
/// # The one write path in this module
///
/// Everything else here reads. This is what closes the loop the plan opens
/// with: observations become a comparison, a comparison becomes a thing an
/// owner decides on, and the substrate that stores it already has the review,
/// the decision log and the surface.
///
/// # Idempotent, in the order that matters
///
/// The candidate id is derived first and answered against what the substrate
/// already holds **before any cohort is read**. That ordering is the lesson
/// from [`super::maturity::mature_silences`], where deriving ripeness ahead of
/// the recorded fact made a settled act report as still open the moment a
/// window widened: a decision already recorded must not be re-derived, because
/// re-deriving it under today's configuration is how it silently moves.
///
/// The listing that answers it **propagates a read fault** rather than folding
/// it to "no candidates". An unreadable substrate that read as empty would
/// re-propose every comparison on every tick.
///
/// # Every refusal keeps its counts
///
/// A comparison below the floor is withheld carrying both cohorts' sizes, a
/// comparison in an unswept scope is withheld carrying the same, and a
/// comparison already decided is withheld carrying the id of the decision. None
/// of them is dropped.
///
/// # Errors
///
/// A crossed tenant, a caller string carrying the field separator, any store
/// that cannot be read, and a substrate that already holds this id with
/// different content — which means two passes disagreed about what the same
/// comparison says, and picking either would hand an owner a finding that is
/// not the one the evidence supports.
#[allow(clippy::too_many_arguments)]
pub fn sweep_candidates_into_learning(
    outcomes: &OutcomeStore,
    outcome_scope: &OutcomeScope,
    learning: &LearningStore,
    learning_scope: &LearningScope,
    comparisons: &[CohortComparison],
    floor: EvidenceFloor,
    target: &LearningTarget,
    standing: &MaturitySweepStanding,
    now: DateTime<Utc>,
) -> Result<CandidateSweep> {
    // The two scopes are separate parameters because they are separate modules'
    // types, not because they may differ. Crossed, this would file one tenant's
    // evidence as another tenant's proposal — a wrong answer both stores would
    // report as a success.
    if outcome_scope.principal != learning_scope.principal
        || outcome_scope.workspace != learning_scope.workspace
    {
        anyhow::bail!(
            "refusing to propose `{}/{}`'s outcomes as `{}/{}`'s learning candidates: one \
             owner's evidence is not a finding about another owner's work, and the evidence \
             refs would point at observations the second owner cannot read",
            outcome_scope.principal,
            outcome_scope.workspace,
            learning_scope.principal,
            learning_scope.workspace,
        );
    }
    guard_component("a principal", &outcome_scope.principal)?;
    guard_component("a workspace", &outcome_scope.workspace)?;

    let mut sweep = CandidateSweep {
        considered: comparisons.len(),
        ..CandidateSweep::default()
    };
    if comparisons.is_empty() {
        return Ok(sweep);
    }

    // What the substrate already holds, of this candidate type. Read ONCE per
    // pass rather than per comparison, and never folded to empty on a fault.
    let held: BTreeSet<String> = learning
        .list_candidates(
            learning_scope,
            LearningCandidateFilters {
                candidate_type: Some(target.candidate_type.as_str().to_string()),
                ..LearningCandidateFilters::default()
            },
        )
        .with_context(|| {
            format!(
                "reading the learning candidates of `{}`/`{}` — an unreadable substrate must \
                 never read as one holding no decisions, or every comparison is proposed again \
                 on every pass",
                learning_scope.principal, learning_scope.workspace
            )
        })?
        .into_iter()
        .map(|candidate| candidate.id)
        .collect();

    let covered = standing.covers(&outcome_scope.principal, &outcome_scope.workspace);

    for comparison in comparisons {
        guard_component("a variant ref", &comparison.variant_ref)?;
        guard_component("a baseline variant version", &comparison.baseline_version)?;
        guard_component("a candidate variant version", &comparison.candidate_version)?;

        let candidate_id = candidate_id_for(outcome_scope, comparison);

        // ANSWERED FROM WHAT IS RECORDED BEFORE ANYTHING IS DERIVED. See the
        // function note: the reverse order is how a settled decision moves.
        if held.contains(&candidate_id) {
            sweep.withheld.push(WithheldComparison {
                variant_ref: comparison.variant_ref.clone(),
                baseline_version: comparison.baseline_version.clone(),
                candidate_version: comparison.candidate_version.clone(),
                reason: ComparisonWithheld::AlreadyProposed { candidate_id },
            });
            continue;
        }

        let baseline_rows = outcomes
            .cohort(
                outcome_scope,
                &comparison.variant_ref,
                &comparison.baseline_version,
            )
            .with_context(|| {
                format!(
                    "reading cohort `{}`/`{}`",
                    comparison.variant_ref, comparison.baseline_version
                )
            })?;
        let candidate_rows = outcomes
            .cohort(
                outcome_scope,
                &comparison.variant_ref,
                &comparison.candidate_version,
            )
            .with_context(|| {
                format!(
                    "reading cohort `{}`/`{}`",
                    comparison.variant_ref, comparison.candidate_version
                )
            })?;

        let baseline = summarise_cohort(
            &comparison.variant_ref,
            &comparison.baseline_version,
            &baseline_rows,
            now,
        );
        let candidate = summarise_cohort(
            &comparison.variant_ref,
            &comparison.candidate_version,
            &candidate_rows,
            now,
        );

        if !covered {
            sweep.withheld.push(WithheldComparison {
                variant_ref: comparison.variant_ref.clone(),
                baseline_version: comparison.baseline_version.clone(),
                candidate_version: comparison.candidate_version.clone(),
                reason: ComparisonWithheld::SilenceNeverSwept {
                    baseline_usable: baseline.usable,
                    candidate_usable: candidate.usable,
                },
            });
            continue;
        }

        // The observations the counts rest on, so a reader can go and look.
        // Sorted and deduplicated: the id set is part of the candidate payload
        // and a payload that depends on file order would make an identical
        // replay look like a changed one.
        let evidence_refs: Vec<String> = baseline_rows
            .iter()
            .chain(candidate_rows.iter())
            .filter(|observation| observation.is_usable_evidence(now))
            .map(|observation| observation.observation_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        // The instant the comparison is complete THROUGH, taken from the
        // evidence rather than the clock. It is what keeps the written payload
        // identical across passes, so a replay resumes instead of colliding
        // with itself — and it is the more honest reading anyway: a comparison
        // is as of its last observation, not as of whenever somebody looked.
        let as_of = baseline_rows
            .iter()
            .chain(candidate_rows.iter())
            .filter(|observation| observation.is_usable_evidence(now))
            .map(|observation| observation.observed_at)
            .max()
            .unwrap_or(now);

        let proposed = match propose(
            &comparison.variant_ref,
            &baseline,
            &candidate,
            evidence_refs,
            as_of,
        ) {
            Ok(proposed) => proposed,
            Err(reason) => {
                sweep.withheld.push(WithheldComparison {
                    variant_ref: comparison.variant_ref.clone(),
                    baseline_version: comparison.baseline_version.clone(),
                    candidate_version: comparison.candidate_version.clone(),
                    reason: ComparisonWithheld::NotProposable(reason),
                });
                continue;
            },
        };

        // The floor is re-applied here, in the conversion, because `propose`'s
        // minimums are the module's and this floor is the deployment's. A
        // candidate below it comes back in `withheld` with its counts.
        let feed = candidates_to_learning(std::slice::from_ref(&proposed), floor, target);
        for withheld in feed.withheld {
            sweep.withheld.push(WithheldComparison {
                variant_ref: comparison.variant_ref.clone(),
                baseline_version: comparison.baseline_version.clone(),
                candidate_version: comparison.candidate_version.clone(),
                reason: ComparisonWithheld::NotProposable(withheld.reason),
            });
        }
        for request in feed.proposed {
            learning
                .ensure_candidate_with_id(learning_scope.clone(), request, &candidate_id)
                .with_context(|| {
                    format!(
                        "filing the comparison of `{}` `{}` against `{}` as learning candidate \
                         `{candidate_id}`",
                        comparison.variant_ref,
                        comparison.baseline_version,
                        comparison.candidate_version,
                    )
                })?;
            sweep.proposed.push(candidate_id.clone());
        }
    }

    Ok(sweep)
}

/// Read the scope's cohorts and file what they support.
///
/// The tick's whole job, split out from [`worker`] so it can be run against
/// real stores with no reactor, no cadence and no configuration in sight.
pub fn sweep_scope(
    workspace_layout: &ArtifactV2Workspace,
    scope: &OutcomeScope,
    floor: EvidenceFloor,
    target: &LearningTarget,
    standing: &MaturitySweepStanding,
    now: DateTime<Utc>,
) -> Result<CandidateSweep> {
    let outcomes = OutcomeStore::new(workspace_layout.clone());
    let learning = LearningStore::new(workspace_layout.clone());
    let learning_scope = LearningScope::new(scope.principal.clone(), scope.workspace.clone());
    let comparisons = comparisons_in_scope(&outcomes, scope)?;
    sweep_candidates_into_learning(
        &outcomes,
        scope,
        &learning,
        &learning_scope,
        &comparisons,
        floor,
        target,
        standing,
        now,
    )
}

// ── 2. Rooms and their access lane → the market read ────────────────────────

/// One document, as the market treated it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketDocument {
    pub read: DocumentRead,
    /// Whether the read is broad enough to be about the market rather than
    /// about one relationship. Reported beside the counts rather than used to
    /// filter: a document only one audience opened is still a fact, and hiding
    /// it would leave an owner unable to tell "narrow" from "absent".
    pub market_backed: bool,
}

/// What the market did with a scope's rooms.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarketRead {
    pub rooms_seen: usize,
    /// Distinct `(room, token)` shares the read rests on — the denominator's
    /// denominator, so an owner can tell a market read over eleven shares from
    /// one over two.
    pub shares_seen: usize,
    pub documents: Vec<MarketDocument>,
    /// Every `(audience, token)` that was shared a room and never presented it
    /// — a delivery question rather than a nudge.
    pub never_opened: Vec<(AudienceRef, String)>,
    /// Every `(audience, token)` that came back — **more than one visit**, not
    /// more than one click.
    ///
    /// Read from the attention row's own signal, which counts visits from the
    /// event `sequence`. A token that viewed the index and then opened a
    /// document in one sitting produced two events and is not here, because it
    /// came once: reporting it would turn a single read into evidence of
    /// interest, which is the difference §6 asks the log to keep.
    pub returned: Vec<(AudienceRef, String)>,
    /// Access events that could not be attributed to a share. Reported, never
    /// counted: the share ledger is the authority on who was shared, and this
    /// read will not claim "shared and read" about a grant it was never told
    /// happened.
    pub unattributed: Vec<UnattributedAccess>,
}

/// Fold every room in a scope into one market read.
///
/// §6: *"the financials were opened by nine of eleven; the team slide by two.
/// That is a fact about what this market finds load-bearing."*
///
/// # Visits, not clicks
///
/// Nothing here derives a visit. The share list, the documents and the raw
/// events go to [`room_attention_from_access`], which delegates to the access
/// log's own `attention_across` — and that reads the event's `sequence`, so one
/// visit that views the index and then opens a document is two events and one
/// visit. Re-deriving it here would give the codebase two answers to one
/// question, and the wrong one would report "came back to it" about somebody
/// who came once and clicked twice.
///
/// # Ghost tokens survive
///
/// The share list is read from the grant ledger, **never** from the events, and
/// revoked and expired grants are kept: the question a market read answers is
/// who was shown this, and a link that lapsed unopened is the purest form of
/// "no". Dropping the people who never appeared would report a room half of
/// whose shares went silent as fully read — inflating apparent engagement
/// exactly where the honest signal is.
///
/// One token per identity, at its earliest issue, because a rotated credential
/// is the same person in the same grant slot — the same reading the obligation
/// sweep takes, so the two cannot disagree about how many people hold a room.
///
/// # Errors
///
/// Any store that cannot be read. A scope with no rooms is an empty read and is
/// **not** a healthy one: `market_confidence_floor` is false for every document
/// in it, and there are no documents in it either.
pub fn market_read(
    workspace_layout: &ArtifactV2Workspace,
    scope: &OutcomeScope,
) -> Result<MarketRead> {
    guard_component("a principal", &scope.principal)?;
    guard_component("a workspace", &scope.workspace)?;

    let rooms = DataRoomStore::new(workspace_layout.clone())
        .list(&DataRoomScope::new(
            scope.principal.clone(),
            scope.workspace.clone(),
        ))
        .with_context(|| {
            format!(
                "listing the data rooms of `{}`/`{}` for the market read",
                scope.principal, scope.workspace
            )
        })?;

    let links = ShareLinkStore::new(workspace_layout.clone());
    let link_scope = ShareLinkScope::new(scope.principal.clone(), scope.workspace.clone());
    let access = AccessStore::new(workspace_layout.clone());
    let access_scope = AccessScope::new(scope.principal.clone(), scope.workspace.clone());

    let mut shares: Vec<RoomShare> = Vec::new();
    let mut events: Vec<AccessEvent> = Vec::new();
    let mut shares_seen = 0usize;

    for room in &rooms {
        let shared_with = shared_identities(&links, &link_scope, &room.room_id)?;
        shares_seen += shared_with.len();
        let documents: Vec<String> = room
            .present_documents()
            .into_iter()
            .map(|entry| entry.artifact_ref.clone())
            .collect();
        events.extend(
            access
                .events_for(&access_scope, &room.room_id)
                .with_context(|| format!("reading the access lane of room `{}`", room.room_id))?,
        );
        shares.push(RoomShare {
            room_id: room.room_id.clone(),
            audience: room.audience.clone(),
            shared_with,
            documents,
        });
    }

    let feed = room_attention_from_access(&shares, &events)
        .context("attributing room access to the shares it belongs to")?;

    let documents = document_market_read(&feed.rooms)
        .into_iter()
        .map(|read| MarketDocument {
            market_backed: market_confidence_floor(&read),
            read,
        })
        .collect();

    // Visits, read from the signal the access log derives from `sequence`.
    // Nothing here counts events.
    let mut returned: Vec<(AudienceRef, String)> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for room in &feed.rooms {
        for attention in &room.attentions {
            if attention.signal() != AttentionSignal::OpenedRepeatedly {
                continue;
            }
            // First-seen dedupe: two rooms on one relationship must not double
            // a line an owner acts on, the same rule `never_opened_across`
            // keeps.
            if seen.insert((room.audience.as_key(), attention.token_issued_to.clone())) {
                returned.push((room.audience.clone(), attention.token_issued_to.clone()));
            }
        }
    }
    returned.sort_by(|left, right| {
        left.0
            .as_key()
            .cmp(&right.0.as_key())
            .then_with(|| left.1.cmp(&right.1))
    });

    Ok(MarketRead {
        rooms_seen: rooms.len(),
        shares_seen,
        documents,
        never_opened: never_opened_across(&feed.rooms),
        returned,
        unattributed: feed.unattributed,
    })
}

/// One room's roster, as the grant ledger knows it.
///
/// See [`market_read`]'s note: earliest issue per identity, dead grants kept.
fn shared_identities(
    links: &ShareLinkStore,
    scope: &ShareLinkScope,
    room_id: &str,
) -> Result<Vec<String>> {
    let grants = links
        .for_resource(scope, room_id)
        .with_context(|| format!("reading the grants on room `{room_id}`"))?;
    let mut earliest: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for grant in grants {
        let issued_at = grant.issued_at;
        earliest
            .entry(grant.issued_to)
            .and_modify(|held| {
                if issued_at < *held {
                    *held = issued_at;
                }
            })
            .or_insert(issued_at);
    }
    Ok(earliest.into_keys().collect())
}

// ── 3. The assertion reverse index → stale and contradicted claims ──────────

/// What the record says about the claims somebody asked about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaimHealth {
    /// The claims the caller named.
    pub claims_asked: Vec<String>,
    /// Every claim the closure ended up holding the **whole** use history of —
    /// the asked ones, plus every claim reached through a supersession.
    ///
    /// Findings are reported for exactly these and no others, and the list
    /// travels with them so a reader can tell what the report was actually
    /// about. A claim outside it was never looked at, which is different from
    /// a claim that was looked at and found healthy.
    pub claims_reached: Vec<String>,
    /// Assertion uses the closure gathered, the pulled-in ones included.
    pub uses_seen: usize,
    /// Whether every supersession resolved, every row converted and every index
    /// entry loaded.
    ///
    /// **False is not a smaller answer, it is a less trustworthy one.** An
    /// unresolved supersession means retirement saw no replacement, so a
    /// superseded claim reads as merely idle and — worse — a stale claim
    /// asserted after its correction existed raises no contradiction at all.
    pub complete: bool,
    /// Claims that have gone quiet, for the reached claims only.
    pub retirement: Vec<RetirementCandidate>,
    /// Stale claims asserted after their correction existed, for the reached
    /// claims only.
    pub contradictions: Vec<Contradiction>,
    pub unresolved: Vec<UnresolvedSupersession>,
    pub refused: Vec<RefusedClaimUse>,
    /// Index entries naming an assertion use the store cannot produce.
    pub dangling: Vec<String>,
}

/// Answer *"is this claim stale, and did we keep saying it after correcting
/// it"* from the record of what was actually said.
///
/// §4's last candidate kind, and phase 5's own instruction: read *"what was
/// actually said, rather than a separately-maintained record of what we meant
/// to say"*.
///
/// # The closure is the half that was missing, and it closes by CLAIM
///
/// [`claim_uses_from_index`] translates an index row's `supersedes` — which
/// names **assertion uses** — into the **claims** retirement compares against,
/// and it can only do that from the rows it is handed. Reading one claim's axis
/// hands it rows naming uses of the claims they replaced without containing
/// them, so every pointer comes back unresolved, retirement sees no
/// supersession, and the contradiction half silently answers nothing.
///
/// Loading the missing use by id resolves the pointer and is **not enough**: it
/// brings in one use of that claim, so the claim's own history is partial, and
/// a partial history under-reports offences — a false clean, which is the worst
/// shape a report like this can take. So each newly-discovered claim is
/// re-seeded through the index and its **whole** use set is loaded before
/// anything is reported about it.
///
/// Progress is guaranteed: a round either loads a claim not yet loaded or ends,
/// and the store holds finitely many claims.
///
/// # The gap this cannot close, named rather than hidden
///
/// **The index has no reverse pointer from a superseded use to the use that
/// superseded it.** A supersession is declared on the *newer* claim's row. So
/// asking only about a stale claim can never discover its own correction, and
/// the report will say — truthfully, for what it looked at — that nothing
/// contradicts it. Naming the correction finds the pair, because the closure
/// walks forward from the declaration. Closing it properly needs a
/// `superseded` axis on the assertion index, which does not exist today, and
/// [`ClaimHealth::claims_reached`] is what keeps the partial answer legible in
/// the meantime.
///
/// # What is reported
///
/// Findings for every claim in [`ClaimHealth::claims_reached`], which is
/// exactly the set whose complete use history is in hand. Filtering to it is
/// correctness rather than tidiness: a claim present through one pulled-in use
/// would have its `total_uses` and `last_used_at` measured against a partial
/// view, and reporting that would be inventing an answer.
///
/// # Errors
///
/// An **empty claim list**, because a retirement report over no claims is a
/// clean bill of health nobody earned — the vacuous pass over an empty
/// collection this codebase keeps finding. A claim ref carrying the field
/// separator. Any store that cannot be read.
pub fn claim_health(
    outward: &OutwardAssertionStore,
    scope: &OutwardScope,
    claim_refs: &[String],
    policy: &RetirementPolicy,
    now: DateTime<Utc>,
) -> Result<ClaimHealth> {
    if claim_refs.is_empty() {
        anyhow::bail!(
            "a claim-health report over no claims would answer `nothing is stale and nothing \
             is contradicted` about a list nobody put anything in; name the claims whose \
             freshness matters rather than reading an empty pass as a clean one"
        );
    }
    guard_component("a principal", &scope.principal)?;
    guard_component("a workspace", &scope.workspace)?;

    let mut asked: Vec<String> = Vec::new();
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for claim_ref in claim_refs {
        guard_component("a claim ref", claim_ref)?;
        let claim_ref = claim_ref.trim().to_string();
        if wanted.insert(claim_ref.clone()) {
            asked.push(claim_ref);
        }
    }

    let mut loaded: BTreeSet<String> = BTreeSet::new();
    let mut rows: BTreeMap<String, OutwardAssertionUse> = BTreeMap::new();
    let mut dangling: Vec<String> = Vec::new();

    loop {
        let pending: Vec<String> = wanted.difference(&loaded).cloned().collect();
        if pending.is_empty() {
            break;
        }
        for claim_ref in pending {
            let use_ids = outward
                .index_entries(scope, CLAIM_AXIS, &claim_ref)
                .with_context(|| format!("reading the assertion uses of claim `{claim_ref}`"))?;
            for use_id in use_ids {
                if rows.contains_key(&use_id) {
                    continue;
                }
                match outward
                    .load_assertion_use(scope, &use_id)
                    .with_context(|| format!("loading assertion use `{use_id}`"))?
                {
                    Some(row) => {
                        rows.insert(use_id, row);
                    },
                    // Index before row: a pointer with no row is the crash
                    // window, and it is REPORTED rather than skipped — a use we
                    // cannot read is a use whose audience we cannot warn.
                    None => dangling.push(use_id),
                }
            }
            loaded.insert(claim_ref);
        }

        // Pointers into claims we have not read yet. Loading the use resolves
        // the pointer; adding its claim is what makes the next round read that
        // claim's WHOLE history, which is what keeps its offence list honest.
        let supplied: Vec<OutwardAssertionUse> = rows.values().cloned().collect();
        let feed = claim_uses_from_index(&supplied)
            .context("translating assertion uses into the claims retirement compares")?;
        for unresolved in &feed.unresolved {
            if rows.contains_key(&unresolved.superseded_use_ref) {
                continue;
            }
            if let Some(row) = outward
                .load_assertion_use(scope, &unresolved.superseded_use_ref)
                .with_context(|| {
                    format!(
                        "loading superseded assertion use `{}`",
                        unresolved.superseded_use_ref
                    )
                })?
            {
                wanted.insert(row.approved_claim_ref.clone());
                rows.insert(unresolved.superseded_use_ref.clone(), row);
            }
        }
    }

    let supplied: Vec<OutwardAssertionUse> = rows.values().cloned().collect();
    let mut feed = claim_uses_from_index(&supplied)
        .context("translating assertion uses into the claims retirement compares")?;

    let retirement = retirement_candidates(&feed.uses, policy, now)
        .into_iter()
        .filter(|candidate| loaded.contains(&candidate.claim_ref))
        .collect();
    let found = contradictions(&feed.uses, now)
        .into_iter()
        .filter(|contradiction| loaded.contains(&contradiction.stale_claim))
        .collect();

    let complete = feed.is_complete() && dangling.is_empty();
    Ok(ClaimHealth {
        claims_asked: asked,
        claims_reached: loaded.into_iter().collect(),
        uses_seen: feed.uses.len(),
        complete,
        retirement,
        contradictions: found,
        unresolved: std::mem::take(&mut feed.unresolved),
        refused: std::mem::take(&mut feed.refused),
        dangling,
    })
}
