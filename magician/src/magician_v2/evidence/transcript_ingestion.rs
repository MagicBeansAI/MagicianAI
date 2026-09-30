//! Ingesting an observed transcript, and putting its claims in front of a human.
//!
//! [`super::observed_statements`] is the writer for a channel nobody could
//! prepare, and it is careful about exactly the right thing: it records the act
//! unconditionally and hands an unconfirmed extraction back as a
//! [`PendingClaim`](super::observed_statements::PendingClaim) instead of
//! asserting it. Nothing called it. No transcript was ever ingested and no
//! surface ever showed a pending claim, so
//! *"unconfirmed extractions are surfaced rather than dropped"* was a property
//! of a module nobody exercised — true in the way an empty set makes every
//! statement about its members true.
//!
//! This module is that caller. Since 2026-08-21 it also has a surface a person
//! can reach: `magician-api/src/transcript_claims_api.rs` is the only place in
//! the workspace that calls the authoritative ingest transition, and
//! `one_owner_surface_ingests_a_transcript` below pins that it stays the only
//! one. A read-only binder or a destination seam may construct a
//! [`TranscriptIngestion`] — the boundary is the ingest call, not the value.
//!
//! Controlled Envoy channel replies use `queue_controlled_reply` after the
//! host prepares an exact payload. They share this pending register without
//! going through observed-transcript ingestion. Channel acceptance is required
//! before a human can confirm one as said; preparation alone is not delivery.
//!
//! The transcript route is **owner-facing rather than a hook on the meeting sink**, and
//! that is a decision rather than a shortcut. Which rooms are *outward* is a
//! judgement about the relationship: the sink knows a meeting happened, not
//! that the people in it were a counterparty, and ingesting every internal
//! standup as an outward disclosure would fill the register with acts nobody
//! owes a correction for. A later automatic feeder is one more caller of this
//! store, not a change here.
//!
//! The shape it provides:
//!
//! ```text
//! utterances → one act (always) → candidates → pending register → owner
//!                                                    ↓ confirm        ↓ reject
//!                                            assertion rows        terminal
//!                                                    ↓
//!                                        commitment (still unconfirmed)
//! ```
//!
//! # Generic by construction
//!
//! A "transcript" here is any observed channel where the words leave before the
//! runtime can record them — a meeting, a phone call, a voice note, an in-person
//! conversation somebody typed up afterwards. The only channel judgement lives
//! in [`OutwardChannel::is_controlled`], which the assertions store already owns,
//! so there is no channel list here to drift from it. Nothing in this file knows
//! what a round, an investor or a term sheet is; a fundraising flow is one
//! caller among whatever else needs *"what did we actually say in that room"*.
//!
//! # Extraction is structural, and deliberately over-surfaces
//!
//! Producing candidates here is not a model call. The rule is mechanical:
//!
//! - the utterance is attributed to **one of our own identities**, as the
//!   runtime resolved it — never as the transcript labelled it, and
//! - it has words in it.
//!
//! Everything else is skipped **with a reason**, counted and returned. An
//! utterance the runtime could not attribute produces nothing, because the
//! failure mode worth engineering against is a claim recorded as ours that we
//! never made — [`SpeakerAttribution::Unresolved`] is not permission, it is the
//! absence of an answer.
//!
//! The other direction — surfacing an ordinary sentence as a candidate — costs
//! the owner one rejection. That asymmetry is why the structural rule is loose
//! and the assertion rule is strict.
//!
//! # Confirmation is somebody else's act
//!
//! [`Commitments`](crate::magician_v2::commitments::Commitments) makes the rule
//! this module matches: a machine may only record the unconfirmed state, there
//! is no parameter that lets a caller hand in a confirmed one, and confirming
//! requires a **named** person because *"an unnamed confirmation is how an agent
//! would grant itself the one control this register has"*. Here the same rule
//! goes one step further, because here we know who produced the candidate:
//! whoever extracted a claim may not confirm it. A confirmation from the
//! extractor is the extractor marking its own homework, which is precisely the
//! self-grant the named-confirmer rule exists to prevent.
//!
//! # A second door, and why it is not a second authority
//!
//! [`StagedIngestConsumer`] applies an evidence-package `ingest_request` row —
//! a transcript somebody pasted into the claims-review console, mapped by hand
//! to named speakers. It is a *reader of staged rows*, not a second ingestion
//! path: it refuses everything the canonical entry could not check for itself
//! and then calls that entry, so the header's one-owner rule above still holds
//! and `one_owner_surface_ingests_a_transcript` still pins it.
//!
//! What it must never do is let the package supply attribution. The document
//! maps a speaker key to a named person, which is a person's typed intent; the
//! *side* that person is on comes from a host-resolved
//! [`StagedIngestRoster`], because [`SpeakerAttribution::Ours`] is the only
//! attribution that becomes an outward assertion, and a document that could
//! declare its own sides would let an installed app put words in our mouths.
//! A name the host cannot place refuses the request outright rather than
//! becoming [`SpeakerAttribution::Unresolved`] — there is no diariser here that
//! could have been unsure.
//!
//! Its one destination is
//! `magician/src/magician_v2/claims_decision_contribution.rs`, reached by the
//! owner route `POST /apps/installations/{id}/claims-ingests/apply`, and
//! `one_host_destination_applies_a_staged_ingest` pins that it stays the only
//! one. That route is where the roster comes from — an authenticated owner
//! session resolving named people to sides, never the staged row.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::agents::ConsequenceClass;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{AudienceKind, AudienceRef};
use crate::magician_v2::commitments::{
    Commitment, CommitmentDecisionOutcome, CommitmentDirection, CommitmentScope, Commitments,
    RecordCommitment,
};
use crate::magician_v2::execution::file_edit::transaction::acquire_record_decision_lock;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

use super::completion_journal::{
    EvidenceCompletionJournal, EvidenceDecisionCompletion, EvidenceDecisionScope,
    EvidenceDecisionTarget,
};
use super::observed_statements::{
    record_observed_statement, ClaimConfirmation, ExtractedClaim, ObservedStatement,
};
use super::outward_assertions::{
    OutwardActDisclosure, OutwardAssertionStore, OutwardChannel, OutwardScope,
};
use super::store_cursor::{StoreFoldCursor, StoreFoldIndex};

/// Field separator for derived ids. A unit separator cannot appear in a key, a
/// claim ref or an address, so `(a, b)` and `(ab, "")` cannot collide into one
/// id — which is why every caller string that feeds a derivation is refused when
/// it carries one.
const FIELD_SEP: char = '\u{1f}';

/// Names this register in a cancelled-fold refusal.
const TRANSCRIPT_CLAIM_REGISTER: &str = "transcript claim register";

/// Who the runtime resolved a speaker to.
///
/// Deliberately three states rather than an `Option<String>` with a flag beside
/// it: *"we could not tell who said this"* and *"the other side said this"* lead
/// to different records and neither may be silently read as *"we said this"*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerAttribution {
    /// Resolved to one of our own identities. The only case that can become an
    /// outward assertion, because an outward assertion answers *"what did WE
    /// tell whom"*.
    Ours(String),
    /// Resolved to somebody on the other side. On the record inside the
    /// transcript payload, never as something we asserted.
    Counterparty(String),
    /// The transcript's speaker label could not be resolved to a known identity.
    ///
    /// **Not permission.** A transcript's own labels are a model's guess, and a
    /// candidate built on one would be a claim attributed to us on the strength
    /// of a diarisation error.
    Unresolved,
}

impl SpeakerAttribution {
    /// The resolved identity, when there is one.
    pub fn identity(&self) -> Option<&str> {
        match self {
            Self::Ours(who) | Self::Counterparty(who) => Some(who.as_str()),
            Self::Unresolved => None,
        }
    }
}

/// Why an utterance produced no candidate claim.
///
/// Returned rather than dropped: *"nothing was extractable"* and *"forty things
/// were dropped because the diariser failed"* are different situations and a
/// caller has to be able to tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The runtime could not resolve who said it.
    SpeakerUnresolved,
    /// The other side said it, so it is not something we asserted.
    SpokenByCounterparty,
    /// No words in it.
    NoWords,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpeakerUnresolved => "speaker_unresolved",
            Self::SpokenByCounterparty => "spoken_by_counterparty",
            Self::NoWords => "no_words",
        }
    }
}

/// One utterance that produced nothing, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedUtterance {
    pub segment_key: String,
    pub reason: SkipReason,
}

/// One thing somebody said, as the runtime knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptUtterance {
    /// Stable key for this segment within the transcript. The candidate claim
    /// ref derives from it, so re-processing the same transcript resolves to the
    /// same claim instead of surfacing it to the owner a second time.
    pub segment_key: String,
    pub speaker: SpeakerAttribution,
    pub spoken_text: String,
    /// An approved claim this utterance refers to, when the caller has a
    /// catalogue to name one from. `None` derives a ref for the segment, which
    /// is the honest shape for *"something was claimed here and nobody has
    /// matched it to an approved claim yet"*.
    pub claim_ref: Option<String>,
    /// What the claim rests on, if anything is known. Carried through to the
    /// assertion row so that *"what did we say on the strength of this source"*
    /// stays answerable.
    pub evidence_refs: Vec<String>,
}

impl TranscriptUtterance {
    pub fn said(
        segment_key: impl Into<String>,
        speaker: SpeakerAttribution,
        spoken_text: impl Into<String>,
    ) -> Self {
        Self {
            segment_key: segment_key.into(),
            speaker,
            spoken_text: spoken_text.into(),
            claim_ref: None,
            evidence_refs: Vec::new(),
        }
    }

    /// The structural extraction rule, in one place.
    ///
    /// `Ok` names the resolved speaker; `Err` names why this utterance produced
    /// nothing. There is no third answer, and no branch that guesses.
    fn candidate(&self) -> std::result::Result<&str, SkipReason> {
        if self.spoken_text.trim().is_empty() {
            return Err(SkipReason::NoWords);
        }
        match &self.speaker {
            SpeakerAttribution::Ours(who) if !who.trim().is_empty() => Ok(who.as_str()),
            SpeakerAttribution::Ours(_) => Err(SkipReason::SpeakerUnresolved),
            SpeakerAttribution::Counterparty(_) => Err(SkipReason::SpokenByCounterparty),
            SpeakerAttribution::Unresolved => Err(SkipReason::SpeakerUnresolved),
        }
    }

    fn candidate_claim_ref(&self, transcript_key: &str) -> String {
        match self.claim_ref.as_deref() {
            Some(supplied) => supplied.to_string(),
            None => format!(
                "claim-{}",
                stable_id(&format!("{transcript_key}{FIELD_SEP}{}", self.segment_key))
            ),
        }
    }
}

/// Where a transcript came from and who was in the room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptSource {
    /// Stable key for the whole transcript. The outward act ref derives from it,
    /// so re-ingesting resumes one act rather than recording the room twice.
    pub transcript_key: String,
    /// Which observed channel this was. The store refuses a controlled one, and
    /// this module does not second-guess that judgement.
    pub channel: OutwardChannel,
    /// Our side's identity in the room, resolved by the runtime.
    pub effective_speaker: String,
    /// Who was in the room. One assertion row per person on confirmation, so a
    /// later correction can be aimed individually.
    pub attendees: Vec<String>,
    /// The relationship the conversation happened in, when there is a registered
    /// one. Required only by the commitment bridge, which refuses without it —
    /// a term with no relationship is nothing anybody can look up later.
    pub audience: Option<AudienceRef>,
    pub engagement_id: Option<String>,
    pub program_id: Option<String>,
    /// What this room cost if what was said in it was wrong. Supplied, never
    /// invented: this module cannot see whether a figure was confidential, and
    /// understating the class is the direction that misleads a later reviewer.
    pub consequence_class: ConsequenceClass,
    /// Who produced the candidates — the extractor, the model run, the import
    /// job. It is recorded so that confirmation can refuse to come from it.
    pub extracted_by: String,
    /// When the words were said, which is not when they were ingested.
    pub occurred_at: DateTime<Utc>,
}

impl TranscriptSource {
    /// A transcript from a meeting, classified as bounded communication — words
    /// said to people who were already in the room.
    ///
    /// A caller that knows more (financials read out, a data room opened on
    /// screen) raises `consequence_class` afterwards.
    pub fn observed(
        transcript_key: impl Into<String>,
        effective_speaker: impl Into<String>,
        attendees: Vec<String>,
        extracted_by: impl Into<String>,
        occurred_at: DateTime<Utc>,
    ) -> Self {
        Self {
            transcript_key: transcript_key.into(),
            channel: OutwardChannel::Meeting,
            effective_speaker: effective_speaker.into(),
            attendees,
            audience: None,
            engagement_id: None,
            program_id: None,
            consequence_class: ConsequenceClass::BoundedCommunication,
            extracted_by: extracted_by.into(),
            occurred_at,
        }
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.channel.is_controlled(),
            "channel `{}` is controlled; use prepare_dispatch before sending instead of importing it as observed",
            self.channel.as_str()
        );
        require_named("a transcript key", &self.transcript_key)?;
        refuse_separator("a transcript key", &self.transcript_key)?;
        require_named("the speaker acting for us", &self.effective_speaker)?;
        refuse_separator("the speaker acting for us", &self.effective_speaker)?;
        require_named(
            "the extractor that produced the candidates",
            &self.extracted_by,
        )?;
        if self.attendees.is_empty() {
            anyhow::bail!(
                "a transcript must name who was in the room: assertion rows are written one per \
                 person, so a confirmed claim with nobody to aim it at would write ZERO rows and \
                 report success — a claim absent from every reverse lookup"
            );
        }
        for attendee in &self.attendees {
            require_named("an attendee", attendee)?;
            refuse_separator("an attendee", attendee)?;
        }
        Ok(())
    }
}

/// Where a candidate claim stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptClaimStatus {
    /// Extracted, nobody has checked it. The only status this module writes on
    /// its own, and the only one a caller can reach without naming a person.
    Pending,
    /// A named person who did not extract it said we made this claim. The
    /// assertion rows exist by the time a claim reads this way.
    Confirmed,
    /// A named person said we did not make this claim. **Terminal.**
    Rejected,
}

impl TranscriptClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }

    /// Whether a decision can still be taken. False for both terminal states —
    /// a decided claim never returns to the queue.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// One candidate claim, held where the owner can see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptClaim {
    pub claim_id: String,
    pub principal: String,
    pub workspace: String,
    /// The act this claim was made in. Its payload is the whole transcript.
    pub outward_act_ref: String,
    pub approved_claim_ref: String,
    pub transcript_key: String,
    pub segment_key: String,
    /// The words themselves, so the owner decides by reading rather than by
    /// trusting a summary — the same reason a commitment carries its source.
    pub stated_text: String,
    /// Whose identity the utterance resolved to, on our side.
    pub speaker: String,
    /// Everyone in the room. One assertion row each on confirmation.
    pub audience: Vec<String>,
    pub evidence_refs: Vec<String>,
    /// Who produced the candidate. Whoever this is may not confirm it.
    pub extracted_by: String,
    /// Verbatim from the writer: what this is waiting for, in a form a surface
    /// can show without rewording it.
    pub awaiting: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_ref: Option<AudienceRef>,
    pub stated_at: DateTime<Utc>,
    pub extracted_at: DateTime<Utc>,
    pub status: TranscriptClaimStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    /// The assertion rows written when it was confirmed. Empty otherwise, and
    /// non-empty is what makes a later rejection impossible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertion_use_ids: Vec<String>,
    /// Monotonic optimistic-concurrency revision. Historical extraction rows
    /// deserialize at one; each accepted decision increments it exactly once.
    #[serde(default = "initial_revision")]
    pub revision: u64,
}

const fn initial_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimDecisionVerb {
    ConfirmClaim,
    RejectClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimDecisionDisposition {
    Applied,
    AlreadyApplied,
}

/// Durable decision receipt stored in the same JSONL record as the claim
/// transition. Consumers may rebuild their audit projection from this record;
/// the package-owned ledger is never authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDecisionReceipt {
    pub receipt_id: String,
    pub decision_id: String,
    pub claim_id: String,
    pub verb: ClaimDecisionVerb,
    pub expected_revision: u64,
    pub resulting_revision: u64,
    pub by: String,
    /// Digest of the complete request identity, including the note. Reusing a
    /// decision id with changed fields is a conflict, never an idempotent
    /// success response for a decision the caller did not make.
    pub request_fingerprint: String,
    pub recorded_at: DateTime<Utc>,
    pub disposition: ClaimDecisionDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDecisionOutcome {
    pub claim: TranscriptClaim,
    pub receipt: ClaimDecisionReceipt,
}

impl TranscriptClaim {
    /// Whether this claim is on the record as something we asserted.
    ///
    /// The single predicate every consumer should ask, mirroring
    /// `Commitment::may_be_restated_outward`: there is no way to get a yes out
    /// of a claim nobody confirmed.
    pub fn is_on_the_record(&self) -> bool {
        self.status == TranscriptClaimStatus::Confirmed
    }

    /// Whether the two records describe the same extraction.
    ///
    /// Used to tell an identical replay (resume) from a changed payload under a
    /// key that already exists (an error).
    fn matches(&self, other: &Self) -> bool {
        self.stated_text == other.stated_text
            && self.speaker == other.speaker
            && self.audience == other.audience
            && self.evidence_refs == other.evidence_refs
            && self.approved_claim_ref == other.approved_claim_ref
            && self.transcript_key == other.transcript_key
            && self.segment_key == other.segment_key
            && self.extracted_by == other.extracted_by
            && self.audience_ref == other.audience_ref
            && self.stated_at == other.stated_at
    }
}

/// A decision taken by a person.
///
/// `by` is not optional and not defaultable. An automated caller that wants to
/// decide has to put a name on it, and the name is checked against the extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerDecision {
    pub by: String,
    /// Free text the decider leaves. Part of the decision's identity: replaying
    /// a decision with a different note is a different decision, not a repeat.
    pub note: Option<String>,
}

