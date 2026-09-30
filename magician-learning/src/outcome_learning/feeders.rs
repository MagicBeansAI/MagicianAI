//! The adapters that connect the five phases to real data.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md`.
//!
//! Every phase in this module folds over inputs it is handed rather than
//! reading a store of its own, which is what keeps them generic — and is also
//! why nothing calls them. Each fold currently runs over an empty collection.
//! These four conversions are the missing half: they turn records other
//! subsystems already keep into the shapes the phases consume.
//!
//! # Conversions, never reach-throughs
//!
//! Each function here is **pure**. It opens no store, performs no I/O and
//! returns intent — a value the caller then feeds to a phase, or hands to the
//! subsystem that owns the writing. A feeder that read another module's
//! storage would give this module a second authoritative copy of somebody
//! else's records, which is the failure the whole set is arranged to avoid.
//!
//! Because nothing here writes, none of the stores' ordering guarantees
//! (index-before-row, record-before-act) are this file's to keep. What is this
//! file's to keep is that a conversion never invents, never rounds and never
//! quietly drops.
//!
//! # Three rules the conversions share
//!
//! - **Nothing is dropped silently.** Every input that cannot be converted
//!   comes back in a refusal list with a typed reason, so a caller can say
//!   *why* a sweep saw less than it expected. A conversion that skipped
//!   quietly would present a partial sweep as a complete one.
//! - **An identical replay resumes; a changed payload is an error.** Where an
//!   input carries a stable id — an act, an assertion use, a room — the same
//!   id twice with the same content is one row, and the same id twice with
//!   different content is a caller assembling its input wrongly, which is
//!   refused rather than silently resolved to whichever copy came last.
//! - **Counts, never rates.** No conversion here computes a ratio, a score or
//!   a confidence from the counts it carries. The counts and their totals
//!   travel together and the reader weighs them.
//!
//! # Generic, and one consumer
//!
//! Nothing in the public API names a domain. The inputs are outward acts,
//! shared rooms, asserted claims and cohort comparisons, all of which exist
//! wherever a system says something to somebody and waits.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::data_room::access_log::{attention_across, AccessEvent};
use magician::magician_v2::audience::AudienceRef;
use magician::magician_v2::evidence::outward_assertions::{
    OutwardActDisclosure, OutwardActStatus, OutwardAssertionUse,
};
use magician::magician_v2::learning::{
    CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
    LearningEvidenceRef, LearningRiskLevel,
};

use super::aggregate::RoomAttention;
use super::maturity::AwaitingOutcome;
use super::proposal::{
    Candidate, CohortSummary, NotProposable, MINIMUM_COHORT, MINIMUM_COUNTERPARTIES,
};
use super::retirement::ClaimUse;
use super::types::{Confounder, DeliveryState};

/// The character every derived id in this subsystem joins its fields with.
///
/// Restated here because the store's copy is private, and because this file is
/// where caller-supplied strings are checked against it: a value carrying the
/// separator can move the boundary between two fields of a derived id, so two
/// different acts can hash to one observation — and the cohort index, which
/// splits a line on this character, truncates the act ref and loses the row
/// entirely.
const FIELD_SEP: char = '\u{1f}';

fn carries_field_separator(value: &str) -> bool {
    value.contains(FIELD_SEP)
}

// ── 1. Candidates → the learning substrate ──────────────────────────────────

/// The smallest sample that may reach the learning substrate.
///
/// A floor is a judgement about the domain, so it is configuration — but it
/// cannot be configured below the module's own minimums, and in particular it
/// cannot be configured to zero. A floor of zero is satisfied by an empty
/// cohort: `0 >= 0` passes, so a comparison of nothing against nothing would
/// arrive as a finding. A predicate that passes vacuously over an empty
/// collection is the exact shape of the bug this floor exists to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceFloor {
    usable: usize,
    counterparties: usize,
}

impl EvidenceFloor {
    /// Refuses anything below [`MINIMUM_COHORT`] / [`MINIMUM_COUNTERPARTIES`].
    pub fn new(usable: usize, counterparties: usize) -> Result<Self> {
        if usable < MINIMUM_COHORT {
            anyhow::bail!(
                "an evidence floor below {MINIMUM_COHORT} usable observations cannot hold: at \
                 zero an empty cohort satisfies it vacuously, and a comparison of nothing \
                 against nothing arrives as a finding"
            );
        }
        if counterparties < MINIMUM_COUNTERPARTIES {
            anyhow::bail!(
                "an evidence floor below {MINIMUM_COUNTERPARTIES} counterparties cannot hold: one \
                 counterparty is a fact about that counterparty, not about the variant"
            );
        }
        Ok(Self {
            usable,
            counterparties,
        })
    }

    pub fn usable(&self) -> usize {
        self.usable
    }

    pub fn counterparties(&self) -> usize {
        self.counterparties
    }

    /// Why these two cohorts may not carry a proposal, if they may not.
    ///
    /// Reuses [`NotProposable`] rather than inventing a parallel reason type:
    /// the refusal a caller shows an owner should read the same whether the
    /// comparison was refused when it was made or when it was converted.
    fn refusal(
        &self,
        baseline: &CohortSummary,
        candidate: &CohortSummary,
    ) -> Option<NotProposable> {
        if baseline.usable < self.usable || candidate.usable < self.usable {
            return Some(NotProposable::SampleTooSmall {
                baseline: baseline.usable,
                candidate: candidate.usable,
                needed: self.usable,
            });
        }
        if baseline.counterparties < self.counterparties
            || candidate.counterparties < self.counterparties
        {
            return Some(NotProposable::TooFewCounterparties {
                baseline: baseline.counterparties,
                candidate: candidate.counterparties,
                needed: self.counterparties,
            });
        }
        None
    }
}

impl Default for EvidenceFloor {
    fn default() -> Self {
        Self {
            usable: MINIMUM_COHORT,
            counterparties: MINIMUM_COUNTERPARTIES,
        }
    }
}

/// Where a converted candidate lands on the learning side.
///
/// The candidate type is the caller's, not this module's: the same cohort
/// comparison can propose a change to a template, a persona or a procedure,
/// and hard-coding one would tie this conversion to a single flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningTarget {
    pub candidate_type: LearningCandidateType,
    pub principal: Option<String>,
    pub workspace: Option<String>,
}

impl LearningTarget {
    pub fn new(candidate_type: LearningCandidateType) -> Self {
        Self {
            candidate_type,
            principal: None,
            workspace: None,
        }
    }

    pub fn in_scope(mut self, principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self.workspace = Some(workspace.into());
        self
    }
}

/// A candidate that did not reach the learning substrate, and why.
///
/// The reason carries the counts it was measured against, because *"nothing
/// was proposed"* and *"nothing was proposed because both cohorts hold three
/// observations and the floor is five"* are different answers to an owner
/// asking why the loop is quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldCandidate {
    pub variant_ref: String,
    pub reason: NotProposable,
}

/// What one conversion produced.
#[derive(Debug, Clone, Default)]
pub struct LearningFeed {
    pub proposed: Vec<CreateLearningCandidateRequest>,
    pub withheld: Vec<WithheldCandidate>,
}

impl LearningFeed {
    pub fn proposed_count(&self) -> usize {
        self.proposed.len()
    }

    pub fn withheld_count(&self) -> usize {
        self.withheld.len()
    }
}

