//! Learning what actually worked — OPC outcome learning, plan phases 1-5.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md`.
//! Doc: `docs/components/magician/outcome-learning.md`.
//!
//! **Written: all five phases** — recording, the maturity policy that decides
//! when silence becomes a fact, the cohort comparison that turns those
//! observations into candidates the owner decides on, the cross-audience
//! aggregation behind a market read, and retirement when a claim is
//! contradicted.
//! Still no scoring and no automatic adoption: a candidate is a thing the owner
//! decides, which is the plan's own sequencing:
//! *"Purely additive… Do this early even if the rest waits — the data cannot be
//! reconstructed afterwards."*
//!
//! # The one producer, and what now starts it
//!
//! [`sweep`] assembles the acts that went quiet from the outward-assertions
//! record and hands them to [`maturity::mature_silences`]; [`worker`] wraps
//! that in a tick loop; and [`book`] is the part that was missing —
//! [`book::WorkspaceMaturityBook`] enumerates the scopes and the acts,
//! [`magician::config::OutcomeMaturityConfig`] is the configuration key, and
//! [`book::spawn_configured_maturity_sweep`] is what a boot path calls. Until
//! all three existed no tick had ever run, and `silent` had no producer at all.
//!
//! **It ships off.** The sweep records a judgement — that somebody who has not
//! answered inside a chosen window has decided — into a store that cannot
//! un-write it, so an operator names the scopes, the window and the cohorts
//! before anything is recorded.
//!
//! # What is still not produced, and what that costs
//!
//! No other label has a producer: nothing records `replied`, `accepted`,
//! `rejected`, `opened` or `progressed` outside this module's own tests. The
//! survivorship bias runs in **both** directions and neither is safe alone —
//! a store that only ever receives replies is indistinguishable from a world
//! where everybody answers, and a store that only ever receives silences says
//! nothing came back from anybody. The sweep closes the first; the second is
//! open until the reply side of a channel writes.
//!
//! The module is deliberately ahead of its inputs — recording cannot be
//! back-filled.
//!
//! # The four adapters, and what now calls each
//!
//! [`feeders`] turns records other subsystems keep into the shapes the phases
//! consume, and every one of its conversions was complete with no caller — so
//! each ran over an empty collection while its own tests passed. [`composition`]
//! is the caller for three of them, and the maturity worker above is the caller
//! for the fourth:
//!
//! - [`feeders::candidates_to_learning`] → filed as a learning candidate by
//!   [`composition::worker::OutcomeProposalWorker::spawn`];
//! - [`feeders::room_attention_from_access`] → the market read, answered by
//!   `GET /outcome-learning/market-read`;
//! - [`feeders::claim_uses_from_index`] → stale and contradicted claims,
//!   answered by `POST /outcome-learning/claim-health`;
//! - [`feeders::awaiting_outcomes`] → already called by
//!   [`sweep::sweep_matured_silences`], which [`book`] and [`worker`] run.
//!
//! The proposal pass **refuses to propose** for a tenant no maturity sweep
//! covers, carrying the sample sizes it would have used. A cohort read out of a
//! store that only ever receives replies is not evidence about a variant, and
//! the counts are what tell an owner which switch is off.
//!
//! # Generic, not a fundraising feature
//!
//! An observation is *"this variant of this act produced this result"*, which is
//! the same shape whether the act was a pitch, a support reply, a proposal or a
//! scheduling message. Nothing in this module names a domain, and the confounder
//! type is deliberately open (`kind`/`value`) because the confounders that
//! matter are discovered late — a closed set would force the interesting ones to
//! be recorded as nothing at all.
//!
//! # The guardrail comes before the loop
//!
//! §2 of the plan, and it is why phase 1 is not just a table:
//!
//! - **Silence is refused before its window closes.** Before maturity, silence
//!   is indistinguishable from "not yet".
//! - **The cohort key is not optional.** Comparing outcomes across proposal
//!   versions is what separates learning from self-confirmation.
//! - **Delivery is tracked apart from response.** A bounce that reads as
//!   `silent` would count as a counterparty ignoring us when nothing arrived.
//! - **The owner is not observed.** There is no field for the operator; the
//!   subject is always an act and a counterparty.