impl OwnerDecision {
    pub fn by(who: impl Into<String>) -> Self {
        Self {
            by: who.into(),
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn validate(&self) -> Result<()> {
        if self.by.trim().is_empty() {
            anyhow::bail!(
                "a decision must name who took it; an unnamed decision is how an automated \
                 caller would grant itself the one control this register has"
            );
        }
        Ok(())
    }
}

/// What ingesting a transcript produced.
///
/// Counts, not rates: *"eleven of forty"* is a fact about this transcript, while
/// a percentage hides whether the run saw forty utterances or four.
#[derive(Debug, Clone)]
pub struct IngestedTranscript {
    /// The act. Recorded whether or not one word of it was understood.
    pub disclosure: OutwardActDisclosure,
    /// Every candidate this transcript stands for, in transcript order, in
    /// whatever state it currently holds.
    ///
    /// Not named `pending`: a fresh ingestion returns all-pending rows, but a
    /// replay after somebody decided returns the decided ones, and a field that
    /// promised otherwise would be read as a queue. The queue is
    /// [`TranscriptIngestion::pending_claims`].
    pub claims: Vec<TranscriptClaim>,
    /// Utterances that produced nothing, and why.
    pub skipped: Vec<SkippedUtterance>,
    pub utterances_seen: usize,
}

/// One log record. The register is append-only and folded on read, so a
/// decision never edits the row it decides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ClaimLogRecord {
    /// Bind all source and extraction metadata before a new import writes its
    /// act or candidates. The transcript payload itself contains only words
    /// and attribution; it cannot fence metadata substitution after a crash.
    IngestionPrepared {
        transcript_key: String,
        request_fingerprint: String,
    },
    Extracted(TranscriptClaim),
    /// Write-ahead intent for the multi-store confirmation transaction. The
    /// assertion rows are idempotent, so an exact retry can safely finish a
    /// preparation after a crash; a different decision may not cross it.
    ConfirmationPrepared {
        claim_id: String,
        decision_id: String,
        expected_revision: u64,
        by: String,
        note: Option<String>,
        request_fingerprint: String,
        prepared_at: DateTime<Utc>,
    },
    Confirmed {
        claim_id: String,
        by: String,
        note: Option<String>,
        assertion_use_ids: Vec<String>,
        at: DateTime<Utc>,
    },
    Rejected {
        claim_id: String,
        by: String,
        note: Option<String>,
        at: DateTime<Utc>,
    },
    ConfirmedWithReceipt {
        claim_id: String,
        by: String,
        note: Option<String>,
        assertion_use_ids: Vec<String>,
        at: DateTime<Utc>,
        receipt: ClaimDecisionReceipt,
    },
    RejectedWithReceipt {
        claim_id: String,
        by: String,
        note: Option<String>,
        at: DateTime<Utc>,
        receipt: ClaimDecisionReceipt,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaimConfirmationPreparation {
    decision_id: String,
    expected_revision: u64,
    by: String,
    note: Option<String>,
    request_fingerprint: String,
    prepared_at: DateTime<Utc>,
}

/// The exact owner command already admitted before a confirmation was
/// interrupted. Reading it grants no authority and performs no mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingClaimConfirmation {
    pub decision_id: String,
    pub expected_revision: u64,
    pub by: String,
    pub note: Option<String>,
    pub prepared_at: DateTime<Utc>,
}

/// The transcript caller and the pending-claim register.
///
/// It owns the pending queue and **nothing else**. Acts and assertion rows live
/// in [`OutwardAssertionStore`], which the evidence plan fixes as the one
/// authoritative copy; a second copy here is how a claim ends up asserted in one
/// subsystem and pending in another.
#[derive(Debug, Clone)]
pub struct TranscriptIngestion {
    workspace_layout: ArtifactV2Workspace,
}

impl TranscriptIngestion {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// The assertions store, over the same workspace root.
    pub fn assertions(&self) -> OutwardAssertionStore {
        OutwardAssertionStore::new(self.workspace_layout.clone())
    }

    /// The workspace root this register writes to.
    ///
    /// Exposed for the sibling stores that must address *the same* root — the
    /// completion journal and its receipt projector both do, and handing them a
    /// second layout to construct from is how two components end up disagreeing
    /// about where a scope lives.
    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }

    fn claims_path(&self, scope: &OutwardScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("transcript_claims")
            .join("claims.jsonl")
    }

    /// Record a receipted claim decision in the scope's completion journal.
    ///
    /// Recovering a receipt from this register means folding the whole claims
    /// log, and the decision has no other index — so without a journal the only
    /// way to answer *"what has been decided since I last looked"* is to replay
    /// history and diff it. The journal is the cursor; see
    /// [`super::completion_journal`] for why an identity-derived order cannot be
    /// one.
    ///
    /// Called on the first-write branch **and** on the replay branch, for the
    /// reason given there: the claim row is authoritative and lands first, so
    /// the replay is where an interrupted journal write is repaired.
    fn journal_completion(
        &self,
        scope: &OutwardScope,
        receipt: &ClaimDecisionReceipt,
        now: DateTime<Utc>,
    ) -> Result<()> {
        EvidenceCompletionJournal::new(self.workspace_layout.clone())
            .record_completion(
                &EvidenceDecisionScope::new(scope.principal.clone(), scope.workspace.clone()),
                &EvidenceDecisionCompletion {
                    decision_id: receipt.decision_id.clone(),
                    target: EvidenceDecisionTarget::TranscriptClaim {
                        claim_id: receipt.claim_id.clone(),
                    },
                    receipt_id: receipt.receipt_id.clone(),
                    request_fingerprint: receipt.request_fingerprint.clone(),
                    completed_at: receipt.recorded_at,
                },
                now,
            )
            .with_context(|| {
                format!(
                    "journalling completed claim decision `{}`",
                    receipt.decision_id
                )
            })?;
        Ok(())
    }

    // ── Ingestion ───────────────────────────────────────────────────────────

    /// Record an observed transcript, and surface what it appeared to claim.
    ///
    /// After binding the admitted request, the act is recorded **before any
    /// candidates and unconditionally**, because it happened:
    /// the record of a disclosure must not depend on how well anybody understood
    /// it. A transcript with nothing extractable, or with no utterances at all,
    /// still produces an act — a room whose record only exists when a model
    /// found something in it is a room that disappears exactly when the model is
    /// worst.
    ///
    /// Every candidate comes back **pending**. Nothing here can write an
    /// assertion, and no argument exists that would make it: the extraction is
    /// handed to [`record_observed_statement`] as
    /// [`ClaimConfirmation::Extracted`], which the writer refuses to assert.
    ///
    /// # Replay
    ///
    /// Ingesting the same `transcript_key` again resumes: the same act, the same
    /// claim ids, no second copy in the queue. Ingesting the same key with
    /// **different words or metadata** is an error rather than a silent no-op.
    /// New imports persist the complete request fingerprint before writing an
    /// act or candidate, so an interrupted import can only resume that request.
    pub fn ingest_transcript(
        &self,
        scope: &OutwardScope,
        source: &TranscriptSource,
        utterances: &[TranscriptUtterance],
        now: DateTime<Utc>,
    ) -> Result<IngestedTranscript> {
        validate_scope(scope)?;
        source.validate()?;
        validate_segments(&source.transcript_key, utterances)?;

        // Concurrent retries must compare against one authoritative opening.
        // Otherwise two callers can each see no act and record different room
        // metadata under the same transcript key before either queues a claim.
        let path = self.claims_path(scope);
        let _guard = acquire_record_decision_lock(
            path.parent().context("missing claim root")?,
            &format!("ingest-{}", stable_id(&source.transcript_key)),
            "transcript ingestion",
        )?;

        let assertions = self.assertions();
        let now_text = now.to_rfc3339();
        let spoken_text = render_transcript(utterances);
        let request_fingerprint = blake3::hash(&serde_json::to_vec(&(
            "transcript-import-v1",
            source,
            utterances,
        ))?)
        .to_hex()
        .to_string();
        let mut prepared = match self.ingestion_fingerprint(scope, &source.transcript_key)? {
            Some(held) => {
                anyhow::ensure!(
                    held == request_fingerprint,
                    "transcript `{}` is already prepared with different words or source/extraction context; resume the original request or ingest a revision under its own key",
                    source.transcript_key
                );
                true
            },
            None => false,
        };
        if !prepared
            && assertions
                .load_act(
                    scope,
                    &super::outward_assertions::derive_act_ref(
                        scope,
                        &format!("observed:{}", source.transcript_key),
                    ),
                )?
                .is_none()
        {
            self.append(
                scope,
                &ClaimLogRecord::IngestionPrepared {
                    transcript_key: source.transcript_key.clone(),
                    request_fingerprint: request_fingerprint.clone(),
                },
            )?;
            prepared = true;
        }

        // Content-addressing the transcript before recording gives us the ref
        // the writer will land on, without duplicating its derivation. Storing
        // is idempotent — identical bytes are already under this ref — so this
        // is the same write the writer would do, taken one step earlier.
        let payload_ref = assertions
            .store_payload(scope, spoken_text.as_bytes())
            .context("storing the transcript as the payload of its own disclosure")?;

        let opening = record_observed_statement(
            &assertions,
            scope,
            &observed_statement(source, &spoken_text, None),
            &now_text,
        )
        .context("recording the act for this transcript")?;
        let act = opening.disclosure;

        if act.exact_payload_artifact_ref != payload_ref {
            anyhow::bail!(
                "transcript `{}` is already recorded as act `{}` with different words. An \
                 identical replay resumes; changed words under a key that already exists would \
                 leave the act pointing at the first transcript while the caller believed it had \
                 recorded the second. Ingest the revision under its own key.",
                source.transcript_key,
                act.outward_act_ref
            );
        }
        anyhow::ensure!(
            act.observed
                && act.effective_sender == source.effective_speaker
                && act.intended_audience == source.attendees
                && act.channel == source.channel
                && act.program_id == source.program_id
                && act.engagement_id == source.engagement_id
                && act.consequence_class == source.consequence_class.as_str(),
            "transcript `{}` is already recorded with different source context; ingest a revision under its own key",
            source.transcript_key
        );

        let mut claims = Vec::new();
        let mut skipped = Vec::new();
        let mut existing = self.claims(scope)?;
        let mut new_claims = Vec::new();

        for utterance in utterances {
            let speaker = match utterance.candidate() {
                Ok(speaker) => speaker.to_string(),
                Err(reason) => {
                    skipped.push(SkippedUtterance {
                        segment_key: utterance.segment_key.clone(),
                        reason,
                    });
                    continue;
                },
            };

            let approved_claim_ref = utterance.candidate_claim_ref(&source.transcript_key);
            let recorded = record_observed_statement(
                &assertions,
                scope,
                &observed_statement(
                    source,
                    &spoken_text,
                    Some(ExtractedClaim {
                        approved_claim_ref: approved_claim_ref.clone(),
                        evidence_refs: utterance.evidence_refs.clone(),
                        // The whole point. There is no branch above that can set
                        // this to `OwnerConfirmed`.
                        confirmation: ClaimConfirmation::Extracted,
                        supersedes: Vec::new(),
                    }),
                ),
                &now_text,
            )
            .with_context(|| {
                format!(
                    "recording the claim extracted from segment `{}`",
                    utterance.segment_key
                )
            })?;

            // The writer's contract, checked rather than assumed. If either of
            // these ever fires, an extraction has been written as something the
            // company asserted, and every downstream correction would chase
            // people over a model's reading of a room.
            if !recorded.assertion_use_ids.is_empty() {
                anyhow::bail!(
                    "segment `{}` produced {} assertion row(s) from an UNCONFIRMED extraction: \
                     nothing may be asserted on a model's reading of a room",
                    utterance.segment_key,
                    recorded.assertion_use_ids.len()
                );
            }
            let Some(surfaced) = recorded.pending else {
                anyhow::bail!(
                    "segment `{}` produced neither an assertion nor a pending claim: an \
                     extraction that is silently dropped is the failure this queue exists to \
                     prevent",
                    utterance.segment_key
                );
            };

            let claim_id = derive_claim_id(scope, &act.outward_act_ref, &approved_claim_ref);
            let claim = TranscriptClaim {
                claim_id: claim_id.clone(),
                principal: scope.principal.clone(),
                workspace: scope.workspace.clone(),
                outward_act_ref: act.outward_act_ref.clone(),
                approved_claim_ref,
                transcript_key: source.transcript_key.clone(),
                segment_key: utterance.segment_key.clone(),
                stated_text: utterance.spoken_text.clone(),
                speaker,
                audience: surfaced.audience.clone(),
                evidence_refs: utterance.evidence_refs.clone(),
                extracted_by: source.extracted_by.clone(),
                awaiting: surfaced.awaiting.clone(),
                audience_ref: source.audience.clone(),
                stated_at: source.occurred_at,
                extracted_at: now,
                status: TranscriptClaimStatus::Pending,
                decided_by: None,
                decision_note: None,
                decided_at: None,
                assertion_use_ids: Vec::new(),
                revision: initial_revision(),
            };

            // A catalogue reference participates in the derived claim ID, but
            // it cannot rename a previously imported source segment. Looking
            // up only that ID lets a changed claim_ref create another pending
            // candidate after an owner has already rejected these same words.
            if let Some(held) = existing.iter().find(|row| {
                row.claim_id == claim_id
                    || (row.outward_act_ref == act.outward_act_ref
                        && row.segment_key == utterance.segment_key)
            }) {
                if !held.matches(&claim) {
                    anyhow::bail!(
                        "segment `{}` is already recorded as claim `{}` with different \
                         content. An identical replay resumes; a changed one is an error rather \
                         than a silent no-op, because the queued words are what the decider reads",
                        utterance.segment_key,
                        held.claim_id
                    );
                }
                claims.push(held.clone());
                continue;
            }

            // Validate the entire batch before publishing any candidate. A
            // conflict in a later segment must not leave earlier rows behind.
            new_claims.push(claim.clone());
            existing.push(claim.clone());
            claims.push(claim);
        }

        // Historical imports lack this write-ahead record. Bind them only
        // after checking their act and every extant claim; an invalid replay
        // must never reserve metadata that blocks the original request.
        if !prepared {
            self.append(
                scope,
                &ClaimLogRecord::IngestionPrepared {
                    transcript_key: source.transcript_key.clone(),
                    request_fingerprint,
                },
            )?;
        }
        for claim in new_claims {
            self.append(scope, &ClaimLogRecord::Extracted(claim))?;
        }

        Ok(IngestedTranscript {
            disclosure: act,
            claims,
            skipped,
            utterances_seen: utterances.len(),
        })
    }

    // ── The owner surface ───────────────────────────────────────────────────

    /// Queue exact, host-prepared Envoy words before channel dispatch. This is
    /// a candidate, never an assertion or proof of delivery. Unlike observed
    /// transcripts, the controlled act already exists before anything leaves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_controlled_reply(
        &self,
        scope: &OutwardScope,
        act_ref: &str,
        transcript_key: &str,
        segment_key: &str,
        text: &str,
        extracted_by: &str,
        stated_at: DateTime<Utc>,
    ) -> Result<TranscriptClaim> {
        validate_scope(scope)?;
        require_named("reply", text)?;
        let assertions = self.assertions();
        let act = assertions
            .load_act(scope, act_ref)?
            .context("prepared reply is missing")?;
        anyhow::ensure!(
            !act.observed && act.channel.is_controlled(),
            "reply must have a controlled act"
        );
        anyhow::ensure!(
            assertions
                .load_payload(scope, &act.exact_payload_artifact_ref)?
                .as_deref()
                == Some(text),
            "reply differs from the prepared payload"
        );
        let approved_claim_ref = format!(
            "claim-{}",
            stable_id(&format!("{transcript_key}{FIELD_SEP}{segment_key}"))
        );
        let claim_id = derive_claim_id(scope, act_ref, &approved_claim_ref);
        let path = self.claims_path(scope);
        let _guard = acquire_record_decision_lock(
            path.parent().context("missing claim root")?,
            &format!("claim-{}", stable_id(&claim_id)),
            "envoy reply",
        )?;
        let claim = TranscriptClaim {
            claim_id: claim_id.clone(), principal: scope.principal.clone(), workspace: scope.workspace.clone(),
            outward_act_ref: act_ref.to_owned(), approved_claim_ref,
            transcript_key: transcript_key.to_owned(), segment_key: segment_key.to_owned(),
            stated_text: text.to_owned(), speaker: act.effective_sender,
            audience: act.intended_audience, evidence_refs: Vec::new(),
            extracted_by: extracted_by.to_owned(),
            awaiting: "Review the exact Envoy reply after the channel accepts it. Confirmation records what was said; it does not verify the claim is true.".into(),
            audience_ref: act.audience, stated_at, extracted_at: Utc::now(),
            status: TranscriptClaimStatus::Pending, decided_by: None, decision_note: None,
            decided_at: None, assertion_use_ids: Vec::new(), revision: initial_revision(),
        };
        if let Some(held) = self.claim(scope, &claim_id)? {
            anyhow::ensure!(
                held.matches(&claim),
                "prepared reply changed under the same message id"
            );
            return Ok(held);
        }
        self.append(scope, &ClaimLogRecord::Extracted(claim.clone()))?;
        Ok(claim)
    }

    pub(crate) fn controlled_reply_claim(
        &self,
        scope: &OutwardScope,
        act_ref: &str,
        transcript_key: &str,
        segment_key: &str,
    ) -> Result<Option<TranscriptClaim>> {
        let approved_claim_ref = format!(
            "claim-{}",
            stable_id(&format!("{transcript_key}{FIELD_SEP}{segment_key}"))
        );
        self.claim(scope, &derive_claim_id(scope, act_ref, &approved_claim_ref))
    }

    /// Everything still waiting for a person, oldest first.
    pub fn pending_claims(&self, scope: &OutwardScope) -> Result<Vec<TranscriptClaim>> {
        Ok(self
            .claims(scope)?
            .into_iter()
            .filter(|claim| claim.status == TranscriptClaimStatus::Pending)
            .collect())
    }

    /// Every claim ever extracted in this scope, decided or not, oldest first.
    pub fn claims(&self, scope: &OutwardScope) -> Result<Vec<TranscriptClaim>> {
        self.fold_claims(scope, StoreFoldCursor::new(TRANSCRIPT_CLAIM_REGISTER))
    }

    /// [`Self::claims`], abandoned when `cancellation` fires.
    ///
    /// The claims log is one file per scope and the queue projection pages it
    /// only after the whole log has been folded, so an owner who navigates away
    /// leaves a blocking worker folding a register nobody will read. A
    /// cancelled fold returns [`super::StoreFoldCancelled`] and no rows; see
    /// that module for why a partial claims log is a different answer rather
    /// than a shorter one.
    pub fn claims_until_cancelled(
        &self,
        scope: &OutwardScope,
        cancellation: &CancellationToken,
    ) -> Result<Vec<TranscriptClaim>> {
        self.fold_claims(
            scope,
            StoreFoldCursor::cancelled_by(TRANSCRIPT_CLAIM_REGISTER, cancellation),
        )
    }