/// Convert cohort comparisons into learning candidates, carrying N and the
/// confounders.
///
/// # The guardrail is re-applied here
///
/// `propose` already refuses an under-powered comparison, so a [`Candidate`]
/// arriving here has usually passed. Usually is not always: `Candidate`'s
/// fields are public, so one can be assembled without ever passing through
/// that refusal, and this conversion is the point where a finding leaves this
/// module for a substrate that stores, routes and surfaces it. A guardrail
/// that only runs on the path the author remembered is not a guardrail.
///
/// A candidate below the floor is **withheld with its counts** rather than
/// dropped, so the caller can say why the loop is quiet.
///
/// # What travels with the candidate
///
/// The whole of both cohort summaries — every count, every confounder value —
/// as the proposed change, and every observation id as an evidence ref. §9:
/// *"every proposal carries sample size, confounders and evidence refs"*, so a
/// candidate citing nothing is withheld too: it looks like a finding and
/// cannot be audited into one.
///
/// # What deliberately does not travel
///
/// `confidence` is left unset. It is the one field on the learning request
/// that would take a rate, and a cohort of six turned into `0.5` reads as
/// precision that is not there. The counts are in the payload; the reader
/// weighs them.
///
/// Every request is `review_required`, at medium risk, in the proposed state:
/// the learning substrate's auto-apply lanes open only for a low-risk
/// candidate that needs no review, so §8's *"it never applies anything; every
/// change is an owner editorial decision"* holds structurally rather than by
/// anyone remembering it.
///
/// Infallible on purpose: a comparison carries no stable id, so there is no
/// replay to resume and no changed payload to refuse. Refusals are per
/// candidate, in `withheld`.
pub fn candidates_to_learning(
    candidates: &[Candidate],
    floor: EvidenceFloor,
    target: &LearningTarget,
) -> LearningFeed {
    let mut feed = LearningFeed::default();
    for candidate in candidates {
        if let Some(reason) = floor.refusal(&candidate.baseline, &candidate.candidate) {
            feed.withheld.push(WithheldCandidate {
                variant_ref: candidate.variant_ref.clone(),
                reason,
            });
            continue;
        }
        // Membership over an empty collection: a candidate whose evidence list
        // is empty cites nothing, and "all of its refs check out" is vacuously
        // true of it.
        if candidate.evidence_refs.is_empty() {
            feed.withheld.push(WithheldCandidate {
                variant_ref: candidate.variant_ref.clone(),
                reason: NotProposable::NoEvidenceCited,
            });
            continue;
        }
        feed.proposed.push(learning_request(candidate, target));
    }
    feed
}

fn learning_request(
    candidate: &Candidate,
    target: &LearningTarget,
) -> CreateLearningCandidateRequest {
    let ((baseline_engaged, baseline_usable), (candidate_engaged, candidate_usable)) =
        candidate.engagement_counts();

    let mut rationale = format!(
        "{baseline_usable} usable observations from {} counterparties on version `{}`; \
         {candidate_usable} from {} on version `{}`. Counts, not rates: nothing here is applied, \
         and the comparison is evidence for an owner decision rather than a result.",
        candidate.baseline.counterparties,
        candidate.baseline.variant_version,
        candidate.candidate.counterparties,
        candidate.candidate.variant_version,
    );
    for caveat in &candidate.caveats {
        rationale.push_str("\n- ");
        rationale.push_str(caveat);
    }

    CreateLearningCandidateRequest {
        principal: target.principal.clone(),
        workspace: target.workspace.clone(),
        candidate_type: target.candidate_type.clone(),
        state: LearningCandidateState::Proposed,
        title: format!(
            "{}: version `{}` compared with `{}`",
            candidate.variant_ref,
            candidate.baseline.variant_version,
            candidate.candidate.variant_version,
        ),
        summary: format!(
            "engaged {baseline_engaged} of {baseline_usable}, then {candidate_engaged} of \
             {candidate_usable}"
        ),
        rationale,
        // Both summaries whole: the counts, the unusable observations that are
        // not among them, and every confounder value with the number of
        // observations that carried it.
        proposed_change: json!({
            "variant_ref": candidate.variant_ref,
            "baseline": candidate.baseline,
            "candidate": candidate.candidate,
            "caveats": candidate.caveats,
            "observed_at": candidate.observed_at,
        }),
        proposed_target: Some(candidate.variant_ref.clone()),
        // Never a rate. See the function note.
        confidence: None,
        source_agent_id: None,
        source_task_id: None,
        source_execution_id: None,
        source_chat_session_id: None,
        event_refs: Vec::new(),
        evidence_refs: candidate
            .evidence_refs
            .iter()
            .map(|observation_id| LearningEvidenceRef {
                kind: "outcome_observation".to_string(),
                id: Some(observation_id.clone()),
                path: None,
                uri: None,
                summary: None,
            })
            .collect(),
        risk_level: LearningRiskLevel::Medium,
        review_required: true,
        review_reason: Some(
            "a cohort comparison is evidence for an owner decision, never a change to apply"
                .to_string(),
        ),
        review_policy: Value::Null,
        promotion_target: None,
        promotion_policy: Value::Null,
    }
}

// ── 2. Access events → room attention ───────────────────────────────────────

/// A room as it was shared: who it went to, and what it holds.
///
/// The share list is supplied rather than derived from the events, and that is
/// the whole point of the type existing. A token that never appears in the log
/// is the never-opened case — the one signal that reports a message may not
/// have arrived at all — and reconstructing the list from observed events
/// would make it invisible by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomShare {
    pub room_id: String,
    pub audience: AudienceRef,
    /// Every token the room was shared with.
    pub shared_with: Vec<String>,
    /// What the room currently holds, so "unopened" means unopened out of what
    /// is actually there.
    pub documents: Vec<String>,
}

/// Why one access event was not folded into a room's attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAttributed {
    /// The event names a room the caller did not supply. Its share list is
    /// unknown, so nothing about it can be counted honestly.
    UnknownRoom,
    /// The token is not on the room's share list. Reported rather than
    /// counted: the aggregation's own rule is that the share list is the
    /// authority on who was shared, and it will not claim "shared and silent"
    /// — or "shared and read" — about a share it was never told happened.
    TokenNotShared,
    /// The event and the room disagree about which relationship the room
    /// serves. One of the two is wrong, and folding the event in would put a
    /// counterparty's attention under somebody else's name.
    AudienceMismatch { on_event: String, on_room: String },
}

/// One event that could not be attributed, named so a caller can find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnattributedAccess {
    pub room_id: String,
    pub token_issued_to: String,
    pub reason: NotAttributed,
}

/// What one conversion produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoomAttentionFeed {
    pub rooms: Vec<RoomAttention>,
    pub unattributed: Vec<UnattributedAccess>,
}

/// Build the market read's input from raw access events.
///
/// # Visits, not clicks
///
/// Delegated to `access_log::attention_across`, which derives visits from the
/// event's own `sequence` rather than counting events. One visit that views
/// the index and then opens a document is two events and one visit, and
/// counting events would report "came back to it" about somebody who came once
/// and clicked twice — the difference between interest and a single read.
/// Re-deriving that here would give the codebase two answers to one question.
///
/// # Ghost tokens survive
///
/// A token on the share list that never appears in the log gets an attention
/// row all the same, holding zero visits and every document in the room as
/// unopened. That row is what keeps it in the denominator of the market read.
/// Dropping it would report a room half of whose shares went silent as fully
/// read, which inflates apparent engagement exactly where the honest signal
/// is.
///
/// # Refusals
///
/// An event that cannot be attributed is returned in `unattributed`, never
/// dropped. The one whole-input error is a room supplied twice with different
/// contents: an identical repeat is one room, and a changed one is a caller
/// assembling its input wrongly, which must not be resolved by silently
/// keeping whichever copy came last.
pub fn room_attention_from_access(
    rooms: &[RoomShare],
    events: &[AccessEvent],
) -> Result<RoomAttentionFeed> {
    let mut known: BTreeMap<&str, &RoomShare> = BTreeMap::new();
    let mut order: Vec<&RoomShare> = Vec::new();
    for room in rooms {
        match known.get(room.room_id.as_str()) {
            Some(held) if *held == room => continue,
            Some(_) => anyhow::bail!(
                "room `{}` was supplied twice with different contents: an identical repeat is \
                 one room, but a changed share list is an assembly error, and resolving it to \
                 whichever copy came last would silently move who counts as shared",
                room.room_id
            ),
            None => {
                known.insert(room.room_id.as_str(), room);
                order.push(room);
            },
        }
    }

    let mut feed = RoomAttentionFeed::default();
    let mut by_room: BTreeMap<&str, Vec<AccessEvent>> = BTreeMap::new();
    for event in events {
        let Some(room) = known.get(event.room_id.as_str()).copied() else {
            feed.unattributed.push(UnattributedAccess {
                room_id: event.room_id.clone(),
                token_issued_to: event.token_issued_to.clone(),
                reason: NotAttributed::UnknownRoom,
            });
            continue;
        };
        if event.audience != room.audience {
            feed.unattributed.push(UnattributedAccess {
                room_id: event.room_id.clone(),
                token_issued_to: event.token_issued_to.clone(),
                reason: NotAttributed::AudienceMismatch {
                    on_event: event.audience.as_key(),
                    on_room: room.audience.as_key(),
                },
            });
            continue;
        }
        if !room
            .shared_with
            .iter()
            .any(|token| token == &event.token_issued_to)
        {
            feed.unattributed.push(UnattributedAccess {
                room_id: event.room_id.clone(),
                token_issued_to: event.token_issued_to.clone(),
                reason: NotAttributed::TokenNotShared,
            });
            continue;
        }
        by_room
            .entry(room.room_id.as_str())
            .or_default()
            .push(event.clone());
    }

    feed.rooms = order
        .into_iter()
        .map(|room| RoomAttention {
            audience: room.audience.clone(),
            shared_with: room.shared_with.clone(),
            attentions: attention_across(
                &room.shared_with,
                &room.documents,
                by_room
                    .get(room.room_id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ),
        })
        .collect();
    Ok(feed)
}

// ── 3. The outward-assertions reverse index → claim uses ────────────────────

/// A supersession this conversion could not translate.
///
/// The reverse index records supersession between **assertion uses**, and
/// retirement reasons about **claims**, so the translation is a lookup through
/// the rows in hand. A pointer to a use outside them cannot be resolved here —
/// the usual cause is reading the index for one claim, whose rows name the
/// uses of the claims they replaced but do not contain them.
///
/// Reported rather than dropped, because the consequence of dropping it is
/// silent and severe: retirement would see no supersession, so a superseded
/// claim would be listed as merely idle and — worse — a stale claim asserted
/// after its correction existed would raise no contradiction at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSupersession {
    /// The use that declared the supersession.
    pub use_ref: String,
    /// The use it declared it replaces, which is not among the supplied rows.
    pub superseded_use_ref: String,
}