pub mod aggregate;
// Where a tick's work comes from, and the one function that starts the sweep.
// Without it `MaturityBook` had no implementor and `MaturityWorker::spawn` had
// no caller, so the only producer of `silent` observations had never run.
pub mod book;
// The callers `feeders` never had: cohort comparisons filed as decisions an
// owner makes, rooms and their access lane folded into the market read, and the
// assertion index read for stale and contradicted claims. Every conversion in
// `feeders` was complete and reached from nothing, so each ran over an empty
// collection. It lives inside this subsystem rather than beside it because
// `feeders` already imports all four sides and none of them imports this — so a
// coordinator here adds no dependency edge, and a sibling module would add four.
pub mod composition;
pub mod feeders;
pub mod maturity;
pub mod proposal;
pub mod retirement;
pub mod store;
// The caller phase 2 was missing: assembling the acts that went quiet and
// recording their silence. Without it `mature_silences` is a decision nobody
// invokes, and the sample only ever contains the counterparties who replied.
pub mod sweep;
pub mod types;
// A tick loop around the sweep. Started from `book::spawn_configured_maturity_sweep`,
// and only when configuration says so: it writes observations that become
// evidence and they cannot be un-recorded, so the switch defaults to off.
pub mod worker;

#[cfg(test)]
mod tests;

pub use aggregate::{
    document_market_read, market_confidence_floor, never_opened_across, question_frequency,
    DocumentRead, QuestionCount, RoomAttention,
};
pub use book::{
    spawn_configured_maturity_sweep, ActCohortSource, BookScope, DeclaredPayloadVariants,
    WorkspaceMaturityBook,
};
pub use composition::api::{configure_outcome_learning_routes, OutcomeLearningSurface};
pub use composition::worker::{
    OutcomeProposalConfig, OutcomeProposalHealth, OutcomeProposalHealthSnapshot,
    OutcomeProposalWorker,
};
pub use composition::{
    candidate_id_for, claim_health, comparisons_in_scope, market_read,
    sweep_candidates_into_learning, sweep_scope, CandidateSweep, ClaimHealth, CohortComparison,
    ComparisonWithheld, MarketDocument, MarketRead, MaturitySweepStanding, WithheldComparison,
};
pub use feeders::{
    awaiting_outcomes, candidates_to_learning, claim_uses_from_index, room_attention_from_access,
    AwaitingFeed, ClaimUseFeed, EvidenceFloor, LearningFeed, LearningTarget, NotAttributed,
    NotAwaiting, NotAwaitingAct, NotConvertible, OutwardActCohort, RefusedClaimUse,
    RoomAttentionFeed, RoomShare, UnattributedAccess, UnresolvedSupersession, WithheldCandidate,
};
pub use maturity::{
    confounder_kind, mature_silences, AwaitingOutcome, MaturationReport, MaturityPolicy, NotMatured,
};
pub use proposal::{
    propose, summarise_cohort, Candidate, CohortSummary, NotProposable, MINIMUM_COHORT,
    MINIMUM_COUNTERPARTIES,
};
pub use retirement::{
    contradictions, retirement_candidates, ClaimUse, Contradiction, RetirementCandidate,
    RetirementPolicy,
};
pub use store::{supersedes_silence, OutcomeScope, OutcomeStore, RecordedCohort};
pub use sweep::{
    act_refs_for_engagement, act_refs_for_work, collect_acts, run_maturity_sweep,
    sweep_matured_silences, ActCohortBinding, CollectedActs, MaturitySweepReport, NotCollected,
    UncollectedAct,
};
pub use types::{Confounder, DeliveryState, OutcomeLabel, OutcomeObservation, RecordOutcome};
pub use worker::{
    MaturityBook, MaturityScope, MaturitySweepConfig, MaturityWorker, MaturityWorkerHealth,
    MaturityWorkerHealthSnapshot,
};