    fn fold_claims(
        &self,
        scope: &OutwardScope,
        mut cursor: StoreFoldCursor,
    ) -> Result<Vec<TranscriptClaim>> {
        let path = self.claims_path(scope);
        // Before the read, so an abandoned fold does no I/O, and again before
        // the parse — the one stretch inside `magician_v2::jsonl` that the
        // per-record check below cannot reach into.
        cursor.checkpoint()?;
        // NotFound is the only error that reads as an empty register. Everything
        // else propagates: an unreadable log folded to "empty" would report a
        // queue full of undecided claims as nothing to look at, and a rejected
        // claim as never decided. Shared semantics live in `magician_v2::jsonl`.
        let read = crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?;
        let Some(raw) = read else {
            return Ok(Vec::new());
        };
        cursor.checkpoint()?;

        // The index is the `seen` set and the decision lookup at once — they
        // were always the same question, and asking it linearly made the fold
        // quadratic in the size of the log.
        let mut index = StoreFoldIndex::new();
        let mut out: Vec<TranscriptClaim> = Vec::new();
        for record in crate::magician_v2::jsonl::parse_log_lines::<ClaimLogRecord>(&raw, &path)? {
            cursor.admit()?;
            match record {
                ClaimLogRecord::Extracted(claim) => {
                    let claim_id = claim.claim_id.clone();
                    index.push_head(&mut out, claim_id, claim);
                },
                ClaimLogRecord::IngestionPrepared { .. }
                | ClaimLogRecord::ConfirmationPrepared { .. } => {},
                ClaimLogRecord::Confirmed {
                    claim_id,
                    by,
                    note,
                    assertion_use_ids,
                    at,
                } => {
                    if let Some(held) = index.head_mut(&mut out, &claim_id) {
                        // A terminal state never resurrects. The API refuses a
                        // second decision before appending one; this is the fold's
                        // own backstop for a log that acquired one anyway.
                        if held.status.is_open() {
                            held.status = TranscriptClaimStatus::Confirmed;
                            held.decided_by = Some(by);
                            held.decision_note = note;
                            held.decided_at = Some(at);
                            held.assertion_use_ids = assertion_use_ids;
                            held.revision = held.revision.saturating_add(1);
                        }
                    }
                },
                ClaimLogRecord::ConfirmedWithReceipt {
                    claim_id,
                    by,
                    note,
                    assertion_use_ids,
                    at,
                    receipt,
                } => {
                    let decision = OwnerDecision {
                        by: by.clone(),
                        note: note.clone(),
                    };
                    if at != receipt.recorded_at {
                        anyhow::bail!(
                            "claim confirmation event time differs from its decision receipt"
                        );
                    }
                    validate_claim_decision_receipt(
                        &receipt,
                        &receipt.decision_id,
                        &claim_id,
                        ClaimDecisionVerb::ConfirmClaim,
                        receipt.expected_revision,
                        &decision,
                    )?;
                    if let Some(held) = index.head_mut(&mut out, &claim_id) {
                        // A terminal state never resurrects. The API refuses a
                        // second decision before appending one; this is the fold's
                        // own backstop for a log that acquired one anyway.
                        if held.status.is_open() {
                            if held.revision != receipt.expected_revision
                                || assertion_use_ids.is_empty()
                            {
                                anyhow::bail!(
                                    "claim confirmation receipt does not transition its recorded head"
                                );
                            }
                            held.status = TranscriptClaimStatus::Confirmed;
                            held.decided_by = Some(by);
                            held.decision_note = note;
                            held.decided_at = Some(at);
                            held.assertion_use_ids = assertion_use_ids;
                            held.revision = held.revision.saturating_add(1);
                            validate_claim_outcome_against_receipt(held, &receipt, &decision)?;
                        }
                    }
                },
                ClaimLogRecord::Rejected {
                    claim_id,
                    by,
                    note,
                    at,
                } => {
                    if let Some(held) = index.head_mut(&mut out, &claim_id) {
                        if held.status.is_open() {
                            held.status = TranscriptClaimStatus::Rejected;
                            held.decided_by = Some(by);
                            held.decision_note = note;
                            held.decided_at = Some(at);
                            held.revision = held.revision.saturating_add(1);
                        }
                    }
                },
                ClaimLogRecord::RejectedWithReceipt {
                    claim_id,
                    by,
                    note,
                    at,
                    receipt,
                } => {
                    let decision = OwnerDecision {
                        by: by.clone(),
                        note: note.clone(),
                    };
                    if at != receipt.recorded_at {
                        anyhow::bail!(
                            "claim rejection event time differs from its decision receipt"
                        );
                    }
                    validate_claim_decision_receipt(
                        &receipt,
                        &receipt.decision_id,
                        &claim_id,
                        ClaimDecisionVerb::RejectClaim,
                        receipt.expected_revision,
                        &decision,
                    )?;
                    if let Some(held) = index.head_mut(&mut out, &claim_id) {
                        if held.status.is_open() {
                            if held.revision != receipt.expected_revision {
                                anyhow::bail!(
                                    "claim rejection receipt does not transition its recorded head"
                                );
                            }
                            held.status = TranscriptClaimStatus::Rejected;
                            held.decided_by = Some(by);
                            held.decision_note = note;
                            held.decided_at = Some(at);
                            held.revision = held.revision.saturating_add(1);
                            validate_claim_outcome_against_receipt(held, &receipt, &decision)?;
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    pub fn claim(&self, scope: &OutwardScope, claim_id: &str) -> Result<Option<TranscriptClaim>> {
        Ok(self
            .claims(scope)?
            .into_iter()
            .find(|claim| claim.claim_id == claim_id))
    }

    /// A person says we did make this claim.
    ///
    /// Writes one assertion row per person in the room, **then** the decision.
    /// That order is the fail-closed one: the invariant worth holding is *"a
    /// claim that reads confirmed is findable in the reverse lookup"*, so a
    /// crash between the two leaves a claim still pending — which a retry
    /// re-decides, the rows being idempotent by derived id. The other order
    /// leaves a claim reading confirmed with nothing behind it, and correction
    /// propagation would never find the people who heard it.
    ///
    /// # Refusals
    ///
    /// - An unnamed decider (see [`OwnerDecision`]).
    /// - The extractor confirming its own extraction.
    /// - A claim already decided, unless the decision is identical.
    pub fn confirm_claim(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
    ) -> Result<TranscriptClaim> {
        validate_scope(scope)?;
        decision.validate()?;
        let claim = self.load_for_decision(scope, claim_id)?;
        if decision
            .by
            .trim()
            .eq_ignore_ascii_case(claim.extracted_by.trim())
        {
            anyhow::bail!(
                "`{}` extracted this claim and cannot also confirm it. A confirmation is somebody \
                 else's act; an extractor confirming its own reading of a room is the self-grant \
                 the named-decider rule exists to prevent",
                decision.by
            );
        }
        if let Some(settled) =
            self.replayed_decision(&claim, TranscriptClaimStatus::Confirmed, decision)?
        {
            return Ok(settled);
        }
        let decision_id =
            legacy_claim_decision_id(claim_id, ClaimDecisionVerb::ConfirmClaim, decision);
        Ok(self
            .confirm_claim_at_revision(
                scope,
                claim_id,
                claim.revision,
                &decision_id,
                decision,
                now,
            )?
            .claim)
    }

    /// Revision-bound claim confirmation with a receipt persisted atomically
    /// beside the authoritative decision event. The cross-process decision
    /// lock closes the read/append race between competing owner decisions.
    pub fn confirm_claim_at_revision(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
    ) -> Result<ClaimDecisionOutcome> {
        self.confirm_claim_at_revision_guarded(
            scope,
            claim_id,
            expected_revision,
            decision_id,
            decision,
            now,
            |_| Ok(()),
        )
    }

    /// Guarded form of [`Self::confirm_claim_at_revision`]. The admission
    /// callback runs with both destination locks held, after exact receipt
    /// replay and CAS validation, and immediately before the first durable
    /// write. Its argument carries the durable preparation time only when this
    /// exact decision is resuming an already-durable `ConfirmationPrepared`
    /// transaction.
    pub fn confirm_claim_at_revision_guarded<F>(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
        admit_write: F,
    ) -> Result<ClaimDecisionOutcome>
    where
        F: FnOnce(Option<DateTime<Utc>>) -> Result<()>,
    {
        validate_scope(scope)?;
        decision.validate()?;
        validate_claim_decision_identity(claim_id, decision_id, expected_revision)?;
        let path = self.claims_path(scope);
        let lock_root = path.parent().context("claim register lost its parent")?;
        let decision_lock_id = format!("decision-{}", stable_id(decision_id));
        let claim_lock_id = format!("claim-{}", stable_id(claim_id));
        let _decision_guard =
            acquire_record_decision_lock(lock_root, &decision_lock_id, "claim decision")?;
        let _claim_guard =
            acquire_record_decision_lock(lock_root, &claim_lock_id, "transcript claim")?;
        let request_fingerprint = claim_decision_request_fingerprint(
            claim_id,
            ClaimDecisionVerb::ConfirmClaim,
            expected_revision,
            decision,
        );
        if let Some(receipt) = self.receipt_for_decision(scope, decision_id)? {
            validate_claim_decision_receipt(
                &receipt,
                decision_id,
                claim_id,
                ClaimDecisionVerb::ConfirmClaim,
                expected_revision,
                decision,
            )?;
            let claim = self
                .claim(scope, claim_id)?
                .context("claim decision receipt points at a missing claim")?;
            validate_claim_outcome_against_receipt(&claim, &receipt, decision)?;
            self.journal_completion(scope, &receipt, now)?;
            return Ok(ClaimDecisionOutcome {
                claim,
                receipt: replayed_claim_receipt(receipt),
            });
        }
        let claim = self.load_for_decision(scope, claim_id)?;
        if claim.revision != expected_revision {
            anyhow::bail!(
                "stale claim revision: expected {expected_revision}, current {}",
                claim.revision
            );
        }
        if claim.transcript_key.starts_with("envoy:") {
            let act = self
                .assertions()
                .load_act(scope, &claim.outward_act_ref)?
                .context("Envoy act missing")?;
            anyhow::ensure!(matches!(act.status, super::outward_assertions::OutwardActStatus::ProviderAccepted | super::outward_assertions::OutwardActStatus::Delivered), "The channel has not acknowledged this Envoy reply; it cannot be confirmed as said.");
        }
        if !claim.status.is_open() {
            anyhow::bail!(
                "claim `{claim_id}` is already {} under a different decision",
                claim.status.as_str()
            );
        }
        if decision
            .by
            .trim()
            .eq_ignore_ascii_case(claim.extracted_by.trim())
        {
            anyhow::bail!(
                "`{}` extracted this claim and cannot also confirm it",
                decision.by
            );
        }

        if claim.audience.is_empty() {
            anyhow::bail!(
                "claim `{claim_id}` names nobody who heard it, so confirming it would write ZERO \
                 assertion rows and report success — a claim absent from every reverse lookup"
            );
        }

        let prepared_at = match self.unresolved_confirmation_preparation(scope, claim_id)? {
            Some(prepared)
                if prepared.decision_id == decision_id
                    && prepared.expected_revision == expected_revision
                    && prepared.by == decision.by
                    && prepared.note == decision.note
                    && prepared.request_fingerprint == request_fingerprint =>
            {
                Some(prepared.prepared_at)
            },
            Some(_) => {
                anyhow::bail!(
                    "claim `{claim_id}` has an incomplete confirmation transaction; only its exact decision may resume"
                )
            },
            None => None,
        };

        // This is deliberately inside the decision/claim critical section.
        // A signed caller can re-sample its authority and proposal lifetime at
        // the last possible point without changing the compatibility wrapper.
        let resuming_preparation = prepared_at.is_some();
        admit_write(prepared_at)?;
        if !resuming_preparation {
            self.append(
                scope,
                &ClaimLogRecord::ConfirmationPrepared {
                    claim_id: claim_id.to_owned(),
                    decision_id: decision_id.to_owned(),
                    expected_revision,
                    by: decision.by.clone(),
                    note: decision.note.clone(),
                    request_fingerprint: request_fingerprint.clone(),
                    prepared_at: now,
                },
            )?;
        }

        let assertions = self.assertions();
        let now_text = now.to_rfc3339();
        let mut assertion_use_ids = Vec::with_capacity(claim.audience.len());
        for member in &claim.audience {
            let recorded = assertions
                .record_assertion_use(
                    scope,
                    &claim.outward_act_ref,
                    &claim.approved_claim_ref,
                    member,
                    &claim.evidence_refs,
                    &[],
                    &now_text,
                )
                .with_context(|| format!("asserting confirmed claim `{claim_id}` to `{member}`"))?;
            assertion_use_ids.push(recorded.assertion_use_id);
        }

        let resulting_revision = expected_revision
            .checked_add(1)
            .context("claim revision overflow")?;
        let receipt = claim_decision_receipt(
            decision_id,
            claim_id,
            ClaimDecisionVerb::ConfirmClaim,
            expected_revision,
            resulting_revision,
            &decision.by,
            request_fingerprint,
            now,
        );
        self.append(
            scope,
            &ClaimLogRecord::ConfirmedWithReceipt {
                claim_id: claim_id.to_string(),
                by: decision.by.clone(),
                note: decision.note.clone(),
                assertion_use_ids,
                at: now,
                receipt: receipt.clone(),
            },
        )?;
        let claim = self
            .claim(scope, claim_id)?
            .context("claim vanished immediately after confirmation")?;
        self.journal_completion(scope, &receipt, now)?;
        Ok(ClaimDecisionOutcome { claim, receipt })
    }

    /// A person says we did not make this claim. **Terminal.**
    ///
    /// Refuses when assertion rows already exist for the claim. A rejection
    /// after the rows were written would leave a claim reading *"we never said
    /// this"* while the reverse lookup still points at everyone it was asserted
    /// to — the register and the record disagreeing about the same sentence. The
    /// route from there is a correction against the claim, which the assertions
    /// store already owns and which reaches the people who heard it.
    pub fn reject_claim(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
    ) -> Result<TranscriptClaim> {
        validate_scope(scope)?;
        decision.validate()?;
        let claim = self.load_for_decision(scope, claim_id)?;
        if let Some(settled) =
            self.replayed_decision(&claim, TranscriptClaimStatus::Rejected, decision)?
        {
            return Ok(settled);
        }
        let decision_id =
            legacy_claim_decision_id(claim_id, ClaimDecisionVerb::RejectClaim, decision);
        Ok(self
            .reject_claim_at_revision(scope, claim_id, claim.revision, &decision_id, decision, now)?
            .claim)
    }

    /// Revision-bound rejection under the same receipt/CAS boundary as
    /// confirmation. A competing confirmation that wins the lock makes this
    /// call stale before any rejection record is appended.
    pub fn reject_claim_at_revision(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
    ) -> Result<ClaimDecisionOutcome> {
        self.reject_claim_at_revision_guarded(
            scope,
            claim_id,
            expected_revision,
            decision_id,
            decision,
            now,
            || Ok(()),
        )
    }

    /// Guarded form of [`Self::reject_claim_at_revision`]. Exact receipt
    /// replay returns without invoking the callback; every new rejection calls
    /// it under both destination locks immediately before the append.
    pub fn reject_claim_at_revision_guarded<F>(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
        admit_write: F,
    ) -> Result<ClaimDecisionOutcome>
    where
        F: FnOnce() -> Result<()>,
    {
        validate_scope(scope)?;
        decision.validate()?;
        validate_claim_decision_identity(claim_id, decision_id, expected_revision)?;
        let path = self.claims_path(scope);
        let lock_root = path.parent().context("claim register lost its parent")?;
        let decision_lock_id = format!("decision-{}", stable_id(decision_id));
        let claim_lock_id = format!("claim-{}", stable_id(claim_id));
        let _decision_guard =
            acquire_record_decision_lock(lock_root, &decision_lock_id, "claim decision")?;
        let _claim_guard =
            acquire_record_decision_lock(lock_root, &claim_lock_id, "transcript claim")?;
        let request_fingerprint = claim_decision_request_fingerprint(
            claim_id,
            ClaimDecisionVerb::RejectClaim,
            expected_revision,
            decision,
        );
        if let Some(receipt) = self.receipt_for_decision(scope, decision_id)? {
            validate_claim_decision_receipt(
                &receipt,
                decision_id,
                claim_id,
                ClaimDecisionVerb::RejectClaim,
                expected_revision,
                decision,
            )?;
            let claim = self
                .claim(scope, claim_id)?
                .context("claim decision receipt points at a missing claim")?;
            validate_claim_outcome_against_receipt(&claim, &receipt, decision)?;
            self.journal_completion(scope, &receipt, now)?;
            return Ok(ClaimDecisionOutcome {
                claim,
                receipt: replayed_claim_receipt(receipt),
            });
        }
        let claim = self.load_for_decision(scope, claim_id)?;
        if claim.revision != expected_revision {
            anyhow::bail!(
                "stale claim revision: expected {expected_revision}, current {}",
                claim.revision
            );
        }
        if !claim.status.is_open() {
            anyhow::bail!(
                "claim `{claim_id}` is already {} under a different decision",
                claim.status.as_str()
            );
        }
        if self
            .unresolved_confirmation_preparation(scope, claim_id)?
            .is_some()
        {
            anyhow::bail!(
                "claim `{claim_id}` has an incomplete confirmation transaction and cannot be rejected; resume that exact confirmation first"
            );
        }

        // Scoped to THIS act. The same approved claim may well have been
        // asserted by an email, and *"we did not say this in that room"* is a
        // different fact from *"we never said this anywhere"* — refusing on the
        // email's row would make an honest rejection impossible.
        let asserted: Vec<_> = self
            .assertions()
            .disclosures_carrying_claim(scope, &claim.approved_claim_ref)?
            .into_iter()
            .filter(|row| row.outward_act_ref == claim.outward_act_ref)
            .collect();
        if !asserted.is_empty() {
            anyhow::bail!(
                "claim `{claim_id}` has already been asserted to {} person/people. Rejecting it \
                 now would leave the register saying we never said it while the reverse lookup \
                 still points at everyone who heard it; raise a correction against \
                 `{}` instead",
                asserted.len(),
                claim.approved_claim_ref
            );
        }

        let resulting_revision = expected_revision
            .checked_add(1)
            .context("claim revision overflow")?;
        let receipt = claim_decision_receipt(
            decision_id,
            claim_id,
            ClaimDecisionVerb::RejectClaim,
            expected_revision,
            resulting_revision,
            &decision.by,
            request_fingerprint,
            now,
        );
        admit_write()?;
        self.append(
            scope,
            &ClaimLogRecord::RejectedWithReceipt {
                claim_id: claim_id.to_string(),
                by: decision.by.clone(),
                note: decision.note.clone(),
                at: now,
                receipt: receipt.clone(),
            },
        )?;
        let claim = self
            .claim(scope, claim_id)?
            .context("claim vanished immediately after rejection")?;
        self.journal_completion(scope, &receipt, now)?;
        Ok(ClaimDecisionOutcome { claim, receipt })
    }

    // ── The commitment bridge ───────────────────────────────────────────────

    /// Turn a confirmed claim into a tracked commitment.
    ///
    /// *"We said we'd have the integration done by March"* said in a room is,
    /// until something does this, a line in a transcript. Afterwards it is a row
    /// in the commitments register, which means it appears in
    /// [`Commitments::unconfirmed_from_us`] — *"exactly the thing worth finding
    /// before the other side does"*.
    ///
    /// The commitment lands **unconfirmed**, and that is not an oversight. The
    /// owner confirmed *"we said this"*. Whether the term is one we stand behind
    /// is a second question with a second answer, and collapsing the two would
    /// let a sentence in a meeting become a binding term with one click. It also
    /// could not be done honestly here even if it were wanted: `Commitments` has
    /// no path that produces a confirmed row without a named person confirming
    /// it, which is the same rule this module obeys one layer up.
    ///
    /// The register is passed in rather than built here, so the caller decides
    /// which one the term lands in; it is normally constructed over the same
    /// workspace root as this ingestion.
    pub fn record_commitment_from_claim(
        &self,
        commitments: &Commitments,
        scope: &OutwardScope,
        claim_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Commitment> {
        validate_scope(scope)?;
        let claim = self
            .claim(scope, claim_id)?
            .with_context(|| format!("no transcript claim `{claim_id}`"))?;
        let request = commitment_request_from_claim(&claim)?;
        commitments.record(
            &CommitmentScope::new(scope.principal.as_str(), scope.workspace.as_str()),
            &request,
            now,
        )
    }

    /// Revision-bound commitment creation through the same mapping as the
    /// first-party API. The commitment and its receipt are one authoritative
    /// register append; the confirmed claim revision is checked immediately
    /// before that append and is immutable afterwards.
    pub fn record_commitment_from_claim_at_revision(
        &self,
        commitments: &Commitments,
        scope: &OutwardScope,
        claim_id: &str,
        expected_claim_revision: u64,
        decision_id: &str,
        now: DateTime<Utc>,
    ) -> Result<CommitmentDecisionOutcome> {
        self.record_commitment_from_claim_at_revision_guarded(
            commitments,
            scope,
            claim_id,
            expected_claim_revision,
            decision_id,
            now,
            || Ok(()),
        )
    }

    /// Guarded form of
    /// [`Self::record_commitment_from_claim_at_revision`]. The callback is
    /// forwarded to the destination commitment store and therefore runs only
    /// after that store owns its decision and target locks.
    pub fn record_commitment_from_claim_at_revision_guarded<F>(
        &self,
        commitments: &Commitments,
        scope: &OutwardScope,
        claim_id: &str,
        expected_claim_revision: u64,
        decision_id: &str,
        now: DateTime<Utc>,
        admit_write: F,
    ) -> Result<CommitmentDecisionOutcome>
    where
        F: FnOnce() -> Result<()>,
    {
        validate_scope(scope)?;
        validate_claim_decision_identity(claim_id, decision_id, expected_claim_revision)?;
        let claim = self
            .claim(scope, claim_id)?
            .with_context(|| format!("no transcript claim `{claim_id}`"))?;
        if claim.revision != expected_claim_revision {
            anyhow::bail!(
                "stale claim revision: expected {expected_claim_revision}, current {}",
                claim.revision
            );
        }
        let request = commitment_request_from_claim(&claim)?;
        commitments.record_at_claim_revision_guarded(
            &CommitmentScope::new(scope.principal.as_str(), scope.workspace.as_str()),
            &request,
            expected_claim_revision,
            decision_id,
            now,
            admit_write,
        )
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn load_for_decision(&self, scope: &OutwardScope, claim_id: &str) -> Result<TranscriptClaim> {
        self.claim(scope, claim_id)?
            .with_context(|| format!("no transcript claim `{claim_id}` in this scope"))
    }

    /// `Ok(Some(claim))` when this exact decision has already been taken.
    ///
    /// A decision differing from the one on file — a different person, a
    /// different note, or the opposite verdict — is an error rather than a
    /// silent no-op: returning the stored row would tell a caller its decision
    /// landed when somebody else's did.
    fn replayed_decision(
        &self,
        claim: &TranscriptClaim,
        wanted: TranscriptClaimStatus,
        decision: &OwnerDecision,
    ) -> Result<Option<TranscriptClaim>> {
        if claim.status.is_open() {
            return Ok(None);
        }
        let same_decider = claim
            .decided_by
            .as_deref()
            .is_some_and(|held| held == decision.by);
        if claim.status == wanted && same_decider && claim.decision_note == decision.note {
            return Ok(Some(claim.clone()));
        }
        anyhow::bail!(
            "claim `{}` is already {} by `{}`, and a decided claim never returns to the queue. \
             A differing decision is an error rather than a silent no-op, because reporting \
             success would tell `{}` their decision landed when it did not",
            claim.claim_id,
            claim.status.as_str(),
            claim.decided_by.as_deref().unwrap_or("nobody"),
            decision.by
        );
    }

    fn append(&self, scope: &OutwardScope, record: &ClaimLogRecord) -> Result<()> {
        let path = self.claims_path(scope);
        let lock_root = path.parent().context("claim register lost its parent")?;
        let _append_guard = acquire_record_decision_lock(lock_root, "claims-log", "claim log")?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn ingestion_fingerprint(
        &self,
        scope: &OutwardScope,
        transcript_key: &str,
    ) -> Result<Option<String>> {
        let path = self.claims_path(scope);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(None);
        };
        let mut fingerprint = None;
        for record in crate::magician_v2::jsonl::parse_log_lines::<ClaimLogRecord>(&raw, &path)? {
            if let ClaimLogRecord::IngestionPrepared {
                transcript_key: held_key,
                request_fingerprint,
            } = record
            {
                if held_key == transcript_key {
                    anyhow::ensure!(
                        fingerprint
                            .as_ref()
                            .is_none_or(|held| held == &request_fingerprint),
                        "transcript `{transcript_key}` has conflicting import preparations"
                    );
                    fingerprint = Some(request_fingerprint);
                }
            }
        }
        Ok(fingerprint)
    }

    fn receipt_for_decision(
        &self,
        scope: &OutwardScope,
        decision_id: &str,
    ) -> Result<Option<ClaimDecisionReceipt>> {
        let path = self.claims_path(scope);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(None);
        };
        for record in crate::magician_v2::jsonl::parse_log_lines::<ClaimLogRecord>(&raw, &path)? {
            let receipt = match record {
                ClaimLogRecord::ConfirmedWithReceipt { receipt, .. }
                | ClaimLogRecord::RejectedWithReceipt { receipt, .. } => Some(receipt),
                ClaimLogRecord::IngestionPrepared { .. }
                | ClaimLogRecord::Extracted(_)
                | ClaimLogRecord::ConfirmationPrepared { .. }
                | ClaimLogRecord::Confirmed { .. }
                | ClaimLogRecord::Rejected { .. } => None,
            };
            if receipt
                .as_ref()
                .is_some_and(|receipt| receipt.decision_id == decision_id)
            {
                return Ok(receipt);
            }
        }
        Ok(None)
    }

    fn unresolved_confirmation_preparation(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
    ) -> Result<Option<ClaimConfirmationPreparation>> {
        let path = self.claims_path(scope);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(None);
        };
        let mut prepared = None;
        let mut completed_decisions = HashSet::new();
        for record in crate::magician_v2::jsonl::parse_log_lines::<ClaimLogRecord>(&raw, &path)? {
            match record {
                ClaimLogRecord::ConfirmationPrepared {
                    claim_id: prepared_claim_id,
                    decision_id,
                    expected_revision,
                    by,
                    note,
                    request_fingerprint,
                    prepared_at,
                } if prepared_claim_id == claim_id => {
                    prepared = Some(ClaimConfirmationPreparation {
                        decision_id,
                        expected_revision,
                        by,
                        note,
                        request_fingerprint,
                        prepared_at,
                    });
                },
                ClaimLogRecord::ConfirmedWithReceipt { receipt, .. } => {
                    completed_decisions.insert(receipt.decision_id);
                },
                _ => {},
            }
        }
        Ok(prepared.filter(|held| !completed_decisions.contains(&held.decision_id)))
    }

    /// Recover the saved command for an interactive owner who lost browser
    /// state. Only the ordinary exact-decision transition may finish it.
    pub fn pending_confirmation(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
    ) -> Result<Option<PendingClaimConfirmation>> {
        validate_scope(scope)?;
        let path = self.claims_path(scope);
        let _guard = acquire_record_decision_lock(
            path.parent().context("missing claim root")?,
            &format!("claim-{}", stable_id(claim_id)),
            "pending confirmation read",
        )?;
        let Some(claim) = self.claim(scope, claim_id)? else {
            return Ok(None);
        };
        if !claim.status.is_open() {
            return Ok(None);
        }
        let Some(prepared) = self.unresolved_confirmation_preparation(scope, claim_id)? else {
            return Ok(None);
        };
        let decision = OwnerDecision {
            by: prepared.by.clone(),
            note: prepared.note.clone(),
        };
        decision.validate()?;
        validate_claim_decision_identity(
            claim_id,
            &prepared.decision_id,
            prepared.expected_revision,
        )?;
        anyhow::ensure!(
            prepared.expected_revision == claim.revision
                && prepared.request_fingerprint
                    == claim_decision_request_fingerprint(
                        claim_id,
                        ClaimDecisionVerb::ConfirmClaim,
                        prepared.expected_revision,
                        &decision,
                    ),
            "unfinished confirmation does not match the claim revision or saved command"
        );
        Ok(Some(PendingClaimConfirmation {
            decision_id: prepared.decision_id,
            expected_revision: prepared.expected_revision,
            by: prepared.by,
            note: prepared.note,
            prepared_at: prepared.prepared_at,
        }))
    }

    /// Return the admission time of one exact unfinished confirmation.
    ///
    /// This is a recovery proof, not a fresh mutation permit. A signed caller
    /// whose response/application lifetime elapsed may use it only to finish a
    /// WAL entry that the destination durably accepted while that envelope was
    /// live. A substituted decision fails closed instead of inheriting the
    /// earlier preparation.
    pub fn exact_confirmation_prepared_at(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        decision: &OwnerDecision,
    ) -> Result<Option<DateTime<Utc>>> {
        validate_scope(scope)?;
        decision.validate()?;
        validate_claim_decision_identity(claim_id, decision_id, expected_revision)?;
        let wanted_fingerprint = claim_decision_request_fingerprint(
            claim_id,
            ClaimDecisionVerb::ConfirmClaim,
            expected_revision,
            decision,
        );
        let Some(prepared) = self.unresolved_confirmation_preparation(scope, claim_id)? else {
            return Ok(None);
        };
        if prepared.decision_id != decision_id
            || prepared.expected_revision != expected_revision
            || prepared.by != decision.by
            || prepared.note != decision.note
            || prepared.request_fingerprint != wanted_fingerprint
        {
            anyhow::bail!(
                "claim has an unfinished confirmation prepared under a different decision"
            );
        }
        Ok(Some(prepared.prepared_at))
    }

    /// Recover an already-applied exact decision without entering a mutation
    /// path. This is the only safe route for response-loss recovery after a
    /// signed proposal's admission lifetime has elapsed: absence returns
    /// `None`, never a late first application.
    ///
    /// # Why a recovery path takes a clock and writes
    ///
    /// The claim row is authoritative and lands before its journal entry, so a
    /// crash in between leaves a completion no cursor would ever surface. The
    /// mutation path repairs that on its replay branch — but only if it is
    /// re-entered, and this route exists exactly for callers who no longer may.
    /// Returning the receipt and journalling nothing would make the hole
    /// permanent, which is the one failure a receipt projector may not have.
    /// [`EvidenceCompletionJournal::record_completion`] is idempotent by
    /// `(family, decision id)`, so the ordinary already-journalled case
    /// appends nothing.
    ///
    /// This does not weaken the no-late-application rule: the append repairs
    /// the index of a decision the register is proved above to already hold,
    /// and mints nothing. A journal failure is reported rather than swallowed,
    /// for the same reason the mutation path reports one — the caller retries
    /// the recovery, which is safe to repeat.
    pub fn recover_claim_decision(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        expected_revision: u64,
        decision_id: &str,
        verb: ClaimDecisionVerb,
        decision: &OwnerDecision,
        now: DateTime<Utc>,
    ) -> Result<Option<ClaimDecisionOutcome>> {
        validate_scope(scope)?;
        decision.validate()?;
        validate_claim_decision_identity(claim_id, decision_id, expected_revision)?;
        let Some(receipt) = self.receipt_for_decision(scope, decision_id)? else {
            return Ok(None);
        };
        validate_claim_decision_receipt(
            &receipt,
            decision_id,
            claim_id,
            verb,
            expected_revision,
            decision,
        )?;
        let claim = self
            .claim(scope, claim_id)?
            .context("claim decision receipt points at a missing claim")?;
        validate_claim_outcome_against_receipt(&claim, &receipt, decision)?;
        // After every validation and never before: journalling a receipt this
        // register has not been proved to hold would announce a completion that
        // did not happen, which is worse than the hole it repairs.
        self.journal_completion(scope, &receipt, now)?;
        Ok(Some(ClaimDecisionOutcome {
            claim,
            receipt: replayed_claim_receipt(receipt),
        }))
    }

    /// The exact stored receipt one completion-journal entry addresses.
    ///
    /// [`Self::recover_claim_decision`] cannot serve a projector: it demands
    /// the verb and the full owner decision, and a cursor carries a completion
    /// rather than the command that caused it. So this read binds what a
    /// journal entry *does* know — the claim the receipt has to sit on and the
    /// decision it has to name — and refuses a locator pointing at some other
    /// claim's receipt.
    ///
    /// Two deliberate differences from the recovery path, both required by the
    /// projector rather than convenient for it:
    ///
    /// * **It returns the record as stored, not the replay view.** A projection
    ///   that re-read the same completion must produce the same row, and the
    ///   replay view rewrites the disposition to `already_applied`.
    /// * **It does not journal.** A consumer that appended an entry for every
    ///   entry it drained would extend the log it is draining, forever.
    pub fn journalled_claim_receipt(
        &self,
        scope: &OutwardScope,
        claim_id: &str,
        decision_id: &str,
    ) -> Result<Option<ClaimDecisionReceipt>> {
        validate_scope(scope)?;
        let Some(receipt) = self.receipt_for_decision(scope, decision_id)? else {
            return Ok(None);
        };
        if receipt.claim_id != claim_id {
            anyhow::bail!(
                "claim decision `{decision_id}` is recorded against claim `{}`, not `{claim_id}`",
                receipt.claim_id
            );
        }
        Ok(Some(receipt))
    }

    /// Complete durable receipt stream for rebuilding the package-owned audit
    /// projection. Ordering is the authoritative log order.
    pub fn decision_receipts(&self, scope: &OutwardScope) -> Result<Vec<ClaimDecisionReceipt>> {
        // Fold first so every receipted event is checked against its prior
        // head, event fields, receipt digest material, and resulting terminal
        // claim before any audit projector can observe the raw receipt stream.
        let _validated_heads = self.claims(scope)?;
        let path = self.claims_path(scope);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Vec::new());
        };
        Ok(
            crate::magician_v2::jsonl::parse_log_lines::<ClaimLogRecord>(&raw, &path)?
                .into_iter()
                .filter_map(|record| match record {
                    ClaimLogRecord::ConfirmedWithReceipt { receipt, .. }
                    | ClaimLogRecord::RejectedWithReceipt { receipt, .. } => Some(receipt),
                    _ => None,
                })
                .collect(),
        )
    }
}

// ── The staged-ingest consumer ───────────────────────────────────────────────

/// Ceiling on the speaker keys one staged document may declare.
///
/// These ceilings are the package's own
/// (`magician_data_v3/system/claims_review/app/workflows/stage-ingest.md`),
/// restated here because a host that trusted the package to have applied them
/// would be trusting the side of the boundary that cannot be trusted. A staged
/// row is data an installed app wrote; every bound it claims to honour is
/// re-checked before the words reach the register.
pub const MAX_STAGED_INGEST_SPEAKERS: usize = 64;

/// Ceiling on the ordered utterances one staged document may carry.
pub const MAX_STAGED_INGEST_UTTERANCES: usize = 1_000;

/// Ceiling on the raw transcript the mapping was read from, and on the total
/// words the mapping assigns.
///
/// One number for both, because the mapped utterances are a transcription of
/// that same text: a document whose words outweigh the transcript they came
/// from is not a mapping of it.
pub const MAX_STAGED_INGEST_TRANSCRIPT_BYTES: usize = 131_072;

/// Ceiling on the mapping document itself, checked **before** it is parsed.
///
/// Twice the transcript ceiling: the room the per-utterance JSON framing and
/// the speaker keys need, and no more. A parser handed an unbounded string lets
/// the app decide how much memory the host spends.
pub const MAX_STAGED_INGEST_DOCUMENT_BYTES: usize = 2 * MAX_STAGED_INGEST_TRANSCRIPT_BYTES;

/// Ceiling on the owner's stated reason for calling the room outward.
pub const MAX_STAGED_INGEST_REASON_BYTES: usize = 2_000;

/// The closed field vocabulary of one staged `ingest_request` row.
///
/// Nothing outside this list may appear, for the reason the signed decision
/// seam compares its source head: a row carrying a field this host does not
/// know about was written against a different contract, and applying it would
/// ingest words under terms nobody here reviewed.
const STAGED_INGEST_ROW_FIELDS: [&str; 10] = [
    "ingest_id",
    "transcript_text",
    "speaker_mapping_json",
    "audience_kind",
    "audience_id",
    "outwardness_reason",
    "actor_ref",
    "apply_state",
    "act_ref",
    "submitted_at",
];

/// The two row fields only the host may ever fill in.
///
/// The package declares both `nullable` and not `required`, so its entity
/// contract stores a row that simply omits them exactly as happily as one that
/// writes an explicit null — no normalisation happens anywhere between the
/// mutation and the read that returns the payload verbatim. Both shapes say
/// the same thing to this host: no application has happened. Demanding the
/// literal null would rest the whole apply lane on a sentence of prose in the
/// package's `stage-ingest` workflow, so one ordinarily-authored row would be
/// permanently unapplyable. Absence and null are therefore read alike, and the
/// rule that actually matters — a row may not arrive already claiming an
/// application — is unchanged: a *value* here is still refused.
const STAGED_INGEST_HOST_STAMPED_FIELDS: [&str; 2] = ["actor_ref", "act_ref"];

/// The claims-review package entity one staged request is a row of.
///
/// Named beside the closed field set it has to agree with rather than at the
/// route, because the two are one fact: a row this file stopped recognising is
/// what [`StagedIngestRequest::from_staged_row`] refuses, and a route spelling
/// the entity name itself could read rows nothing here ever checked.
pub const STAGED_INGEST_REQUEST_ENTITY: &str = "ingest_request";

/// Namespace for the transcript key a staged request resolves to.
///
/// A staged request and a host-route transcript may never share a key: the act
/// ref derives from it, so one namespace would let an app-chosen `ingest_id`
/// land on an act somebody else recorded — adopting it when the words happen to
/// match, and refusing with a confusing "different words" error when they do
/// not.
const STAGED_INGEST_TRANSCRIPT_PREFIX: &str = "staged-ingest:";

fn staged_transcript_key(ingest_id: &str) -> String {
    format!("{STAGED_INGEST_TRANSCRIPT_PREFIX}{ingest_id}")
}

/// The segment key for the utterance at `index`.
///
/// Positional, zero padded so lexicographic order is transcript order. The
/// candidate claim ref derives from it, so inserting an utterance and restaging
/// under the same `ingest_id` shifts every later key — which the canonical
/// entry then catches as changed words under a key that already exists, rather
/// than quietly queueing a second copy of the room.
fn staged_segment_key(index: usize) -> String {
    format!("u{:04}", index + 1)
}

/// Which side of the room the host placed a named person on.
///
/// Two states, not three. [`SpeakerAttribution::Unresolved`] is the honest
/// answer for a diariser that could not tell; there is no diariser here, so an
/// unplaceable name is a refusal rather than a third state — see
/// [`StagedIngestRoster`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedIngestSide {
    /// One of our own identities. The only side whose words can become a
    /// candidate claim, because an outward assertion answers *"what did WE tell
    /// whom"*.
    Ours,
    /// Somebody on the other side.
    Counterparty,
}

/// Which named people are ours and which are the counterparty, **as the host
/// resolved them**.
///
/// This is the security boundary of the whole staged path. The staged document
/// maps a speaker key to a named person, and that mapping is a person's typed
/// intent — but nothing in it says which side that person is on, and it must
/// not: [`SpeakerAttribution::Ours`] is the only attribution that can become an
/// outward assertion, so a document that could declare its own sides would let
/// an installed app put words in our mouths by naming a speaker "ours".
///
/// So the sides come from here, and this may only be built from host identity
/// resolution. Names are compared exactly — folding case or trimming would be a
/// guess about when two identities are one, and the registers this feeds
/// compare identities exactly too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StagedIngestRoster {
    sides: BTreeMap<String, StagedIngestSide>,
}

impl StagedIngestRoster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place a named person on our side.
    pub fn resolved_ours(self, identity: impl Into<String>) -> Result<Self> {
        self.place(identity.into(), StagedIngestSide::Ours)
    }

    /// Place a named person on the other side.
    pub fn resolved_counterparty(self, identity: impl Into<String>) -> Result<Self> {
        self.place(identity.into(), StagedIngestSide::Counterparty)
    }

    /// The side the host placed this person on, or `None`.
    ///
    /// **`None` is "the host cannot place them", never a default.** Every
    /// caller here turns it into a refusal.
    pub fn side_of(&self, identity: &str) -> Option<StagedIngestSide> {
        self.sides.get(identity).copied()
    }

    fn place(mut self, identity: String, side: StagedIngestSide) -> Result<Self> {
        require_named("a roster identity", &identity)?;
        refuse_separator("a roster identity", &identity)?;
        // A silent overwrite would let the last builder call decide whether
        // somebody's words are ours, and two calls disagreeing is exactly the
        // case where nobody should be picking one.
        let held = self.sides.insert(identity.clone(), side);
        if held.is_some_and(|held| held != side) {
            anyhow::bail!(
                "the host roster places `{identity}` on both sides of the room. A contradictory \
                 resolution is not a preference between two answers; it means the identity \
                 register cannot say whose words these were"
            );
        }
        Ok(self)
    }
}

/// Speaker keys mapped to named people, with a repeated key refused rather than
/// collapsed.
///
/// `serde_json` keeps the last value for a repeated key. A document naming one
/// key twice, with a different person each time, would deserialise to whichever
/// the writer put last — so the reviewer who read the first mapping would have
/// approved an attribution the ingest did not use.
///
/// The inner map is private and there is no constructor: the only way to obtain
/// one is to deserialise a document, which is what keeps
/// [`StagedIngestDocument`] unforgeable despite its public fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StagedSpeakerMap(BTreeMap<String, String>);

impl StagedSpeakerMap {
    /// The named person a speaker key stands for, or `None` if the document
    /// never defined it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every `(key, named person)` pair, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, person)| (key.as_str(), person.as_str()))
    }