/// Why one index row could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotConvertible {
    /// The row's timestamp is not readable as an instant.
    ///
    /// Refused rather than defaulted. Dating it now would reset the claim's
    /// idleness and suppress a staleness finding that is true today; dating it
    /// from the beginning of time would nominate the claim immediately. Both
    /// are answers invented from a value nobody can read.
    UnreadableTimestamp { value: String },
}

/// One row that did not convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedClaimUse {
    pub use_ref: String,
    pub reason: NotConvertible,
}

/// What one conversion produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaimUseFeed {
    pub uses: Vec<ClaimUse>,
    pub unresolved: Vec<UnresolvedSupersession>,
    pub refused: Vec<RefusedClaimUse>,
}

impl ClaimUseFeed {
    /// Whether every supersession resolved and every row converted.
    ///
    /// A caller that widens its index read and retries can use this to tell
    /// "the report is complete" from "the report is what could be built".
    pub fn is_complete(&self) -> bool {
        self.unresolved.is_empty() && self.refused.is_empty()
    }
}

/// Convert reverse-index rows into retirement's input.
///
/// # The translation that is the whole job
///
/// An index row's `supersedes` names **assertion uses**; a [`ClaimUse`]'s
/// names **claims**. Passing the ids through unchanged would compile, and
/// would be silently inert: retirement compares those entries against claim
/// refs, so nothing would ever match, no claim would be recognised as replaced
/// and no contradiction would ever be found. The translation is a lookup
/// through the supplied rows, and a pointer that does not resolve is reported.
///
/// Self-supersession is passed through rather than filtered: retirement treats
/// a claim replacing itself as a data error and neither shields nor accuses on
/// it, and duplicating that judgement here would put the rule in two places.
///
/// # Replay
///
/// The same `assertion_use_id` twice with identical content is one use — index
/// files are append-only and a caller merging two axes of them will see
/// repeats. The same id twice with different content is an error: those rows
/// are keyed by `(act, claim, audience)`, so two different bodies under one id
/// mean the input was assembled from records that disagree.
pub fn claim_uses_from_index(uses: &[OutwardAssertionUse]) -> Result<ClaimUseFeed> {
    let mut unique: BTreeMap<&str, &OutwardAssertionUse> = BTreeMap::new();
    let mut order: Vec<&OutwardAssertionUse> = Vec::new();
    for row in uses {
        match unique.get(row.assertion_use_id.as_str()) {
            Some(held) if *held == row => continue,
            Some(_) => anyhow::bail!(
                "assertion use `{}` was supplied twice with different content: an identical \
                 replay is one use, but a changed one means the rows disagree about what was \
                 said, and picking either would put a claim in front of an audience it may \
                 never have reached",
                row.assertion_use_id
            ),
            None => {
                unique.insert(row.assertion_use_id.as_str(), row);
                order.push(row);
            },
        }
    }

    let claim_of: BTreeMap<&str, &str> = unique
        .iter()
        .map(|(use_id, row)| (*use_id, row.approved_claim_ref.as_str()))
        .collect();

    let mut feed = ClaimUseFeed::default();
    for row in order {
        let Ok(used_at) = DateTime::parse_from_rfc3339(&row.recorded_at) else {
            feed.refused.push(RefusedClaimUse {
                use_ref: row.assertion_use_id.clone(),
                reason: NotConvertible::UnreadableTimestamp {
                    value: row.recorded_at.clone(),
                },
            });
            continue;
        };

        let mut supersedes: Vec<String> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for superseded_use in &row.supersedes {
            match claim_of.get(superseded_use.as_str()) {
                Some(claim_ref) => {
                    if seen.insert(*claim_ref) {
                        supersedes.push((*claim_ref).to_string());
                    }
                },
                None => feed.unresolved.push(UnresolvedSupersession {
                    use_ref: row.assertion_use_id.clone(),
                    superseded_use_ref: superseded_use.clone(),
                }),
            }
        }

        feed.uses.push(ClaimUse {
            claim_ref: row.approved_claim_ref.clone(),
            use_ref: row.assertion_use_id.clone(),
            // Verbatim, and only ever compared for distinctness. Where the key
            // was built from an audience reference it already carries the kind,
            // so two relationship kinds sharing an id stay distinct.
            audience: row.audience.clone(),
            used_at: used_at.with_timezone(&Utc),
            supersedes,
        });
    }
    Ok(feed)
}

// ── 4. Outward acts → acts awaiting an outcome ──────────────────────────────

/// One recorded act, with the cohort key it was performed under.
///
/// The act knows what went out and when; it does not know which variant
/// version was live when it did, because that is the deciding subsystem's
/// fact rather than the carrier's. Supplying it here is what makes the
/// resulting observation comparable to anything — without it a sample can only
/// ever agree with itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutwardActCohort {
    pub act: OutwardActDisclosure,
    pub variant_ref: String,
    /// The cohort key: which variant version was active when this act happened.
    pub variant_version: String,
    /// What was true about the situation that is not the thing being tested.
    pub confounders: Vec<Confounder>,
}

/// Why an act is not waiting for an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAwaiting {
    /// Nothing left, so there is nobody to be silent. Maturing this would
    /// record that a counterparty ignored a message never sent.
    NothingLeft {
        status: OutwardActStatus,
    },
    /// The act is settled — failed, or retracted. A terminal state does not
    /// resurrect into a pending one, and a failed act told nobody anything.
    Settled {
        status: OutwardActStatus,
    },
    /// The act left, but nothing recorded when.
    ///
    /// Refused rather than dated from preparation. Preparation is strictly
    /// earlier than dispatch, so a window measured from it closes early and
    /// records a decision the counterparty never had the chance to make —
    /// which is the failure the whole maturity phase exists to prevent.
    UndatedDispatch,
    UnreadableDispatchTime {
        value: String,
    },
    /// Dispatched after the moment being asked about. Nothing is yet awaited,
    /// and a window measured from a time that has not happened would close
    /// after one that has.
    DispatchedInTheFuture {
        dispatched_at: DateTime<Utc>,
    },
    /// A key the observation would be identified by is empty.
    MissingKey {
        field: &'static str,
    },
    /// A key carries the character derived ids join their fields with, so it
    /// can move the boundary between two fields — two different acts hashing
    /// to one observation — and truncate the act ref in the cohort index,
    /// which splits its lines on exactly this character.
    FieldSeparatorIn {
        field: &'static str,
    },
}

/// One act that is not awaiting an outcome, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAwaitingAct {
    pub act_ref: String,
    pub reason: NotAwaiting,
}

/// What one conversion produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AwaitingFeed {
    pub awaiting: Vec<AwaitingOutcome>,
    pub not_awaiting: Vec<NotAwaitingAct>,
}