    /// Every named person, in speaker-key order. Deterministic, because the
    /// order becomes the act's audience list.
    pub fn people(&self) -> impl Iterator<Item = &str> {
        self.0.values().map(String::as_str)
    }
}

impl Serialize for StagedSpeakerMap {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StagedSpeakerMap {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DistinctKeys;

        impl<'de> serde::de::Visitor<'de> for DistinctKeys {
            type Value = StagedSpeakerMap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a speaker map whose keys are distinct")
            }

            fn visit_map<A>(self, mut access: A) -> std::result::Result<StagedSpeakerMap, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut speakers = BTreeMap::new();
                while let Some((key, person)) = access.next_entry::<String, String>()? {
                    if speakers.insert(key.clone(), person).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "speaker key `{key}` is defined twice: the last definition would \
                             silently win, so the mapping a reviewer read would not be the \
                             mapping the ingest used"
                        )));
                    }
                }
                Ok(StagedSpeakerMap(speakers))
            }
        }

        deserializer.deserialize_map(DistinctKeys)
    }
}

/// One explicitly attributed utterance from a staged document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIngestUtterance {
    /// A key defined in the document's speaker map. Never a name, never a
    /// label lifted out of the prose.
    pub speaker: String,
    pub text: String,
}

/// The staged utterance document: an explicit speaker map and an ordered list
/// that assigns every utterance to one of its keys.
///
/// Closed on both ends — an unknown field anywhere in it is a refusal — because
/// a document the host only partly understands is a document whose extra half
/// nobody reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedIngestDocument {
    pub speakers: StagedSpeakerMap,
    pub utterances: Vec<StagedIngestUtterance>,
}

impl StagedIngestDocument {
    /// Parse and check one `speaker_mapping_json` value.
    ///
    /// Refuses before allocating anything large, then refuses every document
    /// that would need a guess to ingest: an utterance whose speaker key the
    /// map never defined, a repeated key, a wordless utterance, a mapping that
    /// names nobody.
    pub fn parse(speaker_mapping_json: &str) -> Result<Self> {
        if speaker_mapping_json.len() > MAX_STAGED_INGEST_DOCUMENT_BYTES {
            anyhow::bail!(
                "a staged speaker mapping is {} bytes; the reviewed ceiling is {}",
                speaker_mapping_json.len(),
                MAX_STAGED_INGEST_DOCUMENT_BYTES
            );
        }
        let document: Self = serde_json::from_str(speaker_mapping_json).context(
            "the staged speaker mapping is not the closed {speakers, utterances} document this \
             host applies",
        )?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<()> {
        if self.speakers.is_empty() {
            anyhow::bail!(
                "a staged mapping names nobody. Attribution in this path comes from the mapping \
                 and from nothing else, so a document with no speakers cannot attribute a single \
                 word"
            );
        }
        if self.speakers.len() > MAX_STAGED_INGEST_SPEAKERS {
            anyhow::bail!(
                "a staged mapping names {} speakers; the reviewed ceiling is {}",
                self.speakers.len(),
                MAX_STAGED_INGEST_SPEAKERS
            );
        }
        for (key, person) in self.speakers.entries() {
            require_named("a staged speaker key", key)?;
            require_named("the named person a staged speaker key stands for", person)?;
        }
        if self.utterances.len() > MAX_STAGED_INGEST_UTTERANCES {
            anyhow::bail!(
                "a staged mapping carries {} utterances; the reviewed ceiling is {}",
                self.utterances.len(),
                MAX_STAGED_INGEST_UTTERANCES
            );
        }
        let mut words = 0usize;
        for (index, utterance) in self.utterances.iter().enumerate() {
            if self.speakers.get(&utterance.speaker).is_none() {
                anyhow::bail!(
                    "staged utterance {} is attributed to speaker key `{}`, which the mapping \
                     never defines. An unmapped utterance is the one thing this document exists \
                     to make impossible: there is no rule here that would work out who said it",
                    index + 1,
                    utterance.speaker
                );
            }
            require_named(
                &format!("the words of staged utterance {}", index + 1),
                &utterance.text,
            )?;
            words = words.saturating_add(utterance.text.len());
        }
        if words > MAX_STAGED_INGEST_TRANSCRIPT_BYTES {
            anyhow::bail!(
                "a staged mapping assigns {words} bytes of words; the reviewed transcript \
                 ceiling is {MAX_STAGED_INGEST_TRANSCRIPT_BYTES}"
            );
        }
        Ok(())
    }
}

/// One staged `ingest_request` row, read and checked.
///
/// Fields are private and [`Self::from_staged_row`] is the only constructor, so
/// there is no way to reach [`StagedIngestConsumer::apply`] with a request that
/// skipped the closed-shape check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIngestRequest {
    ingest_id: String,
    /// The raw text the person mapped from. Checked against the reviewed
    /// ceiling and **never parsed**: reading prose to work out who spoke is the
    /// exact act the staged document exists to replace.
    transcript_text: String,
    document: StagedIngestDocument,
    audience: AudienceRef,
    /// Why the owner called this room outward.
    ///
    /// Required — an ingest with no stated judgement is the auto-feed the
    /// substrate refuses, arriving by hand — but deliberately not copied
    /// anywhere by this consumer. The package's own row is where it is durable,
    /// and a second copy on the act would be a second authority for what the
    /// owner said.
    outwardness_reason: String,
}

impl StagedIngestRequest {
    /// Read one staged row, exactly as the package wrote it.
    ///
    /// Only a row still at `apply_state: recorded` whose host-stamped
    /// `actor_ref` and `act_ref` carry no value — absent or null, which the
    /// package's schema makes interchangeable — is admissible. An `applied` row
    /// already names its act; a `refused` one was refused on purpose, so a host
    /// that means to retry a transient refusal leaves its row at `recorded`
    /// rather than stamping it and losing the ability to; and a row that
    /// arrived with the host's own fields already filled in is claiming an
    /// application that never happened.
    pub fn from_staged_row(row: &serde_json::Value) -> Result<Self> {
        let object = row.as_object().ok_or_else(|| {
            anyhow::anyhow!("a staged ingest row is a JSON object with the closed package shape")
        })?;
        if let Some(unknown) = object
            .keys()
            .find(|key| !STAGED_INGEST_ROW_FIELDS.contains(&key.as_str()))
        {
            anyhow::bail!(
                "the staged ingest row carries `{unknown}`, which is not in the closed package \
                 shape ({}). A row with a field this host does not know about was written \
                 against a different contract, and applying it would ingest words under terms \
                 nobody here reviewed",
                STAGED_INGEST_ROW_FIELDS.join(", ")
            );
        }
        for field in STAGED_INGEST_ROW_FIELDS {
            if STAGED_INGEST_HOST_STAMPED_FIELDS.contains(&field) {
                continue;
            }
            if !object.contains_key(field) {
                anyhow::bail!(
                    "the staged ingest row is missing `{field}`, so it does not have the closed \
                     package shape ({}). Every field the package itself writes must be there to \
                     read; only the host's own fields may be absent",
                    STAGED_INGEST_ROW_FIELDS.join(", ")
                );
            }
        }
        // No return-type annotation: it would tie the borrowed `&str` to a
        // fresh lifetime instead of `object`'s.
        let text = |field: &'static str| {
            object
                .get(field)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("the staged ingest row field `{field}` is not text"))
        };

        let apply_state = text("apply_state")?;
        if apply_state != "recorded" {
            anyhow::bail!(
                "the staged ingest row is `{apply_state}`, not `recorded`. Only a row that has \
                 not been applied may be applied"
            );
        }
        for field in STAGED_INGEST_HOST_STAMPED_FIELDS {
            // Absent and null are the same statement here; only a value is a
            // claim. See `STAGED_INGEST_HOST_STAMPED_FIELDS`.
            if !matches!(object.get(field), None | Some(&serde_json::Value::Null)) {
                anyhow::bail!(
                    "the staged ingest row already carries `{field}`. That is one of the host's \
                     fields, stamped from the authenticated actor and the durable act; a row \
                     that arrived with it filled in is asserting an application nobody performed"
                );
            }
        }
        require_named("a staged ingest's submitted_at", text("submitted_at")?)?;

        let ingest_id = text("ingest_id")?;
        require_named("a staged ingest id", ingest_id)?;
        // `is_safe_scope_id` already excludes every control character, so the
        // separator check the derived-id helpers rely on is covered by it.
        if ingest_id.len() > 192 || !is_safe_scope_id(ingest_id) {
            anyhow::bail!(
                "a staged ingest id must be a safe identifier of at most 192 bytes: it becomes \
                 the transcript key the act ref derives from"
            );
        }

        let transcript_text = text("transcript_text")?;
        require_named("the staged transcript text", transcript_text)?;
        if transcript_text.len() > MAX_STAGED_INGEST_TRANSCRIPT_BYTES {
            anyhow::bail!(
                "a staged transcript is {} bytes; the reviewed ceiling is {}",
                transcript_text.len(),
                MAX_STAGED_INGEST_TRANSCRIPT_BYTES
            );
        }

        let audience_kind = text("audience_kind")?;
        let audience_id = text("audience_id")?;
        let kind = AudienceKind::parse(audience_kind).ok_or_else(|| {
            anyhow::anyhow!(
                "`{audience_kind}` is not an audience kind. Reading an unrecognised word as a \
                 default would file the room against a relationship nobody named"
            )
        })?;
        require_named("a staged ingest's audience id", audience_id)?;
        refuse_separator("a staged ingest's audience id", audience_id)?;

        let outwardness_reason = text("outwardness_reason")?;
        require_named(
            "the owner's reason for calling this room outward",
            outwardness_reason,
        )?;
        if outwardness_reason.len() > MAX_STAGED_INGEST_REASON_BYTES {
            anyhow::bail!(
                "a staged outwardness reason is {} bytes; the reviewed ceiling is {}",
                outwardness_reason.len(),
                MAX_STAGED_INGEST_REASON_BYTES
            );
        }

        Ok(Self {
            ingest_id: ingest_id.to_string(),
            transcript_text: transcript_text.to_string(),
            document: StagedIngestDocument::parse(text("speaker_mapping_json")?)?,
            audience: AudienceRef::new(kind, audience_id),
            outwardness_reason: outwardness_reason.to_string(),
        })
    }

    pub fn ingest_id(&self) -> &str {
        &self.ingest_id
    }

    pub fn audience(&self) -> &AudienceRef {
        &self.audience
    }

    pub fn document(&self) -> &StagedIngestDocument {
        &self.document
    }

    pub fn outwardness_reason(&self) -> &str {
        &self.outwardness_reason
    }

    /// The raw text, for a surface that wants to show what was mapped from. No
    /// consumer here reads it as anything but bytes.
    pub fn transcript_text(&self) -> &str {
        &self.transcript_text
    }
}

/// Everything about a staged ingest that only the host may say.
///
/// The split is the point: the package supplies the words and the mapping, and
/// the host supplies who those people are, when the room happened, and what it
/// cost. Nothing an installed app writes can reach any field on this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIngestHostContext {
    /// The authenticated actor applying the request, recorded as the extractor
    /// that produced the candidates.
    ///
    /// That is the strongest true statement available: the staged row carries
    /// no `actor_ref` until apply, so the host cannot prove who did the
    /// mapping, only who is applying it. Recording the applier means the
    /// applier cannot then confirm what it just queued — the self-grant rule
    /// this register turns on.
    pub applied_by: String,
    /// Our side's identity in the room. Lands as the act's effective sender,
    /// and must be one the roster places on our side.
    pub effective_speaker: String,
    /// Which named people are ours and which are the other side.
    pub roster: StagedIngestRoster,
    /// What this room cost if what was said in it was wrong. Supplied, never
    /// invented: the staged row has no field for it, and understating the class
    /// is the direction that misleads a later reviewer.
    pub consequence_class: ConsequenceClass,
    /// When the words were said, which is neither when they were staged nor
    /// when they are being applied.
    pub occurred_at: DateTime<Utc>,
}

impl StagedIngestHostContext {
    /// A context for a room recorded as bounded communication — words said to
    /// people already in it. A caller that knows more raises the class.
    pub fn resolved(
        applied_by: impl Into<String>,
        effective_speaker: impl Into<String>,
        roster: StagedIngestRoster,
        occurred_at: DateTime<Utc>,
    ) -> Self {
        Self {
            applied_by: applied_by.into(),
            effective_speaker: effective_speaker.into(),
            roster,
            consequence_class: ConsequenceClass::BoundedCommunication,
            occurred_at,
        }
    }

    fn validate(&self) -> Result<()> {
        require_named("the actor applying a staged ingest", &self.applied_by)?;
        refuse_separator("the actor applying a staged ingest", &self.applied_by)?;
        require_named("our side's identity in the room", &self.effective_speaker)?;
        if self.roster.side_of(&self.effective_speaker) != Some(StagedIngestSide::Ours) {
            anyhow::bail!(
                "`{}` is named as our side's identity in the room, but the host roster does not \
                 place them on our side. The act records this identity as its effective sender, \
                 so an unplaced or counterparty name there would file the other side's words as \
                 our own disclosure",
                self.effective_speaker
            );
        }
        if self.consequence_class == ConsequenceClass::PrivateLocal {
            anyhow::bail!(
                "a transcript of an observed room is not private/local: the words already \
                 reached somebody. Recording it as the one class that needs no gate would put an \
                 outward act outside every later review"
            );
        }
        Ok(())
    }
}

/// What applying a staged request produced.
#[derive(Debug, Clone)]
pub struct StagedIngestApplication {
    pub ingest_id: String,
    /// The namespaced key the act ref derives from. Returned so an operator can
    /// find the act by hand.
    pub transcript_key: String,
    /// What the canonical entry recorded. Act-always: the disclosure inside is
    /// there whether or not one word of the room produced a candidate.
    pub ingested: IngestedTranscript,
}

impl StagedIngestApplication {
    /// The act ref — the only value that may move a staged row from `recorded`
    /// to `applied`. A claim count never can: an empty extraction is a real
    /// outcome of a real ingest.
    pub fn outward_act_ref(&self) -> &str {
        &self.ingested.disclosure.outward_act_ref
    }

    /// When the act was **first** recorded, verbatim from the durable row.
    ///
    /// A replay of one `ingest_id` resumes the same act, so this is the original
    /// time rather than the applying caller's clock. It is the honest "has this
    /// landed before" signal — a fact read off the act, rather than a
    /// disposition this consumer would have to invent.
    pub fn act_recorded_at(&self) -> &str {
        &self.ingested.disclosure.prepared_at
    }
}

/// The consumer that turns a staged `ingest_request` row into a real ingest.
///
/// Gate E3 of the apps-platform register
/// (`docs/plans/2026-09-03-apps_platform_open-gates.md`), and the ingest half of
/// the evidence increment's blocking reopen: *"raw paste/upload cannot
/// truthfully supply attribution to the canonical ingest API. The package
/// stages an explicitly mapped, ordered utterance document and never guesses
/// speakers. Canonical host ingest remains act-always."*
///
/// # It adds no second ingest path
///
/// Everything here ends at [`TranscriptIngestion::ingest_transcript`], the same
/// entry `magician-api/src/transcript_claims_api.rs` serves. The consumer's job
/// is to refuse everything that entry could not check for itself, not to become
/// a second place where a room is decided to be outward.
///
/// # Two mappings, and only one of them may come from the package
///
/// A staged document maps a **speaker key to a named person**; that is a
/// person's typed intent and the package owns it. Mapping a **named person to a
/// side** is a different act, and the host owns it — see
/// [`StagedIngestRoster`]. Keeping them apart is what makes the no-guessing
/// property survive contact with an installed app: the package can say "key `a`
/// is Dana", and cannot say "Dana is us".
///
/// # An unplaceable speaker refuses the request
///
/// The runtime path has [`SpeakerAttribution::Unresolved`] for a diariser that
/// could not tell who spoke, and skips those utterances with a reason. There is
/// no diariser here — every speaker was named on purpose — so a name the host
/// cannot place is a disagreement with the identity register, not a miss.
/// Skipping it would drop that person's words out of the review queue while the
/// reviewer believed the whole room was staged, so the whole request refuses
/// and no act is written. `refused` is a state the package's own row already
/// has.
///
/// # The act is the receipt
///
/// There is no ledger here. The act row is durable, content-addressed, and
/// resumes under one `ingest_id`, which is everything a receipt is for; a
/// second host-side record of "this ingest happened" would be a second
/// authority for the same fact, disagreeing with the first the moment one write
/// is interrupted.
#[derive(Debug, Clone)]
pub struct StagedIngestConsumer {
    ingestion: TranscriptIngestion,
}

impl StagedIngestConsumer {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            ingestion: TranscriptIngestion::new(workspace_layout),
        }
    }

    /// The consumer over an existing register, so a caller holding one does not
    /// hand a second workspace layout to disagree with.
    pub fn over(ingestion: &TranscriptIngestion) -> Self {
        Self {
            ingestion: ingestion.clone(),
        }
    }

    /// Apply one staged request through the canonical ingestion entry.
    ///
    /// Idempotent by the same mechanism as the entry beneath it: one
    /// `ingest_id` derives one transcript key, one act and one set of claim
    /// ids, so a replay resumes rather than queueing the room twice. Restaging
    /// the same id with different words is refused there, not repaired here.
    pub fn apply(
        &self,
        scope: &OutwardScope,
        request: &StagedIngestRequest,
        context: &StagedIngestHostContext,
        now: DateTime<Utc>,
    ) -> Result<StagedIngestApplication> {
        context.validate()?;

        // One pass over the mapping, before any write: every named person must
        // be placeable, and everyone but our own speaker is in the room to be
        // told. Refusing here rather than at the first utterance that mentions
        // an unplaceable person means the message names the person, and means a
        // room whose LAST speaker is unplaceable refuses just as early as one
        // whose first is.
        let mut attendees: Vec<String> = Vec::new();
        for person in request.document.speakers.people() {
            if context.roster.side_of(person).is_none() {
                anyhow::bail!(
                    "the staged mapping names `{person}`, and the host cannot place them on \
                     either side of the room. A staged mapping is typed by a person on purpose, \
                     so an unplaceable name is a disagreement with the identity register rather \
                     than a diarisation miss: ingesting anyway would drop their words out of \
                     review while the reviewer believed the whole room was queued"
                );
            }
            // The speaker is not their own audience — the assertion rows are
            // "who we told", one each, and a later correction is aimed at them.
            if person != context.effective_speaker && !attendees.iter().any(|held| held == person) {
                attendees.push(person.to_string());
            }
        }

        let mut utterances = Vec::with_capacity(request.document.utterances.len());
        for (index, staged) in request.document.utterances.iter().enumerate() {
            let Some(person) = request.document.speakers.get(&staged.speaker) else {
                anyhow::bail!(
                    "staged utterance {} names speaker key `{}`, which the mapping never defines",
                    index + 1,
                    staged.speaker
                );
            };
            let speaker = match context.roster.side_of(person) {
                Some(StagedIngestSide::Ours) => SpeakerAttribution::Ours(person.to_string()),
                Some(StagedIngestSide::Counterparty) => {
                    SpeakerAttribution::Counterparty(person.to_string())
                },
                // Unreachable after the pass above, and it stays a refusal
                // rather than an `Unresolved` anyway: this consumer has no
                // branch that can attribute a word to a person nobody placed.
                None => anyhow::bail!(
                    "the staged mapping names `{person}`, whom the host cannot place on either \
                     side of the room"
                ),
            };
            utterances.push(TranscriptUtterance::said(
                staged_segment_key(index),
                speaker,
                staged.text.clone(),
            ));
        }

        let transcript_key = staged_transcript_key(&request.ingest_id);
        let mut source = TranscriptSource::observed(
            transcript_key.clone(),
            context.effective_speaker.clone(),
            attendees,
            context.applied_by.clone(),
            context.occurred_at,
        );
        // `Meeting`, for the reason the owner route hardcodes it: the store
        // refuses a CONTROLLED channel — one the runtime could have recorded
        // before the words left — and letting a staged row name the channel
        // would let an app file a mail we composed as a room we merely
        // overheard, which is the one direction that erases a disclosure.
        source.channel = OutwardChannel::Meeting;
        source.audience = Some(request.audience.clone());
        // Left unset on purpose. The audience ref carries the relationship, and
        // an observed act "has no relationship to name" — deriving a programme
        // or engagement id from one would be the guess the disclosure record
        // refuses to make on its own.
        source.engagement_id = None;
        source.program_id = None;
        source.consequence_class = context.consequence_class;

        let ingested = self
            .ingestion
            .ingest_transcript(scope, &source, &utterances, now)
            .with_context(|| format!("applying staged ingest `{}`", request.ingest_id))?;

        Ok(StagedIngestApplication {
            ingest_id: request.ingest_id.clone(),
            transcript_key,
            ingested,
        })
    }
}

/// Build the commitment request a confirmed claim stands for.
///
/// Separated from the write so the refusals are testable on their own and so a
/// caller with its own register can use the mapping without this module's
/// storage. Every refusal here is a case where recording would assert something
/// nobody agreed to.
pub fn commitment_request_from_claim(claim: &TranscriptClaim) -> Result<RecordCommitment> {
    match claim.status {
        TranscriptClaimStatus::Confirmed => {},
        TranscriptClaimStatus::Pending => anyhow::bail!(
            "claim `{}` is still pending. A commitment built on an unconfirmed extraction is a \
             model's reading of a room entered in the register as a term, and the register is \
             what a later message is composed from",
            claim.claim_id
        ),
        TranscriptClaimStatus::Rejected => anyhow::bail!(
            "claim `{}` was rejected: somebody looked at the words and said we did not say them. \
             A rejected claim is terminal and cannot become a commitment",
            claim.claim_id
        ),
    }
    let Some(audience) = claim.audience_ref.clone() else {
        anyhow::bail!(
            "claim `{}` names no relationship. A commitment has to be filed against one, or \
             nobody preparing for the next conversation can find it",
            claim.claim_id
        );
    };
    if !audience.is_named() {
        anyhow::bail!(
            "claim `{}` names an empty relationship id, which files the term where no lookup \
             will reach it",
            claim.claim_id
        );
    }
    Ok(RecordCommitment {
        audience,
        // The act, not a summary: the owner reads the exact words through the
        // disclosure's immutable payload before confirming anything.
        source_ref: claim.outward_act_ref.clone(),
        // Always ours. Only utterances resolved to one of our own identities
        // become claims here, so there is no branch that could record the other
        // side's offer as something we said.
        direction: CommitmentDirection::StatedByUs,
        terms: claim.stated_text.clone(),
        stated_at: claim.stated_at,
    })
}

fn validate_claim_decision_identity(
    claim_id: &str,
    decision_id: &str,
    expected_revision: u64,
) -> Result<()> {
    if !is_safe_scope_id(claim_id) {
        anyhow::bail!("claim id failed the safe-identifier check");
    }
    if decision_id.trim().is_empty() || decision_id.len() > 192 || !is_safe_scope_id(decision_id) {
        anyhow::bail!("decision id must be a non-empty safe identifier no longer than 192 bytes");
    }
    if expected_revision == 0 {
        anyhow::bail!("expected claim revision must be positive");
    }
    Ok(())
}

fn validate_claim_decision_receipt(
    receipt: &ClaimDecisionReceipt,
    decision_id: &str,
    claim_id: &str,
    verb: ClaimDecisionVerb,
    expected_revision: u64,
    decision: &OwnerDecision,
) -> Result<()> {
    decision.validate()?;
    let resulting_revision = expected_revision
        .checked_add(1)
        .context("claim receipt revision overflow")?;
    let expected = claim_decision_receipt(
        decision_id,
        claim_id,
        verb,
        expected_revision,
        resulting_revision,
        &decision.by,
        claim_decision_request_fingerprint(claim_id, verb, expected_revision, decision),
        receipt.recorded_at.to_owned(),
    );
    if receipt != &expected {
        // Name the cause. A stored receipt that does not re-derive from the
        // request now being presented means the decision was SUBSTITUTED --
        // a different actor, note, verb or revision replayed under a decision
        // id that already resolved. "Does not match its exact request" is true
        // but tells an operator nothing about what to look for.
        anyhow::bail!(
            "claim decision receipt was minted for a different request; a substituted decision \
             cannot inherit an existing decision id"
        );
    }
    Ok(())
}

fn validate_claim_outcome_against_receipt(
    claim: &TranscriptClaim,
    receipt: &ClaimDecisionReceipt,
    decision: &OwnerDecision,
) -> Result<()> {
    let expected_status = match receipt.verb {
        ClaimDecisionVerb::ConfirmClaim => TranscriptClaimStatus::Confirmed,
        ClaimDecisionVerb::RejectClaim => TranscriptClaimStatus::Rejected,
    };
    let assertion_shape_is_valid = match receipt.verb {
        ClaimDecisionVerb::ConfirmClaim => !claim.assertion_use_ids.is_empty(),
        ClaimDecisionVerb::RejectClaim => claim.assertion_use_ids.is_empty(),
    };
    if claim.claim_id != receipt.claim_id
        || claim.status != expected_status
        || claim.revision != receipt.resulting_revision
        || claim.decided_by.as_deref() != Some(decision.by.as_str())
        || claim.decision_note != decision.note
        || claim.decided_at.as_ref() != Some(&receipt.recorded_at)
        || !assertion_shape_is_valid
    {
        anyhow::bail!("claim decision receipt does not match the authoritative claim head");
    }
    Ok(())
}

fn claim_decision_receipt(
    decision_id: &str,
    claim_id: &str,
    verb: ClaimDecisionVerb,
    expected_revision: u64,
    resulting_revision: u64,
    by: &str,
    request_fingerprint: String,
    recorded_at: DateTime<Utc>,
) -> ClaimDecisionReceipt {
    let verb_name = match verb {
        ClaimDecisionVerb::ConfirmClaim => "confirm_claim",
        ClaimDecisionVerb::RejectClaim => "reject_claim",
    };
    let material = format!(
        "{decision_id}{FIELD_SEP}{claim_id}{FIELD_SEP}{verb_name}{FIELD_SEP}{expected_revision}{FIELD_SEP}{resulting_revision}{FIELD_SEP}{by}{FIELD_SEP}{request_fingerprint}"
    );
    ClaimDecisionReceipt {
        receipt_id: format!("claim-receipt-{}", stable_id(&material)),
        decision_id: decision_id.to_owned(),
        claim_id: claim_id.to_owned(),
        verb,
        expected_revision,
        resulting_revision,
        by: by.to_owned(),
        request_fingerprint,
        recorded_at,
        disposition: ClaimDecisionDisposition::Applied,
    }
}

fn claim_decision_request_fingerprint(
    claim_id: &str,
    verb: ClaimDecisionVerb,
    expected_revision: u64,
    decision: &OwnerDecision,
) -> String {
    let verb_name = match verb {
        ClaimDecisionVerb::ConfirmClaim => "confirm_claim",
        ClaimDecisionVerb::RejectClaim => "reject_claim",
    };
    stable_id(&format!(
        "{claim_id}{FIELD_SEP}{verb_name}{FIELD_SEP}{expected_revision}{FIELD_SEP}{}{FIELD_SEP}{}",
        decision.by,
        decision.note.as_deref().unwrap_or_default(),
    ))
}

fn replayed_claim_receipt(mut receipt: ClaimDecisionReceipt) -> ClaimDecisionReceipt {
    receipt.disposition = ClaimDecisionDisposition::AlreadyApplied;
    receipt
}

fn legacy_claim_decision_id(
    claim_id: &str,
    verb: ClaimDecisionVerb,
    decision: &OwnerDecision,
) -> String {
    let verb_name = match verb {
        ClaimDecisionVerb::ConfirmClaim => "confirm",
        ClaimDecisionVerb::RejectClaim => "reject",
    };
    format!(
        "legacy-claim-decision-{}",
        stable_id(&format!(
            "{claim_id}{FIELD_SEP}{verb_name}{FIELD_SEP}{}{FIELD_SEP}{}",
            decision.by,
            decision.note.as_deref().unwrap_or_default()
        ))
    )
}

fn observed_statement(
    source: &TranscriptSource,
    spoken_text: &str,
    claim: Option<ExtractedClaim>,
) -> ObservedStatement {
    ObservedStatement {
        channel: source.channel,
        // One transcript is one act. The key is the transcript's, not a
        // segment's, so a room stays one disclosure however many sentences a
        // later extractor finds in it.
        statement_key: source.transcript_key.clone(),
        effective_speaker: source.effective_speaker.clone(),
        audience: source.attendees.clone(),
        spoken_text: spoken_text.to_string(),
        engagement_id: source.engagement_id.clone(),
        program_id: source.program_id.clone(),
        consequence_class: source.consequence_class,
        claim,
    }
}

/// One JSON object per utterance, in order.
///
/// The payload has to be deterministic — it is content-addressed, and the ref is
/// what makes *"what was reviewed and what was said cannot diverge"* true. It
/// carries attribution beside the words because *"who the runtime thought was
/// speaking"* is part of what the record has to preserve; a flat wall of text
/// would lose it and no later reader could reconstruct it.
fn render_transcript(utterances: &[TranscriptUtterance]) -> String {
    #[derive(Serialize)]
    struct Line<'a> {
        segment: &'a str,
        side: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        speaker: Option<&'a str>,
        said: &'a str,
    }

    let mut out = String::new();
    for utterance in utterances {
        let side = match utterance.speaker {
            SpeakerAttribution::Ours(_) => "ours",
            SpeakerAttribution::Counterparty(_) => "counterparty",
            SpeakerAttribution::Unresolved => "unresolved",
        };
        let line = Line {
            segment: &utterance.segment_key,
            side,
            speaker: utterance.speaker.identity(),
            said: &utterance.spoken_text,
        };
        // Every field is a plain string, so serialisation cannot fail; the
        // fallback keeps the render total rather than making an unrepresentable
        // case decide whether a room gets recorded at all.
        match serde_json::to_string(&line) {
            Ok(rendered) => out.push_str(&rendered),
            Err(_) => out.push_str("{}"),
        }
        out.push('\n');
    }
    out
}

fn validate_scope(scope: &OutwardScope) -> Result<()> {
    require_named("a scope principal", &scope.principal)?;
    require_named("a scope workspace", &scope.workspace)?;
    refuse_separator("a scope principal", &scope.principal)?;
    refuse_separator("a scope workspace", &scope.workspace)?;
    Ok(())
}

/// Segment keys have to be present and distinct.
///
/// Two utterances sharing a key derive one claim ref, so the second would
/// resolve to the first's claim id — and the words the decider reads would be
/// whichever landed first, for a decision that then covers both.
fn validate_segments(transcript_key: &str, utterances: &[TranscriptUtterance]) -> Result<()> {
    let mut seen = HashSet::new();
    let mut claims = HashSet::new();
    for utterance in utterances {
        require_named("a segment key", &utterance.segment_key)?;
        refuse_separator("a segment key", &utterance.segment_key)?;
        if let Some(claim_ref) = utterance.claim_ref.as_deref() {
            require_named("a claim reference", claim_ref)?;
            refuse_separator("a claim reference", claim_ref)?;
        }
        for evidence_ref in &utterance.evidence_refs {
            require_named("an evidence reference", evidence_ref)?;
        }
        if !seen.insert(utterance.segment_key.as_str()) {
            anyhow::bail!(
                "segment key `{}` appears twice in one transcript: two utterances sharing a key \
                 derive one claim, so the second would silently disappear behind the first and \
                 one decision would cover words nobody read",
                utterance.segment_key
            );
        }
        if utterance.candidate().is_ok()
            && !claims.insert(utterance.candidate_claim_ref(transcript_key))
        {
            anyhow::bail!(
                "multiple segments refer to claim `{}` in one transcript; each candidate must have a distinct claim reference",
                utterance.candidate_claim_ref(transcript_key)
            );
        }
    }
    Ok(())
}

fn require_named(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{label} must not be blank");
    }
    Ok(())
}