impl AwaitingFeed {
    pub fn awaiting_count(&self) -> usize {
        self.awaiting.len()
    }
}

/// Assemble the maturity sweep's input from recorded outward acts.
///
/// # What is awaited, and what is not
///
/// Only an act that actually left. A prepared act never went; a failed one
/// told nobody; a retracted one is settled and does not come back. Silence
/// after any of those is a fact about the carrier, not about a counterparty.
///
/// The clock starts at **dispatch**, never at preparation, and an act with no
/// dispatch time is refused rather than dated from the earlier field — see
/// [`NotAwaiting::UndatedDispatch`].
///
/// # What this deliberately does not decide
///
/// - **Whether the window has closed.** That is the maturity policy's
///   judgement and `mature_silences` reports it as still open. A feeder that
///   pre-filtered by age would silently apply a second, undocumented window.
/// - **Whether anything already came back.** The sweep re-reads the act's own
///   observations for that, precisely because a caller's idea of "still
///   awaiting" is the thing that goes stale — and the cost of it being stale
///   is a recorded claim that somebody ignored us when they answered.
///
/// `now` bounds what is knowable, inclusively: an act dispatched at this very
/// instant has been dispatched, and only one dated strictly after it is
/// excluded.
///
/// Delivery state is **carried, not guessed**: a provider's acceptance is not
/// delivery, and an act whose state cannot be told apart from either is
/// carried as unknown so it counts as an act performed while never counting as
/// evidence about a counterparty.
pub fn awaiting_outcomes(acts: &[OutwardActCohort], now: DateTime<Utc>) -> Result<AwaitingFeed> {
    let mut unique: BTreeMap<&str, &OutwardActCohort> = BTreeMap::new();
    let mut order: Vec<&OutwardActCohort> = Vec::new();
    for entry in acts {
        match unique.get(entry.act.outward_act_ref.as_str()) {
            Some(held) if *held == entry => continue,
            Some(_) => anyhow::bail!(
                "act `{}` was supplied twice with different content: an identical repeat is one \
                 act, but a changed cohort key would put one act's outcome in two cohorts and \
                 make the comparison agree with whichever copy was read last",
                entry.act.outward_act_ref
            ),
            None => {
                unique.insert(entry.act.outward_act_ref.as_str(), entry);
                order.push(entry);
            },
        }
    }

    let mut feed = AwaitingFeed::default();
    for entry in order {
        let act_ref = entry.act.outward_act_ref.clone();
        let Some(delivery_state) = awaited_delivery_state(entry.act.status) else {
            let reason = if terminal(entry.act.status) {
                NotAwaiting::Settled {
                    status: entry.act.status,
                }
            } else {
                NotAwaiting::NothingLeft {
                    status: entry.act.status,
                }
            };
            feed.not_awaiting.push(NotAwaitingAct { act_ref, reason });
            continue;
        };

        if let Some(field) = empty_key(entry, &act_ref) {
            feed.not_awaiting.push(NotAwaitingAct {
                act_ref,
                reason: NotAwaiting::MissingKey { field },
            });
            continue;
        }
        if let Some(field) = separator_in_key(entry, &act_ref) {
            feed.not_awaiting.push(NotAwaitingAct {
                act_ref,
                reason: NotAwaiting::FieldSeparatorIn { field },
            });
            continue;
        }

        let Some(dispatched_at) = entry.act.dispatched_at.as_deref() else {
            feed.not_awaiting.push(NotAwaitingAct {
                act_ref,
                reason: NotAwaiting::UndatedDispatch,
            });
            continue;
        };
        let Ok(acted_at) = DateTime::parse_from_rfc3339(dispatched_at) else {
            feed.not_awaiting.push(NotAwaitingAct {
                act_ref,
                reason: NotAwaiting::UnreadableDispatchTime {
                    value: dispatched_at.to_string(),
                },
            });
            continue;
        };
        let acted_at = acted_at.with_timezone(&Utc);
        if acted_at > now {
            feed.not_awaiting.push(NotAwaitingAct {
                act_ref,
                reason: NotAwaiting::DispatchedInTheFuture {
                    dispatched_at: acted_at,
                },
            });
            continue;
        }

        feed.awaiting.push(AwaitingOutcome {
            act_ref,
            variant_ref: entry.variant_ref.clone(),
            variant_version: entry.variant_version.clone(),
            engagement_id: entry.act.engagement_id.clone(),
            program_id: entry.act.program_id.clone(),
            acted_at,
            delivery_state,
            confounders: entry.confounders.clone(),
        });
    }
    Ok(feed)
}

/// The delivery state an act's status supports, or `None` when the act is not
/// awaiting anything at all.
fn awaited_delivery_state(status: OutwardActStatus) -> Option<DeliveryState> {
    match status {
        // In flight. The clock starts when the act left, and the carrier has
        // said nothing yet — so it is an act performed, and not yet evidence.
        OutwardActStatus::Dispatching => Some(DeliveryState::Unknown),
        // Acceptance is not delivery, and the state name says so.
        OutwardActStatus::ProviderAccepted => Some(DeliveryState::Accepted),
        OutwardActStatus::Delivered => Some(DeliveryState::Delivered),
        // Neither evidence of a send nor of a non-send — held open as a
        // question rather than guessed either way.
        OutwardActStatus::DispatchUnknown => Some(DeliveryState::Unknown),
        // A correction succeeded the original, and the status it succeeded is
        // no longer readable from the act. It could have been accepted or
        // delivered, so the honest carry is unknown: unknown is never
        // permission to treat something as evidence about a counterparty.
        OutwardActStatus::Corrected => Some(DeliveryState::Unknown),
        OutwardActStatus::Prepared | OutwardActStatus::Failed | OutwardActStatus::Retracted => None,
    }
}

fn terminal(status: OutwardActStatus) -> bool {
    matches!(
        status,
        OutwardActStatus::Failed | OutwardActStatus::Retracted
    )
}

fn empty_key(entry: &OutwardActCohort, act_ref: &str) -> Option<&'static str> {
    if act_ref.trim().is_empty() {
        return Some("act_ref");
    }
    if entry.variant_ref.trim().is_empty() {
        return Some("variant_ref");
    }
    if entry.variant_version.trim().is_empty() {
        return Some("variant_version");
    }
    None
}

/// The three fields that reach a derived id.
///
/// The observation id is hashed from the scope, the act, the variant, the
/// version and the label joined by the separator, and the cohort index is a
/// line split on it. The engagement and program ids are carried on the
/// observation but reach neither, so they are not checked here — a guard wider
/// than its reason invites the reason to be forgotten.
fn separator_in_key(entry: &OutwardActCohort, act_ref: &str) -> Option<&'static str> {
    if carries_field_separator(act_ref) {
        return Some("act_ref");
    }
    if carries_field_separator(&entry.variant_ref) {
        return Some("variant_ref");
    }
    if carries_field_separator(&entry.variant_version) {
        return Some("variant_version");
    }
    None
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use crate::data_room::access_log::UserAgentClass;
    use crate::outcome_learning::aggregate::{document_market_read, never_opened_across};
    use crate::outcome_learning::maturity::{mature_silences, MaturityPolicy, NotMatured};
    use crate::outcome_learning::retirement::{
        contradictions, retirement_candidates, RetirementPolicy,
    };
    use crate::outcome_learning::store::{OutcomeScope, OutcomeStore};
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::audience::AudienceKind;
    use magician::magician_v2::evidence::outward_assertions::OutwardChannel;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn cohort(
        version: &str,
        usable: usize,
        counterparties: usize,
        engaged: usize,
    ) -> CohortSummary {
        let mut confounders = BTreeMap::new();
        let mut values = BTreeMap::new();
        values.insert("warm".to_string(), usable);
        confounders.insert("introduction".to_string(), values);
        CohortSummary {
            variant_ref: "outward-answer".to_string(),
            variant_version: version.to_string(),
            usable,
            unusable: 2,
            engaged,
            rejected: 0,
            silent: usable - engaged,
            counterparties,
            confounders,
        }
    }

    fn candidate(baseline: CohortSummary, contender: CohortSummary) -> Candidate {
        Candidate {
            variant_ref: "outward-answer".to_string(),
            baseline,
            candidate: contender,
            evidence_refs: vec!["obs-one".to_string(), "obs-two".to_string()],
            caveats: Vec::new(),
            observed_at: now(),
        }
    }

    fn target() -> LearningTarget {
        LearningTarget::new(LearningCandidateType::WorkflowTemplate)
            .in_scope("anonymous", "default")
    }

    fn access(room: &str, token: &str, document: Option<&str>, sequence: u32) -> AccessEvent {
        AccessEvent {
            room_id: room.to_string(),
            audience: AudienceRef::engagement("rel-1"),
            token_issued_to: token.to_string(),
            document_ref: document.map(str::to_string),
            occurred_at: now(),
            dwell_ms: None,
            sequence,
            user_agent_class: UserAgentClass::Unknown,
        }
    }

    fn share(tokens: &[&str], documents: &[&str]) -> RoomShare {
        RoomShare {
            room_id: "room-1".to_string(),
            audience: AudienceRef::engagement("rel-1"),
            shared_with: tokens.iter().map(|token| token.to_string()).collect(),
            documents: documents
                .iter()
                .map(|document| document.to_string())
                .collect(),
        }
    }

    fn assertion_use(
        use_id: &str,
        claim: &str,
        audience: &str,
        at: DateTime<Utc>,
        supersedes: &[&str],
    ) -> OutwardAssertionUse {
        OutwardAssertionUse {
            assertion_use_id: use_id.to_string(),
            outward_act_ref: format!("act-for-{use_id}"),
            approved_claim_ref: claim.to_string(),
            evidence_refs: Vec::new(),
            audience: audience.to_string(),
            supersedes: supersedes.iter().map(|id| id.to_string()).collect(),
            correction_ref: None,
            recorded_at: at.to_rfc3339(),
        }
    }

    fn disclosure(act_ref: &str, status: OutwardActStatus) -> OutwardActDisclosure {
        OutwardActDisclosure {
            outward_act_ref: act_ref.to_string(),
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            audience: None,
            program_id: Some("prog-1".to_string()),
            engagement_id: Some("rel-1".to_string()),
            exact_payload_artifact_ref: "artifact-1".to_string(),
            effective_sender: "sender".to_string(),
            intended_audience: vec!["someone".to_string()],
            channel: OutwardChannel::Email,
            consequence_class: "routine".to_string(),
            effect_receipt_ref: None,
            provider: None,
            provider_message_id: None,
            status,
            prepared_at: (now() - Duration::days(30)).to_rfc3339(),
            dispatched_at: Some((now() - Duration::days(10)).to_rfc3339()),
            settled_at: None,
            observed: false,
        }
    }

    fn act_cohort(act_ref: &str, status: OutwardActStatus) -> OutwardActCohort {
        OutwardActCohort {
            act: disclosure(act_ref, status),
            variant_ref: "outward-answer".to_string(),
            variant_version: "v1".to_string(),
            confounders: vec![Confounder::new("introduction", "warm")],
        }
    }

    // ── Empty input ─────────────────────────────────────────────────────────

    /// Every fold in this module runs over an empty log today, so a feeder that
    /// panicked on nothing would surface as a broken sweep rather than as no
    /// data — and the operator would be debugging the wrong thing.
    #[test]
    fn no_candidates_convert_to_nothing() {
        let feed = candidates_to_learning(&[], EvidenceFloor::default(), &target());
        assert_eq!(feed.proposed_count(), 0);
        assert_eq!(feed.withheld_count(), 0);
    }

    /// Same, for the market read: no rooms and no events is an empty feed, not
    /// a panic and not a room invented to hang the emptiness on.
    #[test]
    fn no_rooms_and_no_events_convert_to_nothing() {
        let feed = room_attention_from_access(&[], &[]).expect("empty input");
        assert_eq!(feed.rooms.len(), 0);
        assert_eq!(feed.unattributed.len(), 0);
    }

    /// Same, for the reverse index. An empty index is the normal state of a
    /// claim nobody has asserted yet.
    #[test]
    fn no_index_rows_convert_to_nothing() {
        let feed = claim_uses_from_index(&[]).expect("empty input");
        assert_eq!(feed.uses.len(), 0);
        assert_eq!(feed.unresolved.len(), 0);
        assert_eq!(feed.refused.len(), 0);
        assert!(feed.is_complete());
    }

    /// Same, for the maturity sweep: no acts is a sweep with nothing to do.
    #[test]
    fn no_acts_convert_to_nothing() {
        let feed = awaiting_outcomes(&[], now()).expect("empty input");
        assert_eq!(feed.awaiting_count(), 0);
        assert_eq!(feed.not_awaiting.len(), 0);
    }

    // ── 1. Candidates → learning ────────────────────────────────────────────

    /// A floor of zero is satisfied by an empty cohort — `0 >= 0` — so a
    /// comparison of nothing against nothing would arrive at the learning
    /// substrate as a finding. The floor must not be configurable into passing
    /// vacuously.
    #[test]
    fn an_evidence_floor_below_the_minimum_is_refused() {
        let error = EvidenceFloor::new(0, 0).expect_err("a floor of zero");
        assert!(error.to_string().contains("vacuously"), "{error}");

        let error = EvidenceFloor::new(MINIMUM_COHORT, 1).expect_err("one counterparty");
        assert!(
            error.to_string().contains("not about the variant"),
            "{error}"
        );

        let floor =
            EvidenceFloor::new(MINIMUM_COHORT, MINIMUM_COUNTERPARTIES).expect("the minimum");
        assert_eq!(floor.usable(), 5);
        assert_eq!(floor.counterparties(), 2);
    }

    /// The guardrail this conversion exists to hold. `Candidate`'s fields are
    /// public, so one can be built without ever passing the proposal refusal —
    /// and it must not be the path by which an under-powered comparison
    /// reaches a substrate that stores and surfaces it.
    #[test]
    fn a_candidate_below_the_floor_is_withheld_with_its_counts() {
        let feed = candidates_to_learning(
            &[candidate(cohort("v1", 3, 2, 1), cohort("v2", 9, 4, 6))],
            EvidenceFloor::default(),
            &target(),
        );
        assert_eq!(feed.proposed_count(), 0);
        assert_eq!(
            feed.withheld,
            vec![WithheldCandidate {
                variant_ref: "outward-answer".to_string(),
                reason: NotProposable::SampleTooSmall {
                    baseline: 3,
                    candidate: 9,
                    needed: 5,
                },
            }]
        );
    }

    /// One counterparty is a fact about that counterparty, not about the
    /// variant — and the refusal has to say which counts it measured, or an
    /// owner asking why the loop is quiet gets silence back.
    #[test]
    fn a_candidate_from_one_counterparty_is_withheld_with_its_counts() {
        let feed = candidates_to_learning(
            &[candidate(cohort("v1", 6, 1, 2), cohort("v2", 7, 3, 5))],
            EvidenceFloor::default(),
            &target(),
        );
        assert_eq!(
            feed.withheld,
            vec![WithheldCandidate {
                variant_ref: "outward-answer".to_string(),
                reason: NotProposable::TooFewCounterparties {
                    baseline: 1,
                    candidate: 3,
                    needed: 2,
                },
            }]
        );
    }

    /// A candidate citing nothing looks like a finding and cannot be audited
    /// into one. Its evidence list is empty, so every claim about it is
    /// vacuously checkable and none of it is actually checked.
    #[test]
    fn a_candidate_citing_no_evidence_is_withheld() {
        let mut uncited = candidate(cohort("v1", 6, 3, 2), cohort("v2", 7, 3, 5));
        uncited.evidence_refs.clear();
        let feed = candidates_to_learning(&[uncited], EvidenceFloor::default(), &target());
        assert_eq!(feed.proposed_count(), 0);
        assert_eq!(feed.withheld[0].reason, NotProposable::NoEvidenceCited);
    }

    /// The N and the confounders must survive the conversion. Without them the
    /// learning substrate holds an assertion that a variant did better, with
    /// nothing to weigh it against — and the introducer's effect is what gets
    /// attributed to the wording.
    #[test]
    fn a_converted_candidate_carries_its_n_and_its_confounders() {
        let feed = candidates_to_learning(
            &[candidate(cohort("v1", 6, 3, 2), cohort("v2", 7, 3, 5))],
            EvidenceFloor::default(),
            &target(),
        );
        assert_eq!(feed.proposed_count(), 1);
        let request = &feed.proposed[0];

        assert_eq!(request.proposed_change["baseline"]["usable"], json!(6));
        assert_eq!(request.proposed_change["candidate"]["usable"], json!(7));
        assert_eq!(
            request.proposed_change["baseline"]["counterparties"],
            json!(3)
        );
        assert_eq!(
            request.proposed_change["candidate"]["confounders"]["introduction"]["warm"],
            json!(7)
        );
        assert_eq!(request.summary, "engaged 2 of 6, then 5 of 7");
        assert_eq!(
            request.evidence_refs[0].id.as_deref(),
            Some("obs-one"),
            "the observation ids are the receipts"
        );
        assert_eq!(request.evidence_refs[1].id.as_deref(), Some("obs-two"));
        assert_eq!(request.evidence_refs[0].kind, "outcome_observation");
        assert_eq!(request.proposed_target.as_deref(), Some("outward-answer"));
    }

    /// `confidence` is the one field on the learning request that takes a
    /// rate. Two engagements out of six written as a number reads as precision
    /// that is not in the sample, and the owner comparing two such numbers is
    /// comparing four observations.
    #[test]
    fn a_converted_candidate_never_carries_a_rate() {
        let feed = candidates_to_learning(
            &[candidate(cohort("v1", 6, 3, 2), cohort("v2", 7, 3, 5))],
            EvidenceFloor::default(),
            &target(),
        );
        assert!(
            feed.proposed[0].confidence.is_none(),
            "counts travel; a rate is never computed from them"
        );
    }

    /// §8's first control, held structurally. The learning substrate's
    /// auto-apply lanes open for a candidate that is low risk and needs no
    /// review; a converted cohort comparison must never satisfy either half,
    /// or an owner editorial decision would be taken by a sweep.
    #[test]
    fn a_converted_candidate_can_never_reach_an_auto_apply_lane() {
        let feed = candidates_to_learning(
            &[candidate(cohort("v1", 6, 3, 2), cohort("v2", 7, 3, 5))],
            EvidenceFloor::default(),
            &target(),
        );
        let request = &feed.proposed[0];
        assert!(request.review_required);
        assert_eq!(request.risk_level, LearningRiskLevel::Medium);
        assert_eq!(request.state, LearningCandidateState::Proposed);
        assert_eq!(request.principal.as_deref(), Some("anonymous"));
        assert_eq!(
            request.candidate_type,
            LearningCandidateType::WorkflowTemplate
        );
    }

    /// Caveats are carried, not filtered. A confounder that differs without
    /// dominating is exactly what an owner needs to weigh, and dropping it
    /// hands them a cleaner story than the data supports.
    #[test]
    fn a_converted_candidate_keeps_its_caveats() {
        let mut with_caveat = candidate(cohort("v1", 6, 3, 2), cohort("v2", 7, 3, 5));
        with_caveat.caveats = vec!["stage differs between cohorts".to_string()];
        let feed = candidates_to_learning(&[with_caveat], EvidenceFloor::default(), &target());
        assert!(
            feed.proposed[0]
                .rationale
                .contains("stage differs between cohorts"),
            "{}",
            feed.proposed[0].rationale
        );
        assert_eq!(
            feed.proposed[0].proposed_change["caveats"],
            json!(["stage differs between cohorts"])
        );
    }

    // ── 2. Access events → room attention ───────────────────────────────────

    /// Visits come from the event's own sequence, never from how many events a
    /// token produced. One sitting that views the index and then opens a
    /// document is two events and one visit; counting events would report
    /// "came back to it" about somebody who came once and clicked twice.
    #[test]
    fn visits_come_from_sequence_not_from_the_event_count() {
        let feed = room_attention_from_access(
            &[share(&["token-a"], &["doc-1"])],
            &[
                access("room-1", "token-a", None, 1),
                access("room-1", "token-a", Some("doc-1"), 1),
            ],
        )
        .expect("one room");

        let attention = &feed.rooms[0].attentions[0];
        assert_eq!(attention.visits, 1, "one sitting is one visit");
        assert_eq!(attention.presentations, 2, "two page views happened");
        assert_eq!(attention.index_views, 1);
        assert_eq!(attention.documents_opened, vec!["doc-1".to_string()]);
    }

    /// A token shared and never seen must stay in the denominator. Dropping
    /// ghosts turns a room half of whose shares went silent into a room that
    /// was fully read, which inflates apparent engagement exactly where the
    /// honest signal is.
    #[test]
    fn a_token_that_never_appeared_stays_in_the_denominator() {
        let feed = room_attention_from_access(
            &[share(&["token-a", "token-ghost"], &["doc-1"])],
            &[access("room-1", "token-a", Some("doc-1"), 1)],
        )
        .expect("one room");

        let read = document_market_read(&feed.rooms);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].document, "doc-1");
        assert_eq!(read[0].opened_by, 1);
        assert_eq!(read[0].shared_with, 2, "the ghost is one of the two");
        assert_eq!(read[0].audiences, 1);

        assert_eq!(
            never_opened_across(&feed.rooms),
            vec![(AudienceRef::engagement("rel-1"), "token-ghost".to_string())],
            "the never-opened token is a delivery question, not a nudge"
        );
    }

    /// A room shared and never presented at all still produces a row. It is
    /// the strongest delivery signal available anywhere in the set, and a
    /// conversion that emitted nothing for it would delete it.
    #[test]
    fn a_room_nobody_visited_still_produces_its_share_list() {
        let feed = room_attention_from_access(&[share(&["token-a"], &["doc-1"])], &[])
            .expect("one silent room");
        assert_eq!(feed.rooms.len(), 1);
        assert_eq!(feed.rooms[0].attentions.len(), 1);
        assert_eq!(feed.rooms[0].attentions[0].visits, 0);
        assert_eq!(
            feed.rooms[0].attentions[0].documents_unopened,
            vec!["doc-1".to_string()]
        );
        assert_eq!(
            never_opened_across(&feed.rooms),
            vec![(AudienceRef::engagement("rel-1"), "token-a".to_string())]
        );
    }

    /// An event naming a token the share list does not hold means the list is
    /// stale. It is reported rather than counted: the share list is the
    /// authority on who was shared, and this must not claim a share it was
    /// never told about — but nor may it swallow the evidence that one exists.
    #[test]
    fn an_event_from_an_unlisted_token_is_reported_not_counted() {
        let feed = room_attention_from_access(
            &[share(&["token-a"], &["doc-1"])],
            &[access("room-1", "token-forwarded", Some("doc-1"), 1)],
        )
        .expect("one room");

        assert_eq!(
            feed.unattributed,
            vec![UnattributedAccess {
                room_id: "room-1".to_string(),
                token_issued_to: "token-forwarded".to_string(),
                reason: NotAttributed::TokenNotShared,
            }]
        );
        assert_eq!(feed.rooms[0].attentions.len(), 1);
        assert_eq!(feed.rooms[0].attentions[0].token_issued_to, "token-a");
        assert_eq!(feed.rooms[0].attentions[0].visits, 0);
    }

    /// An event for a room whose share list was not supplied cannot be counted
    /// honestly — the denominator is unknown — and silently discarding it
    /// would report a market read that is quietly missing a room.
    #[test]
    fn an_event_for_an_unsupplied_room_is_reported_not_dropped() {
        let feed = room_attention_from_access(
            &[share(&["token-a"], &["doc-1"])],
            &[access("room-other", "token-a", Some("doc-1"), 1)],
        )
        .expect("one room");

        assert_eq!(feed.unattributed[0].reason, NotAttributed::UnknownRoom);
        assert_eq!(feed.unattributed[0].room_id, "room-other");
        assert_eq!(feed.rooms[0].attentions[0].visits, 0);
    }

    /// The kind is part of an audience's identity, so a room serving one
    /// relationship kind and an event claiming another are two different
    /// audiences. Folding the event in would file a counterparty's attention
    /// under somebody else's name and overstate the market's breadth.
    #[test]
    fn an_event_naming_a_different_audience_is_not_folded_in() {
        let mut mismatched = access("room-1", "token-a", Some("doc-1"), 1);
        mismatched.audience = AudienceRef::new(AudienceKind::Account, "rel-1");

        let feed = room_attention_from_access(&[share(&["token-a"], &["doc-1"])], &[mismatched])
            .expect("one room");

        assert_eq!(
            feed.unattributed[0].reason,
            NotAttributed::AudienceMismatch {
                on_event: "account:rel-1".to_string(),
                on_room: "engagement:rel-1".to_string(),
            }
        );
        assert_eq!(feed.rooms[0].attentions[0].visits, 0);
        assert_eq!(document_market_read(&feed.rooms)[0].opened_by, 0);
    }

    /// A room supplied twice with different share lists means the caller
    /// assembled its input from records that disagree. Resolving it to
    /// whichever copy came last would silently move who counts as shared,
    /// which is the denominator of every number downstream.
    #[test]
    fn a_room_supplied_twice_resumes_only_when_it_is_identical() {
        let repeated = room_attention_from_access(
            &[
                share(&["token-a"], &["doc-1"]),
                share(&["token-a"], &["doc-1"]),
            ],
            &[],
        )
        .expect("an identical repeat");
        assert_eq!(repeated.rooms.len(), 1, "an identical repeat is one room");

        let error = room_attention_from_access(
            &[
                share(&["token-a"], &["doc-1"]),
                share(&["token-a", "token-b"], &["doc-1"]),
            ],
            &[],
        )
        .expect_err("a changed share list");
        assert!(
            error.to_string().contains("who counts as shared"),
            "{error}"
        );
    }

    // ── 3. Reverse index → claim uses ───────────────────────────────────────

    /// The translation that is the whole job. The index records supersession
    /// between assertion USES; retirement reasons about CLAIMS. Passing the
    /// use ids through unchanged compiles and is silently inert — nothing ever
    /// matches, so no claim is recognised as replaced and no contradiction is
    /// ever found.
    #[test]
    fn a_supersession_is_translated_from_use_ids_to_claim_refs() {
        let old = now() - Duration::days(200);
        let correction = now() - Duration::days(100);
        let relapse = now() - Duration::days(10);

        let feed = claim_uses_from_index(&[
            assertion_use("use-1", "claim-old", "aud-1", old, &[]),
            assertion_use("use-2", "claim-new", "aud-2", correction, &["use-1"]),
            assertion_use("use-3", "claim-old", "aud-3", relapse, &[]),
        ])
        .expect("three rows");

        assert_eq!(
            feed.uses[1].supersedes,
            vec!["claim-old".to_string()],
            "the use id resolved to the claim it carried"
        );
        assert!(feed.is_complete());

        let found = contradictions(&feed.uses, now());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].stale_claim, "claim-old");
        assert_eq!(found[0].superseding_claim, "claim-new");
        assert_eq!(found[0].superseded_at, correction);
        assert_eq!(found[0].offending_uses.len(), 1);
        assert_eq!(found[0].offending_uses[0].use_ref, "use-3");
        assert_eq!(found[0].offending_uses[0].audience, "aud-3");
        assert_eq!(found[0].audiences_told_stale, 1);
    }

    /// A supersession pointing outside the supplied rows must be reported. The
    /// consequence of dropping it is silent: the replaced claim comes back as
    /// merely idle, and a stale claim asserted after its correction existed
    /// raises no contradiction at all.
    #[test]
    fn an_unresolvable_supersession_is_reported_not_dropped() {
        let feed = claim_uses_from_index(&[assertion_use(
            "use-2",
            "claim-new",
            "aud-1",
            now() - Duration::days(100),
            &["use-elsewhere"],
        )])
        .expect("one row");

        assert_eq!(
            feed.unresolved,
            vec![UnresolvedSupersession {
                use_ref: "use-2".to_string(),
                superseded_use_ref: "use-elsewhere".to_string(),
            }]
        );
        assert!(!feed.is_complete(), "the feed knows it is partial");
        assert_eq!(feed.uses[0].supersedes, Vec::<String>::new());
    }

    /// A row whose timestamp cannot be read must not be dated by guess. Dating
    /// it now would reset the claim's idleness and suppress a staleness
    /// finding that is true today; dating it from the beginning of time would
    /// nominate the claim immediately.
    #[test]
    fn a_row_with_an_unreadable_timestamp_is_refused_not_dated() {
        let mut broken = assertion_use("use-1", "claim-old", "aud-1", now(), &[]);
        broken.recorded_at = "last tuesday".to_string();

        let feed = claim_uses_from_index(&[
            broken,
            assertion_use(
                "use-2",
                "claim-other",
                "aud-1",
                now() - Duration::days(400),
                &[],
            ),
        ])
        .expect("one readable row");

        assert_eq!(
            feed.refused,
            vec![RefusedClaimUse {
                use_ref: "use-1".to_string(),
                reason: NotConvertible::UnreadableTimestamp {
                    value: "last tuesday".to_string(),
                },
            }]
        );
        assert_eq!(feed.uses.len(), 1, "the readable row still converts");
        assert_eq!(feed.uses[0].claim_ref, "claim-other");
    }

    /// Index files are append-only and interleaved, so a caller merging axes
    /// sees the same row twice. An identical repeat is one use — doubling it
    /// would double a claim's N — and a changed one means the records
    /// disagree about what was said, which is an error rather than a pick.
    #[test]
    fn an_identical_index_replay_resumes_and_a_changed_one_is_an_error() {
        let row = assertion_use(
            "use-1",
            "claim-old",
            "aud-1",
            now() - Duration::days(400),
            &[],
        );
        let feed = claim_uses_from_index(&[row.clone(), row.clone()]).expect("identical replay");
        assert_eq!(feed.uses.len(), 1);

        let policy = RetirementPolicy::new(Duration::days(180)).expect("a positive window");
        let stale = retirement_candidates(&feed.uses, &policy, now());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].total_uses, 1, "a replay must not inflate the N");
        assert_eq!(stale[0].evidence_refs, vec!["use-1".to_string()]);

        let mut changed = row.clone();
        changed.audience = "aud-2".to_string();
        let error = claim_uses_from_index(&[row, changed]).expect_err("a changed payload");
        assert!(error.to_string().contains("disagree"), "{error}");
    }

    // ── 4. Outward acts → awaiting outcomes ─────────────────────────────────

    /// An act that never left has nobody to be silent. Maturing it would
    /// record that a counterparty ignored a message that was never sent, which
    /// is the most misleading row this loop can produce.
    #[test]
    fn an_act_that_never_left_is_not_awaiting_an_outcome() {
        let feed = awaiting_outcomes(&[act_cohort("act-1", OutwardActStatus::Prepared)], now())
            .expect("one act");
        assert_eq!(feed.awaiting_count(), 0);
        assert_eq!(
            feed.not_awaiting,
            vec![NotAwaitingAct {
                act_ref: "act-1".to_string(),
                reason: NotAwaiting::NothingLeft {
                    status: OutwardActStatus::Prepared,
                },
            }]
        );
    }

    /// A settled act does not come back. A failed act told nobody anything and
    /// a retracted one is finished, so admitting either to the awaiting set
    /// would resurrect a terminal state as a pending one.
    #[test]
    fn a_terminal_act_never_re_enters_the_awaiting_set() {
        let feed = awaiting_outcomes(
            &[
                act_cohort("act-failed", OutwardActStatus::Failed),
                act_cohort("act-retracted", OutwardActStatus::Retracted),
            ],
            now(),
        )
        .expect("two acts");

        assert_eq!(feed.awaiting_count(), 0);
        assert_eq!(
            feed.not_awaiting,
            vec![
                NotAwaitingAct {
                    act_ref: "act-failed".to_string(),
                    reason: NotAwaiting::Settled {
                        status: OutwardActStatus::Failed,
                    },
                },
                NotAwaitingAct {
                    act_ref: "act-retracted".to_string(),
                    reason: NotAwaiting::Settled {
                        status: OutwardActStatus::Retracted,
                    },
                },
            ]
        );
    }

    /// Delivery is carried from the act's own state, never inferred. A
    /// provider's acceptance is not delivery, and a corrected act's earlier
    /// state is no longer readable — so it is carried as unknown, because
    /// unknown is never permission to treat something as evidence about a
    /// counterparty.
    #[test]
    fn delivery_state_is_carried_from_the_act_never_guessed() {
        let feed = awaiting_outcomes(
            &[
                act_cohort("act-accepted", OutwardActStatus::ProviderAccepted),
                act_cohort("act-delivered", OutwardActStatus::Delivered),
                act_cohort("act-unknown", OutwardActStatus::DispatchUnknown),
                act_cohort("act-corrected", OutwardActStatus::Corrected),
                act_cohort("act-inflight", OutwardActStatus::Dispatching),
            ],
            now(),
        )
        .expect("five acts");

        let states: Vec<(String, DeliveryState)> = feed
            .awaiting
            .iter()
            .map(|awaiting| (awaiting.act_ref.clone(), awaiting.delivery_state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("act-accepted".to_string(), DeliveryState::Accepted),
                ("act-delivered".to_string(), DeliveryState::Delivered),
                ("act-unknown".to_string(), DeliveryState::Unknown),
                ("act-corrected".to_string(), DeliveryState::Unknown),
                ("act-inflight".to_string(), DeliveryState::Unknown),
            ]
        );
        assert!(
            !DeliveryState::Unknown.reached_someone(),
            "an act carried as unknown is an act performed, never evidence about a counterparty"
        );
    }

    /// An act nothing dated must not be dated from preparation. Preparation is
    /// strictly earlier than dispatch, so a window measured from it closes
    /// early and records a decision the counterparty never had the chance to
    /// make.
    #[test]
    fn an_undated_dispatch_is_refused_rather_than_dated_from_preparation() {
        let mut undated = act_cohort("act-1", OutwardActStatus::Delivered);
        undated.act.dispatched_at = None;

        let feed = awaiting_outcomes(&[undated], now()).expect("one act");
        assert_eq!(feed.awaiting_count(), 0);
        assert_eq!(
            feed.not_awaiting[0].reason,
            NotAwaiting::UndatedDispatch,
            "preparation time is earlier than the act and must not stand in for it"
        );

        let mut unreadable = act_cohort("act-2", OutwardActStatus::Delivered);
        unreadable.act.dispatched_at = Some("yesterday".to_string());
        let feed = awaiting_outcomes(&[unreadable], now()).expect("one act");
        assert_eq!(
            feed.not_awaiting[0].reason,
            NotAwaiting::UnreadableDispatchTime {
                value: "yesterday".to_string(),
            }
        );
    }

    /// The knowable boundary is inclusive: an act dispatched at this very
    /// instant has been dispatched. Only one dated strictly after `now` is
    /// excluded, or its window would close before a window opened earlier.
    #[test]
    fn the_dispatch_boundary_is_inclusive_and_the_future_is_excluded() {
        let mut exactly_now = act_cohort("act-now", OutwardActStatus::Delivered);
        exactly_now.act.dispatched_at = Some(now().to_rfc3339());
        let feed = awaiting_outcomes(&[exactly_now], now()).expect("one act");
        assert_eq!(feed.awaiting_count(), 1);
        assert_eq!(feed.awaiting[0].acted_at, now());

        let mut later = act_cohort("act-later", OutwardActStatus::Delivered);
        let ahead = now() + Duration::seconds(1);
        later.act.dispatched_at = Some(ahead.to_rfc3339());
        let feed = awaiting_outcomes(&[later], now()).expect("one act");
        assert_eq!(feed.awaiting_count(), 0);
        assert_eq!(
            feed.not_awaiting[0].reason,
            NotAwaiting::DispatchedInTheFuture {
                dispatched_at: ahead,
            }
        );
    }

    /// The observation id is hashed from these fields joined by the separator,
    /// and the cohort index is a line split on it. A value carrying the
    /// separator can move the boundary between two fields — two different acts
    /// hashing to one observation — and truncates the act ref in the index, so
    /// the row it points at can never be read back.
    #[test]
    fn a_key_carrying_the_field_separator_is_refused() {
        let mut in_act_ref = act_cohort("act-1", OutwardActStatus::Delivered);
        in_act_ref.act.outward_act_ref = "act\u{1f}1".to_string();
        let mut in_variant_ref = act_cohort("act-2", OutwardActStatus::Delivered);
        in_variant_ref.variant_ref = "outward\u{1f}answer".to_string();
        let mut in_version = act_cohort("act-3", OutwardActStatus::Delivered);
        in_version.variant_version = "v\u{1f}1".to_string();

        let feed = awaiting_outcomes(&[in_act_ref, in_variant_ref, in_version], now())
            .expect("three acts");
        assert_eq!(
            feed.awaiting_count(),
            0,
            "none of the three may be admitted"
        );
        assert_eq!(
            feed.not_awaiting
                .iter()
                .map(|refused| refused.reason.clone())
                .collect::<Vec<NotAwaiting>>(),
            vec![
                NotAwaiting::FieldSeparatorIn { field: "act_ref" },
                NotAwaiting::FieldSeparatorIn {
                    field: "variant_ref",
                },
                NotAwaiting::FieldSeparatorIn {
                    field: "variant_version",
                },
            ]
        );

        // The engagement id is carried on the observation and reaches no
        // derived id, so it is not checked: a guard wider than its reason
        // invites the reason to be forgotten.
        let mut carried = act_cohort("act-4", OutwardActStatus::Delivered);
        carried.act.engagement_id = Some("rel\u{1f}1".to_string());
        let feed = awaiting_outcomes(&[carried], now()).expect("one act");
        assert_eq!(feed.awaiting_count(), 1);
        assert_eq!(
            feed.awaiting[0].engagement_id.as_deref(),
            Some("rel\u{1f}1")
        );
    }

    /// The cohort key is not optional: without it an outcome cannot be
    /// compared against anything, so it can only ever confirm what is already
    /// believed. Refused here rather than at the store, so a sweep says which
    /// act it could not place.
    #[test]
    fn an_act_with_no_cohort_key_is_refused() {
        let mut blank = act_cohort("act-1", OutwardActStatus::Delivered);
        blank.variant_version = "  ".to_string();
        let feed = awaiting_outcomes(&[blank], now()).expect("one act");
        assert_eq!(
            feed.not_awaiting[0].reason,
            NotAwaiting::MissingKey {
                field: "variant_version",
            }
        );
    }

    /// An act supplied twice under two cohort keys would put one outcome in
    /// two cohorts, and the comparison would then agree with whichever copy
    /// was read last. An identical repeat is one act.
    #[test]
    fn an_act_supplied_twice_resumes_only_when_it_is_identical() {
        let entry = act_cohort("act-1", OutwardActStatus::Delivered);
        let feed = awaiting_outcomes(&[entry.clone(), entry.clone()], now()).expect("a repeat");
        assert_eq!(feed.awaiting_count(), 1);

        let mut changed = entry.clone();
        changed.variant_version = "v2".to_string();
        let error = awaiting_outcomes(&[entry, changed], now()).expect_err("a changed key");
        assert!(error.to_string().contains("two cohorts"), "{error}");
    }

    /// The feeder must not pre-judge maturity. Deciding it here would apply a
    /// second, undocumented window beside the policy's, and an act inside its
    /// window would vanish from the sweep instead of being reported as still
    /// open.
    #[test]
    fn the_feeder_does_not_decide_maturity_the_policy_does() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = OutcomeStore::new(ArtifactV2Workspace::new(tmp.path()));
        let scope = OutcomeScope::new("anonymous", "default");
        let policy = MaturityPolicy::new(Duration::days(7)).expect("a positive window");

        let mut fresh = act_cohort("act-fresh", OutwardActStatus::Delivered);
        fresh.act.dispatched_at = Some((now() - Duration::hours(1)).to_rfc3339());
        // `act_cohort` dispatches ten days ago, so this one is past its window.
        let ripe = act_cohort("act-ripe", OutwardActStatus::Delivered);

        let feed = awaiting_outcomes(&[fresh, ripe], now()).expect("two acts");
        assert_eq!(
            feed.awaiting_count(),
            2,
            "both are assembled; the policy decides which has ripened"
        );

        let report = mature_silences(&store, &scope, &feed.awaiting, &policy, now())
            .expect("the sweep runs");
        assert_eq!(report.matured, vec!["act-ripe".to_string()]);
        assert_eq!(report.matured_count(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "act-fresh");
        assert!(matches!(report.skipped[0].1, NotMatured::StillOpen { .. }));

        let recorded = store
            .observations_for_act(&scope, "act-ripe")
            .expect("the act's observations");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].variant_version, "v1");
        assert_eq!(recorded[0].engagement_id.as_deref(), Some("rel-1"));
        assert_eq!(
            recorded[0].confounders,
            vec![Confounder::new("introduction", "warm")],
            "the confounders travel with the silence"
        );
    }
}