fn refuse_separator(label: &str, value: &str) -> Result<()> {
    if value.contains(FIELD_SEP) {
        anyhow::bail!(
            "{label} must not contain U+001F: it is the separator that keeps a derived id's \
             components apart, and a value carrying it can fuse two different records into one id"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The id for one `(scope, act, claim)` triple.
///
/// Derived rather than allocated, so *"re-ingesting a transcript resumes the
/// same queue entry"* is a property of the id rather than a lookup somebody has
/// to remember to do.
fn derive_claim_id(
    scope: &OutwardScope,
    outward_act_ref: &str,
    approved_claim_ref: &str,
) -> String {
    format!(
        "tclaim-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{outward_act_ref}{FIELD_SEP}{approved_claim_ref}",
            scope.principal, scope.workspace
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests assert on the status, so it is imported here rather than
    // at the top, where it would be an unused import in a non-test build.
    use crate::magician_v2::commitments::CommitmentStatus;

    fn ingestion() -> (tempfile::TempDir, TranscriptIngestion, OutwardScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let ingestion = TranscriptIngestion::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, ingestion, OutwardScope::new("anonymous", "default"))
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn source() -> TranscriptSource {
        let mut source = TranscriptSource::observed(
            "call-2026-08-19",
            "founder@example.com",
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string(),
            ],
            "transcript-extractor",
            at("2026-08-19T09:00:00Z"),
        );
        source.audience = Some(AudienceRef::engagement("eng-1"));
        source.engagement_id = Some("eng-1".to_string());
        source
    }

    fn ours(segment: &str, text: &str) -> TranscriptUtterance {
        TranscriptUtterance::said(
            segment,
            SpeakerAttribution::Ours("founder@example.com".to_string()),
            text,
        )
    }

    /// The act must exist whether or not a single word of the room was
    /// understood. A record that only appears when extraction succeeds
    /// disappears exactly when the extractor is worst, and *"was this ever
    /// disclosed"* would then answer "no" for the rooms most worth auditing.
    #[test]
    fn an_ingested_transcript_records_the_act_even_when_nothing_is_extractable() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![
            TranscriptUtterance::said(
                "seg-1",
                SpeakerAttribution::Counterparty("alice@example.com".to_string()),
                "what does the pipeline look like",
            ),
            TranscriptUtterance::said("seg-2", SpeakerAttribution::Unresolved, "hard to say"),
            ours("seg-3", "   "),
        ];

        let ingested = ingestion
            .ingest_transcript(&scope, &source(), &utterances, at("2026-08-19T10:00:00Z"))
            .expect("ingest");

        assert!(ingested.disclosure.observed);
        assert_eq!(ingested.disclosure.prepared_at, "2026-08-19T10:00:00+00:00");
        assert_eq!(
            ingested.disclosure.intended_audience,
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string()
            ]
        );
        assert_eq!(ingested.utterances_seen, 3);
        assert_eq!(ingested.claims.len(), 0);
        assert_eq!(
            ingested
                .skipped
                .iter()
                .map(|skipped| (skipped.segment_key.as_str(), skipped.reason))
                .collect::<Vec<_>>(),
            vec![
                ("seg-1", SkipReason::SpokenByCounterparty),
                ("seg-2", SkipReason::SpeakerUnresolved),
                ("seg-3", SkipReason::NoWords),
            ],
            "a skipped utterance is counted with its reason, never silently dropped"
        );

        assert!(ingestion
            .assertions()
            .load_act(&scope, &ingested.disclosure.outward_act_ref)
            .expect("load")
            .is_some());
    }

    /// An empty transcript is still a room somebody was in. The act is the
    /// record that it happened, and it must not depend on there being anything
    /// to extract from it.
    #[test]
    fn an_empty_transcript_records_the_act_and_returns_no_claims() {
        let (_tmp, ingestion, scope) = ingestion();

        let ingested = ingestion
            .ingest_transcript(&scope, &source(), &[], at("2026-08-19T10:00:00Z"))
            .expect("ingest");

        assert_eq!(ingested.utterances_seen, 0);
        assert_eq!(ingested.claims.len(), 0);
        assert_eq!(ingested.skipped.len(), 0);
        assert!(ingested.disclosure.observed);
        assert_eq!(
            ingestion.pending_claims(&scope).expect("queue").len(),
            0,
            "no claims, and no phantom queue entry standing in for the empty room"
        );
        assert_eq!(
            ingestion
                .assertions()
                .load_payload(&scope, &ingested.disclosure.exact_payload_artifact_ref)
                .expect("payload")
                .expect("present"),
            "",
            "the empty transcript is the payload, and it is content-addressed like any other"
        );
    }

    /// The failure this whole separation exists to prevent: a claim recorded as
    /// something the company asserted on nothing but a model's reading of a
    /// room. Ingestion has no argument that would produce one.
    #[test]
    fn a_claim_is_never_asserted_without_confirmation() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![ours("seg-1", "our revenue last quarter was 400k")];

        let ingested = ingestion
            .ingest_transcript(&scope, &source(), &utterances, at("2026-08-19T10:00:00Z"))
            .expect("ingest");

        assert_eq!(ingested.claims.len(), 1);
        let claim = &ingested.claims[0];
        assert_eq!(claim.status, TranscriptClaimStatus::Pending);
        assert_eq!(claim.stated_text, "our revenue last quarter was 400k");
        assert_eq!(claim.speaker, "founder@example.com");
        assert_eq!(
            claim.audience,
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string()
            ]
        );
        assert_eq!(claim.extracted_by, "transcript-extractor");
        assert_eq!(claim.assertion_use_ids.len(), 0);
        assert!(!claim.is_on_the_record());
        assert!(
            claim.awaiting.contains("owner confirmation"),
            "the writer's own words reach the surface verbatim: {}",
            claim.awaiting
        );

        assert_eq!(
            ingestion
                .assertions()
                .disclosures_carrying_claim(&scope, &claim.approved_claim_ref)
                .expect("reverse lookup")
                .len(),
            0,
            "nothing is in the reverse lookup, so no correction can chase anybody over it"
        );
        assert_eq!(ingestion.pending_claims(&scope).expect("queue").len(), 1);
    }

    /// Confirming is what puts a claim on the record, one row per person in the
    /// room. One row for "the room" would make a later per-person correction
    /// impossible to aim.
    #[test]
    fn confirming_writes_one_assertion_per_person_in_the_room() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();

        let confirmed = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");

        assert_eq!(confirmed.status, TranscriptClaimStatus::Confirmed);
        assert!(confirmed.is_on_the_record());
        assert_eq!(confirmed.decided_by.as_deref(), Some("owner@example.com"));
        assert_eq!(confirmed.decided_at, Some(at("2026-08-20T08:00:00Z")));
        assert_eq!(confirmed.assertion_use_ids.len(), 2);

        let mut heard: Vec<String> = ingestion
            .assertions()
            .disclosures_carrying_claim(&scope, &confirmed.approved_claim_ref)
            .expect("reverse lookup")
            .into_iter()
            .map(|row| row.audience)
            .collect();
        heard.sort();
        assert_eq!(
            heard,
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string()
            ],
            "each person in the room can be corrected individually"
        );
        assert_eq!(
            ingestion.pending_claims(&scope).expect("queue").len(),
            0,
            "a decided claim leaves the queue"
        );
    }

    /// A confirmation is somebody else's act. An extractor confirming its own
    /// reading of a room would make the one control this register has something
    /// the machine grants itself — the rule the commitments register enforces by
    /// requiring a named confirmer.
    #[test]
    fn the_extractor_may_not_confirm_its_own_extraction() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();

        let error = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("transcript-extractor"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect_err("self-grant");
        assert!(
            error.to_string().contains("cannot also confirm it"),
            "{error}"
        );

        let blank = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("   "),
                at("2026-08-20T08:00:00Z"),
            )
            .expect_err("unnamed");
        assert!(
            blank.to_string().contains("must name who took it"),
            "{blank}"
        );

        assert_eq!(
            ingestion
                .claim(&scope, &claim_id)
                .expect("load")
                .expect("present")
                .status,
            TranscriptClaimStatus::Pending,
            "a refused confirmation leaves the claim exactly where it was"
        );
    }

    /// An identical replay resumes; a differing one is an error rather than a
    /// silent no-op. Returning the stored row for a second, different decision
    /// would tell the second decider their call landed when somebody else's had.
    #[test]
    fn confirming_twice_resumes_and_a_differing_confirmation_is_an_error() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();
        let decision = OwnerDecision::by("owner@example.com").with_note("yes, I said that");

        let first = ingestion
            .confirm_claim(&scope, &claim_id, &decision, at("2026-08-20T08:00:00Z"))
            .expect("first");
        let replay = ingestion
            .confirm_claim(&scope, &claim_id, &decision, at("2026-08-21T08:00:00Z"))
            .expect("identical replay resumes");

        assert_eq!(
            first, replay,
            "the replay changes nothing, not even the timestamp"
        );
        assert_eq!(replay.decided_at, Some(at("2026-08-20T08:00:00Z")));
        assert_eq!(replay.assertion_use_ids.len(), 2);

        let other_person = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("cofounder@example.com").with_note("yes, I said that"),
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("a different decider is a different decision");
        assert!(
            other_person.to_string().contains("already confirmed"),
            "{other_person}"
        );

        let other_note = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("owner@example.com").with_note("actually I said April"),
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("a different note is a different decision");
        assert!(
            other_note
                .to_string()
                .contains("never returns to the queue"),
            "{other_note}"
        );

        assert_eq!(
            ingestion
                .claim(&scope, &claim_id)
                .expect("load")
                .expect("present")
                .decided_by
                .as_deref(),
            Some("owner@example.com"),
            "the first decision is the one on file"
        );
    }

    /// A rejected claim is terminal. Nothing may resurrect it — not a
    /// confirmation, not a second rejection with different words — because a
    /// claim that can come back is a claim somebody can grind into existence.
    #[test]
    fn a_rejected_claim_stays_rejected() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = ingested.claims[0].clone();
        let decision = OwnerDecision::by("owner@example.com").with_note("nobody said that");

        let rejected = ingestion
            .reject_claim(
                &scope,
                &claim.claim_id,
                &decision,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("reject");
        assert_eq!(rejected.status, TranscriptClaimStatus::Rejected);
        assert_eq!(rejected.assertion_use_ids.len(), 0);

        let replay = ingestion
            .reject_claim(
                &scope,
                &claim.claim_id,
                &decision,
                at("2026-08-22T08:00:00Z"),
            )
            .expect("identical replay resumes");
        assert_eq!(replay, rejected);

        let resurrect = ingestion
            .confirm_claim(
                &scope,
                &claim.claim_id,
                &OwnerDecision::by("cofounder@example.com"),
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("a terminal state never resurrects");
        assert!(
            resurrect.to_string().contains("already rejected"),
            "{resurrect}"
        );

        assert_eq!(
            ingestion
                .claim(&scope, &claim.claim_id)
                .expect("load")
                .expect("present")
                .status,
            TranscriptClaimStatus::Rejected
        );
        assert_eq!(
            ingestion
                .assertions()
                .disclosures_carrying_claim(&scope, &claim.approved_claim_ref)
                .expect("reverse lookup")
                .len(),
            0,
            "a rejected claim never reaches the record"
        );
    }

    /// The crash window `confirm_claim`'s ordering deliberately leaves open:
    /// assertion rows written, decision record not yet appended, so the claim
    /// still reads pending. Rejecting from there would leave the register saying
    /// we never said it while the reverse lookup still points at everyone who
    /// heard it — two records disagreeing about one sentence.
    #[test]
    fn rejecting_a_claim_whose_rows_already_exist_is_refused_and_names_the_route_that_works() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "our revenue last quarter was 400k")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = ingested.claims[0].clone();

        ingestion
            .assertions()
            .record_assertion_use(
                &scope,
                &claim.outward_act_ref,
                &claim.approved_claim_ref,
                "alice@example.com",
                &[],
                &[],
                "2026-08-20T08:00:00+00:00",
            )
            .expect("the row half of a confirmation that then crashed");
        assert_eq!(
            ingestion
                .claim(&scope, &claim.claim_id)
                .expect("load")
                .expect("present")
                .status,
            TranscriptClaimStatus::Pending,
            "the decision record never landed, so the queue still shows it as open"
        );

        let error = ingestion
            .reject_claim(
                &scope,
                &claim.claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("already asserted");
        let message = error.to_string();
        assert!(message.contains("already been asserted to 1"), "{message}");
        assert!(
            message.contains(&claim.approved_claim_ref),
            "the refusal names the claim a correction has to be raised against: {message}"
        );
    }

    /// Re-ingesting the same transcript resumes one act and one queue entry;
    /// re-ingesting the key with different words is an error, because the stored
    /// act would keep pointing at the first transcript while the caller believed
    /// it had recorded the second.
    #[test]
    fn re_ingesting_resumes_and_changed_words_under_one_key_are_an_error() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![ours("seg-1", "our revenue last quarter was 400k")];

        let first = ingestion
            .ingest_transcript(&scope, &source(), &utterances, at("2026-08-19T10:00:00Z"))
            .expect("first");
        let replay = ingestion
            .ingest_transcript(&scope, &source(), &utterances, at("2026-08-19T11:00:00Z"))
            .expect("replay");

        assert_eq!(
            first.disclosure.outward_act_ref, replay.disclosure.outward_act_ref,
            "one transcript is one act"
        );
        assert_eq!(
            replay.disclosure.prepared_at, "2026-08-19T10:00:00+00:00",
            "the replay must not rewrite when the room happened"
        );
        assert_eq!(first.claims, replay.claims);
        assert_eq!(
            ingestion.pending_claims(&scope).expect("queue").len(),
            1,
            "one entry, not two: the decider is not asked the same question twice"
        );

        let edited = vec![ours("seg-1", "our revenue last quarter was 900k")];
        let error = ingestion
            .ingest_transcript(&scope, &source(), &edited, at("2026-08-19T12:00:00Z"))
            .expect_err("changed words under an existing key");
        assert!(error.to_string().contains("different words"), "{error}");
    }

    #[test]
    fn replay_refuses_changed_relationship_time_and_extractor() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![ours("seg-1", "we can start in March")];
        let original = source();
        let now = at("2026-08-19T10:00:00Z");
        let first = ingestion
            .ingest_transcript(&scope, &original, &utterances, now)
            .unwrap();
        for change in 0..3 {
            let mut changed = original.clone();
            match change {
                0 => changed.audience = Some(AudienceRef::engagement("another-relationship")),
                1 => changed.occurred_at = at("2026-08-20T09:00:00Z"),
                _ => changed.extracted_by = "another-extractor".into(),
            }
            assert!(ingestion
                .ingest_transcript(&scope, &changed, &utterances, now)
                .is_err());
        }
        assert_eq!(ingestion.claims(&scope).unwrap(), first.claims);
        assert_eq!(
            ingestion
                .ingest_transcript(&scope, &original, &utterances, now)
                .unwrap()
                .claims,
            first.claims
        );
    }

    #[test]
    fn payload_write_failure_still_preserves_the_original_import_binding() {
        let (_tmp, ingestion, scope) = ingestion();
        let root = ingestion
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("outward_assertions");
        std::fs::create_dir_all(&root).unwrap();
        // A regular file where payload storage needs a directory injects an
        // I/O failure before any observed act or candidate can be written.
        let blocker = root.join("payloads");
        std::fs::write(&blocker, "unavailable").unwrap();
        let original = source();
        let utterances = vec![ours("seg-1", "we start in March")];
        let now = at("2026-08-19T10:00:00Z");
        assert!(ingestion
            .ingest_transcript(&scope, &original, &utterances, now)
            .is_err());
        assert!(ingestion.claims(&scope).unwrap().is_empty());
        assert!(ingestion
            .ingestion_fingerprint(&scope, &original.transcript_key)
            .unwrap()
            .is_some());
        let act_ref = super::super::outward_assertions::derive_act_ref(
            &scope,
            &format!("observed:{}", original.transcript_key),
        );
        assert!(ingestion
            .assertions()
            .load_act(&scope, &act_ref)
            .unwrap()
            .is_none());
        std::fs::remove_file(blocker).unwrap();
        let mut changed = original.clone();
        changed.audience = Some(AudienceRef::engagement("other-engagement"));
        assert!(ingestion
            .ingest_transcript(&scope, &changed, &utterances, now)
            .is_err());
        let resumed = ingestion
            .ingest_transcript(&scope, &original, &utterances, at("2026-08-20T10:00:00Z"))
            .unwrap();
        assert_eq!(resumed.claims.len(), 1);
        assert_eq!(resumed.claims[0].audience_ref, original.audience);
        assert_eq!(resumed.claims[0].stated_at, original.occurred_at);
    }

    #[test]
    fn interrupted_import_keeps_metadata_bound_before_any_candidate_lands() {
        for change in 0..5 {
            let (_tmp, ingestion, scope) = ingestion();
            let original = source();
            let utterances = vec![ours("seg-1", "we can start in March")];
            let now = at("2026-08-19T10:00:00Z");
            let first = ingestion
                .ingest_transcript(&scope, &original, &utterances, now)
                .unwrap();
            // Retain the write-ahead records and act, as if the process stopped
            // before its first candidate append. Reopen through a new store.
            let path = ingestion.claims_path(&scope);
            let raw = std::fs::read_to_string(&path).unwrap();
            let retained: String = raw
                .lines()
                .filter(|line| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["record"]
                        != "extracted"
                })
                .map(|line| format!("{line}\n"))
                .collect();
            std::fs::write(&path, retained).unwrap();
            let reopened = TranscriptIngestion::new(ingestion.workspace_layout.clone());
            let mut changed_source = original.clone();
            let mut changed_utterances = utterances.clone();
            match change {
                0 => changed_source.audience = Some(AudienceRef::engagement("other-engagement")),
                1 => changed_source.extracted_by = "other-extractor".into(),
                2 => changed_source.occurred_at = at("2026-08-20T09:00:00Z"),
                3 => changed_utterances[0].claim_ref = Some("other-claim".into()),
                _ => changed_utterances[0].evidence_refs = vec!["other-evidence".into()],
            }
            assert!(
                reopened
                    .ingest_transcript(&scope, &changed_source, &changed_utterances, now)
                    .is_err(),
                "interrupted import accepted substituted metadata case {change}"
            );
            assert!(reopened.claims(&scope).unwrap().is_empty());
            let resumed = reopened
                .ingest_transcript(&scope, &original, &utterances, now)
                .unwrap();
            assert_eq!(resumed.claims, first.claims);
        }
    }

    #[test]
    fn conflicting_candidates_do_not_leave_an_unfinishable_partial_import() {
        let (_tmp, ingestion, scope) = ingestion();
        let mut utterances = vec![
            ours("seg-1", "we start in March"),
            ours("seg-2", "we start in April"),
        ];
        for utterance in &mut utterances {
            utterance.claim_ref = Some("same-catalogue-claim".into());
        }
        let now = at("2026-08-19T10:00:00Z");
        assert!(ingestion
            .ingest_transcript(&scope, &source(), &utterances, now)
            .is_err());
        assert!(
            ingestion.claims(&scope).unwrap().is_empty(),
            "an invalid batch must not publish its first candidate"
        );
        utterances[1].claim_ref = Some("other-catalogue-claim".into());
        assert_eq!(
            ingestion
                .ingest_transcript(&scope, &source(), &utterances, now)
                .unwrap()
                .claims
                .len(),
            2
        );
    }

    #[test]
    fn legacy_partial_import_is_validated_whole_before_adopting_a_fingerprint() {
        let (_tmp, ingestion, scope) = ingestion();
        let original = source();
        let utterances = vec![
            ours("seg-1", "we start in March"),
            ours("seg-2", "we finish in April"),
        ];
        let now = at("2026-08-19T10:00:00Z");
        let first = ingestion
            .ingest_transcript(&scope, &original, &utterances, now)
            .unwrap();
        let rejected = ingestion
            .reject_claim(
                &scope,
                &first.claims[1].claim_id,
                &OwnerDecision::by("owner@example.com"),
                now,
            )
            .unwrap();
        // A historical partial import has no fingerprint and only the later
        // candidate. A refused retry must not publish the earlier candidate or
        // bind the substituted metadata, even though it encounters it first.
        let path = ingestion.claims_path(&scope);
        let raw = std::fs::read_to_string(&path).unwrap();
        let retained: String = raw
            .lines()
            .filter(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                row["record"] != "ingestion_prepared" && row["claim_id"] != first.claims[0].claim_id
            })
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, retained).unwrap();
        let mut changed = utterances.clone();
        changed[1].evidence_refs = vec!["substituted-evidence".into()];
        assert!(ingestion
            .ingest_transcript(&scope, &original, &changed, now)
            .is_err());
        assert_eq!(ingestion.claims(&scope).unwrap(), vec![rejected.clone()]);
        assert!(ingestion
            .ingestion_fingerprint(&scope, &original.transcript_key)
            .unwrap()
            .is_none());
        let resumed = ingestion
            .ingest_transcript(&scope, &original, &utterances, now)
            .unwrap();
        assert_eq!(resumed.claims, vec![first.claims[0].clone(), rejected]);
        assert!(ingestion
            .ingestion_fingerprint(&scope, &original.transcript_key)
            .unwrap()
            .is_some());
        assert_eq!(
            ingestion
                .ingest_transcript(&scope, &original, &utterances, now)
                .unwrap()
                .claims,
            resumed.claims
        );
    }

    #[test]
    fn partial_import_cannot_resume_with_a_different_room_context() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![ours("seg-1", "we can start in March")];
        let original = source();
        let now = at("2026-08-19T10:00:00Z");
        // The observed act landed, but candidate extraction did not finish.
        record_observed_statement(
            &ingestion.assertions(),
            &scope,
            &observed_statement(&original, &render_transcript(&utterances), None),
            &now.to_rfc3339(),
        )
        .unwrap();
        for change in 0..5 {
            let mut changed = original.clone();
            match change {
                0 => changed.attendees = vec!["another-recipient@example.com".into()],
                1 => changed.effective_speaker = "another-speaker".into(),
                2 => changed.program_id = Some("another-program".into()),
                3 => changed.engagement_id = Some("another-engagement".into()),
                _ => changed.consequence_class = ConsequenceClass::ConfidentialDisclosure,
            }
            assert!(ingestion
                .ingest_transcript(&scope, &changed, &utterances, now)
                .is_err());
            assert!(ingestion.claims(&scope).unwrap().is_empty());
        }
        let resumed = ingestion
            .ingest_transcript(&scope, &original, &utterances, now)
            .unwrap();
        assert_eq!(resumed.claims.len(), 1);
        assert_eq!(
            resumed.claims[0].audience,
            resumed.disclosure.intended_audience
        );
    }

    /// A confirmed promise becomes a tracked commitment — and lands
    /// **unconfirmed**, which is the row §6A says to go looking for. Confirming
    /// "we said this" is not confirming "this term is one we stand behind", and
    /// collapsing the two would let a sentence in a meeting become binding with
    /// one click.
    #[test]
    fn a_confirmed_promise_becomes_an_unconfirmed_commitment() {
        let (tmp, ingestion, scope) = ingestion();
        // Same workspace root, so both registers land under one scope.
        let commitments = Commitments::new(ArtifactV2Workspace::new(tmp.path()));
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we will have the integration done by March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();

        let too_early = ingestion
            .record_commitment_from_claim(
                &commitments,
                &scope,
                &claim_id,
                at("2026-08-20T08:00:00Z"),
            )
            .expect_err("an unconfirmed extraction is a model's reading of a room");
        assert!(
            too_early.to_string().contains("still pending"),
            "{too_early}"
        );

        ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");

        let commitment = ingestion
            .record_commitment_from_claim(
                &commitments,
                &scope,
                &claim_id,
                at("2026-08-20T08:05:00Z"),
            )
            .expect("bridge");

        assert_eq!(
            commitment.terms,
            "we will have the integration done by March"
        );
        assert_eq!(commitment.direction, CommitmentDirection::StatedByUs);
        assert_eq!(commitment.stated_at, at("2026-08-19T09:00:00Z"));
        assert_eq!(commitment.source_ref, ingested.disclosure.outward_act_ref);
        assert!(
            !commitment.may_be_restated_outward(),
            "a term nobody has agreed to is not something the agent may repeat"
        );
        assert!(commitment.needs_owner_check());

        let owed = commitments
            .unconfirmed_from_us(
                &CommitmentScope::new("anonymous", "default"),
                &AudienceRef::engagement("eng-1"),
            )
            .expect("query");
        assert_eq!(owed.len(), 1);
        assert_eq!(owed[0].commitment_id, commitment.commitment_id);
    }

    /// A rejected claim can never become a commitment: somebody read the words
    /// and said we did not say them, and a register built from those is a
    /// register of things nobody promised.
    #[test]
    fn a_rejected_claim_cannot_become_a_commitment() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we will have the integration done by March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let mut claim = ingested.claims[0].clone();

        ingestion
            .reject_claim(
                &scope,
                &claim.claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("reject");
        claim.status = TranscriptClaimStatus::Rejected;

        let error = commitment_request_from_claim(&claim).expect_err("rejected is terminal");
        assert!(error.to_string().contains("was rejected"), "{error}");

        claim.status = TranscriptClaimStatus::Confirmed;
        claim.audience_ref = None;
        let unfiled = commitment_request_from_claim(&claim).expect_err("no relationship");
        assert!(
            unfiled.to_string().contains("names no relationship"),
            "{unfiled}"
        );
    }

    /// U+001F is what keeps a derived id's components apart. A caller string
    /// carrying it could fuse two different records — two transcripts, two
    /// segments, two tenants — into one id, and one decision would then cover
    /// words nobody read.
    #[test]
    fn a_caller_string_carrying_the_unit_separator_is_refused() {
        let (_tmp, ingestion, scope) = ingestion();

        let mut fused_key = source();
        fused_key.transcript_key = "call\u{1f}2026".to_string();
        let error = ingestion
            .ingest_transcript(
                &scope,
                &fused_key,
                &[ours("seg-1", "hello")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("fused transcript key");
        assert!(error.to_string().contains("U+001F"), "{error}");

        let error = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg\u{1f}1", "hello")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("fused segment key");
        assert!(error.to_string().contains("U+001F"), "{error}");

        let fused_scope = OutwardScope::new("anon\u{1f}ymous", "default");
        let error = ingestion
            .ingest_transcript(
                &fused_scope,
                &source(),
                &[ours("seg-1", "hello")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("fused scope");
        assert!(error.to_string().contains("U+001F"), "{error}");

        assert_eq!(
            ingestion.pending_claims(&scope).expect("queue").len(),
            0,
            "a refused ingestion queues nothing"
        );
    }

    /// Two utterances under one segment key derive one claim, so the second
    /// would disappear behind the first and one decision would cover words
    /// nobody read.
    #[test]
    fn a_repeated_segment_key_is_refused_rather_than_collapsed() {
        let (_tmp, ingestion, scope) = ingestion();
        let utterances = vec![
            ours("seg-1", "our revenue last quarter was 400k"),
            ours("seg-1", "and we are default alive"),
        ];

        let error = ingestion
            .ingest_transcript(&scope, &source(), &utterances, at("2026-08-19T10:00:00Z"))
            .expect_err("duplicate segment key");
        assert!(error.to_string().contains("appears twice"), "{error}");
    }

    /// A room with nobody in it would write ZERO assertion rows on confirmation
    /// and report success — a claim absent from every reverse lookup, which is
    /// the exact silent drop the assertions store exists to prevent.
    #[test]
    fn a_transcript_with_nobody_in_the_room_is_refused() {
        let (_tmp, ingestion, scope) = ingestion();
        let mut unheard = source();
        unheard.attendees = Vec::new();

        let error = ingestion
            .ingest_transcript(
                &scope,
                &unheard,
                &[ours("seg-1", "hello")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("nobody heard it");
        assert!(error.to_string().contains("ZERO rows"), "{error}");
    }

    /// A channel that COULD have been prepared must not arrive here: recording
    /// an email as observed would claim nothing could have gated it. The refusal
    /// belongs to the writer, and this pins that ingestion does not route around
    /// it.
    #[test]
    fn a_controlled_channel_is_refused_by_the_writer_beneath_this_one() {
        let (_tmp, ingestion, scope) = ingestion();
        let mut controlled = source();
        controlled.channel = OutwardChannel::Email;

        let error = ingestion
            .ingest_transcript(&scope, &controlled, &[], at("2026-08-19T10:00:00Z"))
            .expect_err("email is controlled");
        // `{:#}` walks the context chain; `to_string` would show only this
        // module's wrapper and hide the refusal being pinned.
        let message = format!("{error:#}");
        assert!(message.contains("controlled"), "{message}");
        assert!(
            message.contains("prepare_dispatch"),
            "a refusal must name the call that would have worked: {message}"
        );
    }

    /// The claim ref a caller supplies from an approved-claims catalogue is the
    /// one the assertion lands under, so a confirmed transcript claim is
    /// findable by the same reverse lookup as an emailed one.
    #[test]
    fn a_supplied_claim_ref_is_what_the_assertion_lands_under() {
        let (_tmp, ingestion, scope) = ingestion();
        let mut utterance = ours("seg-1", "our revenue last quarter was 400k");
        utterance.claim_ref = Some("claim-revenue-q2".to_string());
        utterance.evidence_refs = vec!["evidence-books".to_string()];

        let ingested = ingestion
            .ingest_transcript(&scope, &source(), &[utterance], at("2026-08-19T10:00:00Z"))
            .expect("ingest");
        assert_eq!(ingested.claims[0].approved_claim_ref, "claim-revenue-q2");

        ingestion
            .confirm_claim(
                &scope,
                &ingested.claims[0].claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");

        let resting = ingestion
            .assertions()
            .disclosures_resting_on_evidence(&scope, "evidence-books")
            .expect("evidence lookup");
        assert_eq!(
            resting.len(),
            2,
            "one row per person, findable by the source it rested on"
        );
        assert_eq!(resting[0].approved_claim_ref, "claim-revenue-q2");
    }

    #[test]
    fn changing_a_segment_claim_reference_cannot_resurrect_a_rejected_import() {
        for (legacy, original_ref) in [
            (false, None),
            (false, Some("catalog-claim-1".to_owned())),
            (true, None),
            (true, Some("catalog-claim-1".to_owned())),
        ] {
            let (_tmp, ingestion, scope) = ingestion();
            let mut utterance = ours("seg-1", "we will deliver on Friday");
            utterance.claim_ref = original_ref;
            let first = ingestion
                .ingest_transcript(
                    &scope,
                    &source(),
                    &[utterance.clone()],
                    at("2026-08-19T10:00:00Z"),
                )
                .unwrap();
            let rejected = ingestion
                .reject_claim(
                    &scope,
                    &first.claims[0].claim_id,
                    &OwnerDecision::by("owner@example.com"),
                    at("2026-08-20T08:00:00Z"),
                )
                .unwrap();
            if legacy {
                let path = ingestion.claims_path(&scope);
                let raw = std::fs::read_to_string(&path).unwrap();
                let retained: String = raw
                    .lines()
                    .filter(|line| {
                        serde_json::from_str::<serde_json::Value>(line).unwrap()["record"]
                            != "ingestion_prepared"
                    })
                    .map(|line| format!("{line}\n"))
                    .collect();
                std::fs::write(&path, retained).unwrap();
            }
            // Identical words/attribution render the same payload, while the
            // supplied catalogue ref would otherwise derive a fresh queue ID.
            utterance.claim_ref = Some("catalog-claim-2".to_owned());
            let error = ingestion
                .ingest_transcript(&scope, &source(), &[utterance], at("2026-08-21T10:00:00Z"))
                .expect_err("a replay may not replace a segment's claim identity");
            assert!(error.to_string().contains("different"), "{error}");
            if legacy {
                assert!(error.to_string().contains(&rejected.claim_id), "{error}");
            }
            assert_eq!(ingestion.claims(&scope).unwrap(), vec![rejected]);
            assert!(ingestion.pending_claims(&scope).unwrap().is_empty());
        }
    }

    /// Exactly one surface calls the authoritative ingest transition, and it
    /// is an owner route.
    ///
    /// # This test used to assert the opposite, and that was the point
    ///
    /// It read *"nothing outside this file ingests a transcript"*, pinning the
    /// header's admission that this module was written and unreachable — so the
    /// pending register had no producer in a running process and no owner had
    /// ever been shown a claim. Its own instruction was: *"when a route or a
    /// tool ingests a transcript, this test fails. Correct the header to name
    /// it."* `magician-api/src/transcript_claims_api.rs` is that route.
    ///
    /// What it pins now is **which** surface, and that there is one. A second
    /// entry appearing here is a second thing deciding what counts as an
    /// outward room, which is a judgement about the relationship rather than
    /// about the recording — so it should be a deliberate change, not a
    /// discovery.
    #[test]
    fn one_owner_surface_ingests_a_transcript() {
        use crate::magician_v2::doc_wiring_scan::scan_workspace;

        const OWN_FILE: &str = "magician/src/magician_v2/evidence/transcript_ingestion.rs";
        const OWNER_ROUTE: &str = "magician-api/src/transcript_claims_api.rs";

        // Read-only binders and signed decision adapters may construct the
        // store. The authority boundary is the ingest call, not construction.
        let callers = scan_workspace(".ingest_transcript(&scope", &[OWN_FILE]);
        assert!(
            callers.files_searched > 100,
            "only {} files were read, so this proves nothing",
            callers.files_searched
        );
        assert_eq!(
            callers.hits,
            vec![OWNER_ROUTE.to_string()],
            "the set of surfaces ingesting a transcript changed; exactly one owner route \
             should, and a second is a second thing deciding which rooms are outward"
        );

        // And the writer this module exists to call still has exactly one
        // caller — this module — which is the fact the header's first paragraph
        // turns on. Two callers would be two authoritative copies of what we
        // told whom, which §2 of the plan forbids.
        let writers = scan_workspace(
            "record_observed_statement(",
            &["magician/src/magician_v2/evidence/observed_statements.rs"],
        );
        assert_eq!(
            writers.hits,
            vec![OWN_FILE.to_string()],
            "the set of callers of `record_observed_statement` changed; the header names one"
        );
    }

    #[test]
    fn revision_bound_claim_decision_persists_and_replays_one_receipt() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-cas", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = &ingested.claims[0];
        let decision = OwnerDecision::by("owner@example.com").with_note("I said it");

        let first = ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-claim-cas",
                &decision,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");
        assert_eq!(first.claim.status, TranscriptClaimStatus::Confirmed);
        assert_eq!(first.claim.revision, 2);
        assert_eq!(first.receipt.verb, ClaimDecisionVerb::ConfirmClaim);
        assert_eq!(first.receipt.expected_revision, 1);
        assert_eq!(first.receipt.resulting_revision, 2);
        assert_eq!(first.receipt.disposition, ClaimDecisionDisposition::Applied);

        let replay = ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-claim-cas",
                &decision,
                at("2026-08-22T08:00:00Z"),
            )
            .expect("exact replay");
        assert_eq!(replay.claim, first.claim);
        assert_eq!(replay.receipt.receipt_id, first.receipt.receipt_id);
        assert_eq!(replay.receipt.recorded_at, first.receipt.recorded_at);
        assert_eq!(
            replay.receipt.disposition,
            ClaimDecisionDisposition::AlreadyApplied
        );
        assert_eq!(
            ingestion.decision_receipts(&scope).expect("receipts").len(),
            1,
            "a retry projects one authoritative receipt, not two"
        );
    }

    /// E2's wiring on the claims side. The claims register has no decision
    /// index at all, so before the journal the only way to answer *"what has
    /// been decided since I last looked"* was to fold the whole log and diff
    /// it. Both terminal verbs land in the journal, in completion order, once.
    #[test]
    fn completed_claim_decisions_reach_the_completion_journal_once() {
        use super::super::completion_journal::EvidenceCompletionCursor;

        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[
                    ours("seg-journal-1", "we can start in March"),
                    ours("seg-journal-2", "the integration ships in April"),
                ],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        assert_eq!(ingested.claims.len(), 2, "one claim per outward utterance");
        let confirmed_claim = ingested.claims[0].claim_id.clone();
        let rejected_claim = ingested.claims[1].claim_id.clone();
        let decision = OwnerDecision::by("owner@example.com").with_note("I said it");

        let confirmed = ingestion
            .confirm_claim_at_revision(
                &scope,
                &confirmed_claim,
                1,
                "decision-journal-confirm",
                &decision,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");
        let rejected = ingestion
            .reject_claim_at_revision(
                &scope,
                &rejected_claim,
                1,
                "decision-journal-reject",
                &decision,
                at("2026-08-20T08:01:00Z"),
            )
            .expect("reject");

        let journal = EvidenceCompletionJournal::new(ingestion.workspace_layout.clone());
        let journal_scope = EvidenceDecisionScope::new("anonymous", "default");
        let page = journal
            .page_after(&journal_scope, EvidenceCompletionCursor::START, 10)
            .expect("page the journal");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| (entry.seq, entry.decision_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, "decision-journal-confirm"),
                (2, "decision-journal-reject"),
            ],
            "the cursor reads completion order, not identity order"
        );
        assert_eq!(page.entries[0].receipt_id, confirmed.receipt.receipt_id);
        assert_eq!(page.entries[1].receipt_id, rejected.receipt.receipt_id);
        assert_eq!(
            page.entries[0].target,
            EvidenceDecisionTarget::TranscriptClaim {
                claim_id: confirmed_claim.clone(),
            }
        );
        assert!(!page.has_more);

        // An exact replay is `already_applied`. Journalling it again would hand
        // the projector the same completion twice.
        ingestion
            .confirm_claim_at_revision(
                &scope,
                &confirmed_claim,
                1,
                "decision-journal-confirm",
                &decision,
                at("2026-08-22T08:00:00Z"),
            )
            .expect("exact replay");
        assert_eq!(journal.head(&journal_scope).expect("head").seq(), 2);
    }

    /// The crash window the replay branch exists to close: the claim row is
    /// authoritative and lands first, so a process that dies before the journal
    /// append leaves a completed decision no cursor would ever surface. The
    /// retry repairs it rather than reporting a clean replay over a hole.
    #[test]
    fn a_replay_repairs_a_journal_entry_that_never_landed() {
        use super::super::completion_journal::EvidenceCompletionCursor;

        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-journal-heal", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();
        let decision = OwnerDecision::by("owner@example.com");
        ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim_id,
                1,
                "decision-journal-heal",
                &decision,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");

        let journal_root = ingestion
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("decision_journal");
        std::fs::remove_dir_all(&journal_root)
            .expect("simulate a crash between the claim row and its journal append");

        let journal = EvidenceCompletionJournal::new(ingestion.workspace_layout.clone());
        let journal_scope = EvidenceDecisionScope::new("anonymous", "default");
        assert_eq!(journal.head(&journal_scope).expect("head").seq(), 0);

        ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim_id,
                1,
                "decision-journal-heal",
                &decision,
                at("2026-08-21T08:00:00Z"),
            )
            .expect("replay");
        let page = journal
            .page_after(&journal_scope, EvidenceCompletionCursor::START, 10)
            .expect("page");
        assert_eq!(page.entries.len(), 1, "the replay restored the completion");
        assert_eq!(page.entries[0].decision_id, "decision-journal-heal");
    }

    /// The same crash window, for the caller who may no longer re-enter the
    /// mutation path: a signed proposal whose admission lifetime elapsed can
    /// only take the recovery route, so if that route journalled nothing the
    /// hole would be permanent and no cursor would ever carry the completion.
    #[test]
    fn recovery_repairs_a_journal_entry_the_mutation_path_can_no_longer_reach() {
        use super::super::completion_journal::EvidenceCompletionCursor;

        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-journal-recover", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();
        let decision = OwnerDecision::by("owner@example.com");
        let confirmed = ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim_id,
                1,
                "decision-journal-recover",
                &decision,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm");

        let journal_root = ingestion
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("decision_journal");
        std::fs::remove_dir_all(&journal_root)
            .expect("simulate a crash between the claim row and its journal append");

        let journal = EvidenceCompletionJournal::new(ingestion.workspace_layout.clone());
        let journal_scope = EvidenceDecisionScope::new("anonymous", "default");
        assert_eq!(journal.head(&journal_scope).expect("head").seq(), 0);

        let recovered = ingestion
            .recover_claim_decision(
                &scope,
                &claim_id,
                1,
                "decision-journal-recover",
                ClaimDecisionVerb::ConfirmClaim,
                &decision,
                at("2026-08-25T08:00:00Z"),
            )
            .expect("recover")
            .expect("the decision was applied");
        assert_eq!(
            recovered.receipt.disposition,
            ClaimDecisionDisposition::AlreadyApplied
        );

        let page = journal
            .page_after(&journal_scope, EvidenceCompletionCursor::START, 10)
            .expect("page");
        assert_eq!(
            page.entries.len(),
            1,
            "the recovery route restored the completion the mutation path never journalled"
        );
        assert_eq!(page.entries[0].decision_id, "decision-journal-recover");
        assert_eq!(page.entries[0].receipt_id, confirmed.receipt.receipt_id);
        assert_eq!(
            page.entries[0].target,
            EvidenceDecisionTarget::TranscriptClaim {
                claim_id: claim_id.clone(),
            }
        );
        // The repair is the receipt's own completion time, not the recovery's,
        // so a projector reading the entry sees when the register accepted it.
        assert_eq!(page.entries[0].completed_at, at("2026-08-20T08:00:00Z"));
        assert_eq!(page.entries[0].journaled_at, at("2026-08-25T08:00:00Z"));

        // Recovery is repeatable, so it must not append a second completion.
        ingestion
            .recover_claim_decision(
                &scope,
                &claim_id,
                1,
                "decision-journal-recover",
                ClaimDecisionVerb::ConfirmClaim,
                &decision,
                at("2026-08-26T08:00:00Z"),
            )
            .expect("recover again")
            .expect("the decision was applied");
        assert_eq!(journal.head(&journal_scope).expect("head").seq(), 1);
    }

    #[test]
    fn guarded_denial_precedes_claim_wal_and_terminal_write() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-guarded", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = &ingested.claims[0];
        let decision = OwnerDecision::by("owner@example.com");
        let error = ingestion
            .confirm_claim_at_revision_guarded(
                &scope,
                &claim.claim_id,
                claim.revision,
                "decision-claim-guarded",
                &decision,
                at("2026-08-20T08:00:00Z"),
                |_| anyhow::bail!("proposal expired"),
            )
            .expect_err("denied admission cannot create a preparation");
        assert!(error.to_string().contains("proposal expired"));
        assert_eq!(
            ingestion
                .exact_confirmation_prepared_at(
                    &scope,
                    &claim.claim_id,
                    claim.revision,
                    "decision-claim-guarded",
                    &decision,
                )
                .expect("inspect preparation"),
            None
        );
        assert!(ingestion
            .claim(&scope, &claim.claim_id)
            .expect("claim")
            .expect("claim exists")
            .status
            .is_open());
    }

    #[test]
    fn prepared_confirmation_resumes_exactly_and_blocks_competing_rejection() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-wal", "we will send the revision Friday")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = &ingested.claims[0];
        let decision = OwnerDecision::by("owner@example.com").with_note("reviewed context");
        let decision_id = "claim-decision-wal-1";
        assert!(ingestion
            .pending_confirmation(&scope, &claim.claim_id)
            .unwrap()
            .is_none());
        let fingerprint = claim_decision_request_fingerprint(
            &claim.claim_id,
            ClaimDecisionVerb::ConfirmClaim,
            claim.revision,
            &decision,
        );
        ingestion
            .append(
                &scope,
                &ClaimLogRecord::ConfirmationPrepared {
                    claim_id: claim.claim_id.clone(),
                    decision_id: decision_id.to_owned(),
                    expected_revision: claim.revision,
                    by: decision.by.clone(),
                    note: decision.note.clone(),
                    request_fingerprint: fingerprint,
                    prepared_at: at("2026-08-20T08:00:00Z"),
                },
            )
            .expect("persist simulated write-ahead preparation");
        let reopened = TranscriptIngestion::new(ingestion.workspace_layout.clone());
        let pending = reopened
            .pending_confirmation(&scope, &claim.claim_id)
            .unwrap()
            .unwrap();
        assert_eq!(pending.decision_id, decision_id);
        assert_eq!(pending.expected_revision, claim.revision);
        assert_eq!(pending.by, decision.by);
        assert_eq!(pending.note, decision.note);
        assert_eq!(
            reopened
                .claim(&scope, &claim.claim_id)
                .unwrap()
                .unwrap()
                .status,
            TranscriptClaimStatus::Pending
        );
        assert!(reopened
            .pending_confirmation(
                &OutwardScope::new("another-owner", "default"),
                &claim.claim_id
            )
            .unwrap()
            .is_none());
        assert_eq!(
            ingestion
                .exact_confirmation_prepared_at(
                    &scope,
                    &claim.claim_id,
                    claim.revision,
                    decision_id,
                    &decision,
                )
                .expect("read exact preparation"),
            Some(at("2026-08-20T08:00:00Z")),
            "the durable admission time remains available for post-expiry recovery"
        );

        let rejection = ingestion
            .reject_claim_at_revision(
                &scope,
                &claim.claim_id,
                claim.revision,
                "claim-decision-wal-reject",
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:01:00Z"),
            )
            .expect_err("a competing decision cannot cross the prepared confirmation");
        assert!(rejection.to_string().contains("incomplete confirmation"));

        let completed = ingestion
            .confirm_claim_at_revision_guarded(
                &scope,
                &claim.claim_id,
                claim.revision,
                decision_id,
                &decision,
                at("2026-08-20T08:02:00Z"),
                |prepared_at| {
                    assert_eq!(prepared_at, Some(at("2026-08-20T08:00:00Z")));
                    Ok(())
                },
            )
            .expect("the exact prepared confirmation resumes");
        assert_eq!(completed.claim.status, TranscriptClaimStatus::Confirmed);
        assert!(reopened
            .pending_confirmation(&scope, &claim.claim_id)
            .unwrap()
            .is_none());
        let recovered = ingestion
            .recover_claim_decision(
                &scope,
                &claim.claim_id,
                claim.revision,
                decision_id,
                ClaimDecisionVerb::ConfirmClaim,
                &decision,
                at("2026-08-20T08:03:00Z"),
            )
            .expect("recover receipt")
            .expect("receipt exists");
        assert_eq!(
            recovered.receipt.disposition,
            ClaimDecisionDisposition::AlreadyApplied
        );
    }

    #[test]
    fn claim_decision_id_binds_note_actor_verb_and_revision() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-bind", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = &ingested.claims[0];
        let first = OwnerDecision::by("owner@example.com").with_note("exact note");
        ingestion
            .reject_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-claim-bind",
                &first,
                at("2026-08-20T08:00:00Z"),
            )
            .expect("reject");

        for changed in [
            OwnerDecision::by("owner@example.com").with_note("changed note"),
            OwnerDecision::by("other@example.com").with_note("exact note"),
        ] {
            let error = ingestion
                .reject_claim_at_revision(
                    &scope,
                    &claim.claim_id,
                    1,
                    "decision-claim-bind",
                    &changed,
                    at("2026-08-21T08:00:00Z"),
                )
                .expect_err("substituted replay");
            assert!(error.to_string().contains("substituted"), "{error}");
        }

        let opposite = ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-claim-bind",
                &first,
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("opposite verb under one decision id");
        assert!(opposite.to_string().contains("substituted"), "{opposite}");

        let stale = ingestion
            .reject_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-claim-new",
                &first,
                at("2026-08-21T08:00:00Z"),
            )
            .expect_err("new decision against a terminal head");
        assert!(
            stale.to_string().contains("stale claim revision")
                || stale.to_string().contains("already rejected"),
            "{stale}"
        );
    }

    #[test]
    fn revision_bound_commitment_bridge_keeps_the_two_owner_gates_separate() {
        let (tmp, ingestion, scope) = ingestion();
        let commitments = Commitments::new(ArtifactV2Workspace::new(tmp.path()));
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-term", "we will ship Friday")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        let claim = &ingested.claims[0];
        let confirmed = ingestion
            .confirm_claim_at_revision(
                &scope,
                &claim.claim_id,
                1,
                "decision-confirm-source-claim",
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("confirm source claim");

        let recorded = ingestion
            .record_commitment_from_claim_at_revision(
                &commitments,
                &scope,
                &claim.claim_id,
                confirmed.claim.revision,
                "decision-record-term",
                at("2026-08-20T08:01:00Z"),
            )
            .expect("record term");
        assert_eq!(recorded.commitment.status, CommitmentStatus::Unconfirmed);
        assert_eq!(recorded.commitment.revision, 1);
        assert!(!recorded.commitment.may_be_restated_outward());

        let commitment_scope = CommitmentScope::new("anonymous", "default");
        let final_gate = commitments
            .confirm_at_revision(
                &commitment_scope,
                &AudienceRef::engagement("eng-1"),
                &recorded.commitment.commitment_id,
                1,
                "decision-confirm-term",
                "owner@example.com",
                at("2026-08-20T08:02:00Z"),
            )
            .expect("second named-person gate");
        assert_eq!(final_gate.commitment.status, CommitmentStatus::Confirmed);
        assert_eq!(final_gate.commitment.revision, 2);
        assert!(final_gate.commitment.may_be_restated_outward());
    }

    // ── E4: the claims fold is indexed, bounded and abandonable ─────────────

    /// A caller that has stopped waiting gets a refusal, not a short queue.
    ///
    /// The claims log is one file per scope and the queue projection pages it
    /// only after the whole log is folded, so an abandoned read otherwise runs
    /// to completion on a blocking worker. It has to refuse rather than return
    /// what it had: the log is a log of decisions, so stopping before a
    /// `Rejected` record reports a decided claim as still awaiting a person.
    #[test]
    fn a_cancelled_claims_fold_refuses_rather_than_returning_a_partial_queue() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[ours("seg-1", "we can start in March")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        ingestion
            .reject_claim(
                &scope,
                &ingested.claims[0].claim_id,
                &OwnerDecision::by("owner@example.com").with_note("nobody said that"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("reject");

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = ingestion
            .claims_until_cancelled(&scope, &cancellation)
            .expect_err("a cancelled fold refuses");
        assert!(
            crate::magician_v2::evidence::store_cursor::fold_was_cancelled(&error),
            "a cancelled read is the caller's own doing, not a register fault: {error}"
        );
    }

    /// Cancellation is the only thing the cancellable entry point changes. An
    /// uncancelled fold answers exactly what the ordinary read answers,
    /// decisions and order included — the indexed fold is a cost fix, not a new
    /// answer.
    #[test]
    fn an_uncancelled_claims_fold_answers_exactly_what_the_ordinary_read_answers() {
        let (_tmp, ingestion, scope) = ingestion();
        let ingested = ingestion
            .ingest_transcript(
                &scope,
                &source(),
                &[
                    ours("seg-1", "we can start in March"),
                    ours("seg-2", "our revenue last quarter was 400k"),
                ],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("ingest");
        assert_eq!(ingested.claims.len(), 2);
        ingestion
            .reject_claim(
                &scope,
                &ingested.claims[1].claim_id,
                &OwnerDecision::by("owner@example.com").with_note("nobody said that"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect("reject");

        let ordinary = ingestion.claims(&scope).expect("ordinary read");
        let cancellable = ingestion
            .claims_until_cancelled(&scope, &CancellationToken::new())
            .expect("a token nobody cancels");
        assert_eq!(
            ordinary
                .iter()
                .map(|claim| (claim.claim_id.as_str(), claim.status))
                .collect::<Vec<_>>(),
            cancellable
                .iter()
                .map(|claim| (claim.claim_id.as_str(), claim.status))
                .collect::<Vec<_>>()
        );
        // The decision the index has to carry actually landed, and the head
        // rows are still in append order.
        assert_eq!(
            ordinary
                .iter()
                .map(|claim| (claim.claim_id.as_str(), claim.status))
                .collect::<Vec<_>>(),
            vec![
                (
                    ingested.claims[0].claim_id.as_str(),
                    TranscriptClaimStatus::Pending
                ),
                (
                    ingested.claims[1].claim_id.as_str(),
                    TranscriptClaimStatus::Rejected
                ),
            ]
        );
    }

    // ── The staged-ingest consumer ──────────────────────────────────────────

    fn mapping() -> serde_json::Value {
        serde_json::json!({
            "speakers": { "a": "founder@example.com", "b": "alice@example.com" },
            "utterances": [
                { "speaker": "a", "text": "we can start in March" },
                { "speaker": "b", "text": "what does the pipeline look like" },
            ],
        })
    }

    fn staged_row(ingest_id: &str, mapping_json: &str) -> serde_json::Value {
        serde_json::json!({
            "ingest_id": ingest_id,
            "transcript_text": "founder: we can start in March\nalice: and the pipeline?",
            "speaker_mapping_json": mapping_json,
            "audience_kind": "engagement",
            "audience_id": "eng-1",
            "outwardness_reason": "a diligence call with the counterparty",
            "actor_ref": serde_json::Value::Null,
            "apply_state": "recorded",
            "act_ref": serde_json::Value::Null,
            "submitted_at": "2026-08-19T09:30:00Z",
        })
    }

    fn staged_request(ingest_id: &str, mapping: &serde_json::Value) -> StagedIngestRequest {
        StagedIngestRequest::from_staged_row(&staged_row(ingest_id, &mapping.to_string()))
            .expect("staged row")
    }

    fn host_roster() -> StagedIngestRoster {
        StagedIngestRoster::new()
            .resolved_ours("founder@example.com")
            .expect("ours")
            .resolved_counterparty("alice@example.com")
            .expect("counterparty")
    }

    fn host_context(roster: StagedIngestRoster) -> StagedIngestHostContext {
        StagedIngestHostContext::resolved(
            "owner@example.com",
            "founder@example.com",
            roster,
            at("2026-08-19T09:00:00Z"),
        )
    }

    /// The whole staged path in one assertion: the mapping decides who spoke,
    /// the host roster decides which side they were on, and only our side's
    /// words become something an owner is asked about.
    #[test]
    fn a_staged_ingest_attributes_only_from_the_explicit_mapping() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);

        let applied = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T10:00:00Z"),
            )
            .expect("apply");

        assert_eq!(applied.ingest_id, "ingest-1");
        assert_eq!(applied.transcript_key, "staged-ingest:ingest-1");
        assert_eq!(applied.act_recorded_at(), "2026-08-19T10:00:00+00:00");
        assert_eq!(applied.ingested.utterances_seen, 2);
        assert_eq!(
            applied
                .ingested
                .skipped
                .iter()
                .map(|skipped| (skipped.segment_key.as_str(), skipped.reason))
                .collect::<Vec<_>>(),
            vec![("u0002", SkipReason::SpokenByCounterparty)],
            "the counterparty's words are on the record inside the payload, never as a claim"
        );

        let claims = &applied.ingested.claims;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].speaker, "founder@example.com");
        assert_eq!(claims[0].segment_key, "u0001");
        assert_eq!(claims[0].stated_text, "we can start in March");
        assert_eq!(claims[0].status, TranscriptClaimStatus::Pending);
        assert_eq!(
            claims[0].audience,
            vec!["alice@example.com".to_string()],
            "our own speaker is not their own audience; the rows are who we told"
        );
        assert_eq!(
            claims[0].audience_ref,
            Some(AudienceRef::engagement("eng-1"))
        );
        assert_eq!(
            claims[0].extracted_by, "owner@example.com",
            "the authenticated applier is the extractor of record"
        );
        assert!(
            ingestion
                .assertions()
                .load_act(&scope, applied.outward_act_ref())
                .expect("load")
                .expect("present")
                .observed
        );
    }

    /// The property this consumer exists to hold. A named person the host
    /// cannot place is not a diarisation miss — somebody typed that name — so
    /// the request refuses whole rather than quietly dropping their words out
    /// of the queue the reviewer believes they are reading.
    #[test]
    fn a_staged_speaker_the_host_cannot_place_refuses_the_whole_ingest() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);
        let partial = StagedIngestRoster::new()
            .resolved_ours("founder@example.com")
            .expect("ours");

        let error = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(partial),
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("unplaceable speaker");
        assert!(error.to_string().contains("cannot place them"), "{error}");
        assert!(
            ingestion.claims(&scope).expect("claims").is_empty(),
            "a refused request writes nothing"
        );

        // And nothing was written on the refusal: the act this later apply
        // records carries the LATER clock, so no half-act is being resumed.
        let applied = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T11:00:00Z"),
            )
            .expect("apply once the roster is complete");
        assert_eq!(applied.act_recorded_at(), "2026-08-19T11:00:00+00:00");
    }

    /// Act-always, carried into the staged path. A room where only the other
    /// side spoke yields no candidate and still leaves a durable record that it
    /// happened — the package must never read a claim count as an apply
    /// receipt.
    #[test]
    fn a_staged_ingest_records_the_act_even_when_only_the_counterparty_spoke() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);
        let mapping = serde_json::json!({
            "speakers": { "a": "founder@example.com", "b": "alice@example.com" },
            "utterances": [{ "speaker": "b", "text": "we will come back to you" }],
        });

        let applied = consumer
            .apply(
                &scope,
                &staged_request("ingest-quiet", &mapping),
                &host_context(host_roster()),
                at("2026-08-19T10:00:00Z"),
            )
            .expect("apply");

        assert!(applied.ingested.claims.is_empty());
        assert_eq!(applied.ingested.skipped.len(), 1);
        assert!(!applied.outward_act_ref().is_empty());
        assert!(ingestion
            .assertions()
            .load_act(&scope, applied.outward_act_ref())
            .expect("load")
            .is_some());
        assert!(ingestion.pending_claims(&scope).expect("queue").is_empty());
    }

    /// An utterance whose speaker key the map never defines is the one thing
    /// the staged document exists to make impossible. It is refused at the
    /// door, not attributed by proximity, order, or prose.
    #[test]
    fn an_unmapped_utterance_is_refused_rather_than_guessed() {
        let mapping = serde_json::json!({
            "speakers": { "a": "founder@example.com" },
            "utterances": [{ "speaker": "c", "text": "we can start in March" }],
        });
        let error =
            StagedIngestRequest::from_staged_row(&staged_row("ingest-1", &mapping.to_string()))
                .expect_err("unmapped");
        assert!(error.to_string().contains("never defines"), "{error}");
    }

    /// JSON keeps the last value for a repeated key, so a collapsed duplicate
    /// would silently reassign attribution away from the mapping a reviewer
    /// read.
    #[test]
    fn a_repeated_speaker_key_is_refused_rather_than_collapsed() {
        let error = StagedIngestDocument::parse(
            "{\"speakers\":{\"a\":\"founder@example.com\",\"a\":\"alice@example.com\"},\
             \"utterances\":[]}",
        )
        .expect_err("duplicate key");
        // Rendered with `{:#}`: the refusal is serde's, reached under this
        // module's parse context, and a bare `to_string` shows only the context.
        assert!(format!("{error:#}").contains("defined twice"), "{error:#}");
    }

    /// The closed-shape check, in both directions. A row the host only partly
    /// understands was written against a different contract, and a row that
    /// already carries the host's own fields is claiming an application nobody
    /// performed.
    #[test]
    fn only_a_recorded_row_with_the_closed_package_shape_is_admissible() {
        let mapping = mapping().to_string();

        let mut applied = staged_row("ingest-1", &mapping);
        applied["apply_state"] = serde_json::Value::String("applied".to_string());
        let error = StagedIngestRequest::from_staged_row(&applied).expect_err("applied");
        assert!(error.to_string().contains("not `recorded`"), "{error}");

        let mut stamped = staged_row("ingest-1", &mapping);
        stamped["act_ref"] = serde_json::Value::String("act-1".to_string());
        let error = StagedIngestRequest::from_staged_row(&stamped).expect_err("stamped");
        assert!(error.to_string().contains("host's fields"), "{error}");

        let mut widened = staged_row("ingest-1", &mapping);
        widened.as_object_mut().expect("object").insert(
            "attribution_hint".to_string(),
            serde_json::Value::Bool(true),
        );
        let error = StagedIngestRequest::from_staged_row(&widened).expect_err("extra field");
        assert!(
            error.to_string().contains("closed package shape"),
            "{error}"
        );

        let mut narrowed = staged_row("ingest-1", &mapping);
        narrowed
            .as_object_mut()
            .expect("object")
            .remove("outwardness_reason");
        let error = StagedIngestRequest::from_staged_row(&narrowed).expect_err("missing field");
        assert!(
            error.to_string().contains("closed package shape"),
            "{error}"
        );
    }

    /// The row this gate reads is the payload the entity store kept, verbatim.
    /// `actor_ref` and `act_ref` are declared nullable and not required, and
    /// the store skips an absent optional field rather than filling it in — so
    /// the row a compliant `stage_ingest` run writes may carry eight keys, not
    /// ten. Demanding the literal nulls made that ordinary row permanently
    /// unapplyable on the strength of a sentence in the workflow prompt.
    #[test]
    fn a_staged_row_that_omits_the_nullable_host_fields_is_still_admissible() {
        let mapping = mapping().to_string();

        let mut omitted = staged_row("ingest-1", &mapping);
        let object = omitted.as_object_mut().expect("object");
        object.remove("actor_ref");
        object.remove("act_ref");
        assert_eq!(object.len(), 8);

        assert_eq!(
            StagedIngestRequest::from_staged_row(&omitted).expect("omitted host fields"),
            StagedIngestRequest::from_staged_row(&staged_row("ingest-1", &mapping))
                .expect("explicit nulls")
        );
    }

    /// Reading absence as "no value" must not slide into reading it as "skip
    /// the check". Each host field is judged on its own, so a row that leaves
    /// one out and stamps the other is still asserting an application nobody
    /// performed.
    #[test]
    fn one_stamped_host_field_is_refused_even_when_the_other_is_absent() {
        let mapping = mapping().to_string();

        for stamped in STAGED_INGEST_HOST_STAMPED_FIELDS {
            let mut row = staged_row("ingest-1", &mapping);
            let object = row.as_object_mut().expect("object");
            object.remove("actor_ref");
            object.remove("act_ref");
            object.insert(
                stamped.to_string(),
                serde_json::Value::String("already-applied".to_string()),
            );
            let error = StagedIngestRequest::from_staged_row(&row).expect_err(stamped);
            assert!(error.to_string().contains("host's fields"), "{error}");
            assert!(error.to_string().contains(stamped), "{error}");
        }
    }

    /// One `ingest_id` is one room. A retry after a lost response resumes the
    /// same act and the same queue entries rather than asking the owner about
    /// the same words twice.
    #[test]
    fn replaying_one_staged_ingest_id_resumes_the_same_act_and_queue() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);

        let first = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T10:00:00Z"),
            )
            .expect("first apply");
        let second = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T12:00:00Z"),
            )
            .expect("replay");

        assert_eq!(first.outward_act_ref(), second.outward_act_ref());
        assert_eq!(
            first.ingested.claims[0].claim_id,
            second.ingested.claims[0].claim_id
        );
        assert_eq!(
            second.act_recorded_at(),
            "2026-08-19T10:00:00+00:00",
            "the act keeps the clock of the apply that really recorded it"
        );
        assert_eq!(
            ingestion.pending_claims(&scope).expect("queue").len(),
            1,
            "a replay does not queue the room a second time"
        );
    }

    /// The act's effective sender is who spoke for us. A name the roster does
    /// not place on our side would file the other side's words as our own
    /// disclosure, so it refuses before anything is written.
    #[test]
    fn an_effective_speaker_the_roster_does_not_call_ours_is_refused() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);
        let mut context = host_context(host_roster());
        context.effective_speaker = "alice@example.com".to_string();

        let error = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &context,
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("counterparty as our speaker");
        assert!(
            error
                .to_string()
                .contains("does not place them on our side"),
            "{error}"
        );
        assert!(ingestion.claims(&scope).expect("claims").is_empty());
    }

    /// A contradictory resolution is not a preference between two answers. If
    /// the identity register places one person on both sides, nobody can say
    /// whose words those were.
    #[test]
    fn a_roster_that_places_one_person_on_both_sides_is_refused() {
        let error = StagedIngestRoster::new()
            .resolved_ours("dana@example.com")
            .expect("ours")
            .resolved_counterparty("dana@example.com")
            .expect_err("contradiction");
        assert!(error.to_string().contains("both sides"), "{error}");
    }

    /// The self-grant rule reaches the staged path through `extracted_by`: the
    /// authenticated actor that applied the ingest queued these words, so it
    /// may not also be the person who says we said them.
    #[test]
    fn the_actor_that_applied_a_staged_ingest_may_not_confirm_what_it_queued() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);
        let applied = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T10:00:00Z"),
            )
            .expect("apply");
        let claim_id = applied.ingested.claims[0].claim_id.clone();

        let error = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-08-20T08:00:00Z"),
            )
            .expect_err("self-grant");
        assert!(
            error.to_string().contains("cannot also confirm it"),
            "{error}"
        );

        let confirmed = ingestion
            .confirm_claim(
                &scope,
                &claim_id,
                &OwnerDecision::by("founder@example.com"),
                at("2026-08-20T08:01:00Z"),
            )
            .expect("somebody else confirms");
        assert_eq!(confirmed.status, TranscriptClaimStatus::Confirmed);
    }

    /// An unrecognised audience kind is not a kind. Reading it as a default
    /// would file the room against a relationship nobody named, and the
    /// commitment bridge would later refuse to find it.
    #[test]
    fn an_unknown_audience_kind_is_refused_rather_than_defaulted() {
        let mut row = staged_row("ingest-1", &mapping().to_string());
        row["audience_kind"] = serde_json::Value::String("vendor".to_string());
        let error = StagedIngestRequest::from_staged_row(&row).expect_err("unknown kind");
        assert!(
            error.to_string().contains("is not an audience kind"),
            "{error}"
        );
    }

    /// The package's own ceilings, re-checked. A host that trusted the app to
    /// have applied them would be trusting the side of the boundary that
    /// cannot be trusted.
    #[test]
    fn a_staged_document_over_the_reviewed_ceilings_is_refused() {
        let mut speakers = serde_json::Map::new();
        for index in 0..=MAX_STAGED_INGEST_SPEAKERS {
            speakers.insert(
                format!("k{index}"),
                serde_json::Value::String(format!("person-{index}@example.com")),
            );
        }
        let too_many_speakers = serde_json::json!({
            "speakers": serde_json::Value::Object(speakers),
            "utterances": [],
        });
        let error = StagedIngestDocument::parse(&too_many_speakers.to_string())
            .expect_err("speaker ceiling");
        assert!(error.to_string().contains("reviewed ceiling"), "{error}");

        let utterances: Vec<serde_json::Value> = (0..=MAX_STAGED_INGEST_UTTERANCES)
            .map(|_| serde_json::json!({ "speaker": "a", "text": "x" }))
            .collect();
        let too_many_utterances = serde_json::json!({
            "speakers": { "a": "founder@example.com" },
            "utterances": utterances,
        });
        let error = StagedIngestDocument::parse(&too_many_utterances.to_string())
            .expect_err("utterance ceiling");
        assert!(error.to_string().contains("reviewed ceiling"), "{error}");

        let long = "x".repeat(1_000);
        let wordy: Vec<serde_json::Value> = (0..200)
            .map(|_| serde_json::json!({ "speaker": "a", "text": long }))
            .collect();
        let too_many_words = serde_json::json!({
            "speakers": { "a": "founder@example.com" },
            "utterances": wordy,
        });
        let error =
            StagedIngestDocument::parse(&too_many_words.to_string()).expect_err("word ceiling");
        assert!(
            error.to_string().contains("reviewed transcript ceiling"),
            "{error}"
        );
    }

    /// Private/local is the one class that needs no gate. A room that already
    /// happened cannot be filed under it, or the disclosure sits outside every
    /// later review.
    #[test]
    fn a_room_that_already_happened_cannot_be_filed_as_private_local() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);
        let mut context = host_context(host_roster());
        context.consequence_class = ConsequenceClass::PrivateLocal;

        let error = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &context,
                at("2026-08-19T10:00:00Z"),
            )
            .expect_err("private/local");
        assert!(error.to_string().contains("not private/local"), "{error}");
        assert!(ingestion.claims(&scope).expect("claims").is_empty());
    }

    /// A staged request and a host-route transcript may never resolve to one
    /// act. Sharing the key namespace would let an app-chosen `ingest_id` adopt
    /// an act somebody else recorded.
    #[test]
    fn a_staged_ingest_key_cannot_collide_with_a_host_route_transcript() {
        let (_tmp, ingestion, scope) = ingestion();
        let consumer = StagedIngestConsumer::over(&ingestion);

        let mut host_source = source();
        host_source.transcript_key = "ingest-1".to_string();
        let by_route = ingestion
            .ingest_transcript(
                &scope,
                &host_source,
                &[ours("seg-1", "the route recorded this room")],
                at("2026-08-19T10:00:00Z"),
            )
            .expect("route ingest");

        let staged = consumer
            .apply(
                &scope,
                &staged_request("ingest-1", &mapping()),
                &host_context(host_roster()),
                at("2026-08-19T10:05:00Z"),
            )
            .expect("staged apply");

        assert_ne!(
            by_route.disclosure.outward_act_ref,
            staged.outward_act_ref(),
            "one id under two producers is two rooms, and neither may adopt the other's act"
        );
    }

    /// Exactly one destination applies a staged row, and it is the seam that
    /// already applies signed package decisions.
    ///
    /// # This test used to assert the opposite, and that was the point
    ///
    /// It read *"the consumer has no production caller yet"*, recording that
    /// the half a person could reach had not been built: an `ingest_request`
    /// row could never leave `apply_state: recorded`, so the package's
    /// `stage_ingest` workflow ended in a queue with no drain and every rule
    /// above held only over the empty set. Its instruction was to name the
    /// caller here and in the header when it landed.
    /// `magician/src/magician_v2/claims_decision_contribution.rs` is that
    /// caller — the one place that already holds an authenticated actor and a
    /// server-minted app scope, which is where [`StagedIngestHostContext`] has
    /// to come from.
    ///
    /// What it pins now is **which** destination, and that there is one. A
    /// second is a second place deciding which side of a room a named person
    /// is on, which is the judgement [`StagedIngestRoster`] exists to keep
    /// with the host — so it should be a deliberate change, not a discovery.
    #[test]
    fn one_host_destination_applies_a_staged_ingest() {
        use crate::magician_v2::doc_wiring_scan::scan_workspace;

        const OWN_FILE: &str = "magician/src/magician_v2/evidence/transcript_ingestion.rs";
        const DESTINATION: &str = "magician/src/magician_v2/claims_decision_contribution.rs";

        let callers = scan_workspace("StagedIngestConsumer::", &[OWN_FILE]);
        assert!(
            callers.files_searched > 100,
            "only {} files were read, so this proves nothing",
            callers.files_searched
        );
        assert_eq!(
            callers.hits,
            vec![DESTINATION.to_string()],
            "the set of destinations applying a staged ingest changed; exactly one should, and \
             a second is a second place deciding whose words are ours"
        );
    }
}
