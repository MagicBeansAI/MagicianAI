//! Recording what was said on a channel nobody could prepare — plan phase 3.
//!
//! The observed-channel writer of §4:
//!
//! ```text
//! spoken → transcript captured → statement extracted → owner-confirmed where
//! needed → appended
//! ```
//!
//! # Generic by construction
//!
//! Nothing here is about meetings specifically. It is about **any** channel
//! where the words leave before the runtime can record them — a live room, a
//! phone call, an in-person conversation someone transcribed afterwards. The
//! distinction that matters is `OutwardChannel::is_controlled`, which the store
//! already owns, so this module has no channel list of its own to drift.
//!
//! # Why extraction and assertion are separated
//!
//! A model reading a transcript produces a *guess* about what was claimed. The
//! outward assertions store answers *"what did we tell whom"*, and its answers
//! drive correction propagation — so writing an unconfirmed guess into it as an
//! assertion would put a model's reading of a room into the graph as if the
//! company had asserted it, and a later correction would chase people about
//! something nobody said.
//!
//! So: the **act** is always recorded, because it happened and the record of a
//! disclosure must not depend on how well it was understood. The **claim** is
//! recorded only once confirmed. Unconfirmed extractions come back as
//! [`PendingClaim`] for a caller to surface, rather than being written or
//! silently dropped.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::ConsequenceClass;

use super::outward_assertions::{
    OutwardActDisclosure, OutwardAssertionStore, OutwardChannel, OutwardScope, PrepareOutwardAct,
};

/// How much confidence there is that a statement asserted a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimConfirmation {
    /// The owner said yes. Only this is written as an assertion.
    OwnerConfirmed,
    /// A model read it out of a transcript and nobody has checked.
    Extracted,
}

impl ClaimConfirmation {
    /// Whether this is strong enough to record as something the company
    /// asserted.
    ///
    /// Extraction is not. It is a reading of a room, and the assertions store is
    /// consulted when a figure turns out to be wrong — chasing people over a
    /// claim nobody verified was made is worse than not finding it.
    pub fn may_be_asserted(self) -> bool {
        matches!(self, Self::OwnerConfirmed)
    }
}

/// A claim a statement appeared to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedClaim {
    pub approved_claim_ref: String,
    pub evidence_refs: Vec<String>,
    pub confirmation: ClaimConfirmation,
    /// Earlier assertions this one replaces, if the speaker corrected
    /// themselves in the room.
    pub supersedes: Vec<String>,
}

/// One statement made on an observed channel.
#[derive(Debug, Clone)]
pub struct ObservedStatement {
    pub channel: OutwardChannel,
    /// A stable key for this statement — typically the transcript segment id.
    ///
    /// The act ref derives from it, so re-processing a transcript resumes the
    /// same record instead of recording the same sentence twice. A transcript is
    /// exactly the kind of input that gets replayed after a crash, a model
    /// change or a re-run, so this is load-bearing rather than defensive.
    pub statement_key: String,
    /// Who spoke, as the runtime knows them — never as the transcript names
    /// them. A transcript's speaker labels are a model's guess.
    pub effective_speaker: String,
    /// Who was in the room.
    pub audience: Vec<String>,
    pub spoken_text: String,
    pub engagement_id: Option<String>,
    pub program_id: Option<String>,
    /// What this statement cost if it was wrong.
    ///
    /// Supplied by the caller, like every other write point's class
    /// (`prepare_dispatch`). It was hardcoded to bounded communication, which is
    /// right for *"Tuesday at three works"* and **understates** a room where
    /// financials were read out — and understating is the direction that
    /// matters, since a class is what a later reviewer reads to decide how much
    /// a disclosure cost.
    ///
    /// This module deliberately does not judge content — it cannot see whether a
    /// figure was confidential — so it takes the judgement rather than inventing
    /// one. [`ObservedStatement::spoken`] defaults it for the ordinary case.
    pub consequence_class: ConsequenceClass,
    pub claim: Option<ExtractedClaim>,
}

impl ObservedStatement {
    /// A statement on an observed channel, classified as bounded communication —
    /// words said to people who are already present.
    ///
    /// The right default because the audience is exactly who was in the room. A
    /// caller that knows more — that a data room was opened, that financials
    /// were disclosed — sets `consequence_class` afterwards.
    pub fn spoken(
        channel: OutwardChannel,
        statement_key: impl Into<String>,
        effective_speaker: impl Into<String>,
        audience: Vec<String>,
        spoken_text: impl Into<String>,
    ) -> Self {
        Self {
            channel,
            statement_key: statement_key.into(),
            effective_speaker: effective_speaker.into(),
            audience,
            spoken_text: spoken_text.into(),
            engagement_id: None,
            program_id: None,
            consequence_class: ConsequenceClass::BoundedCommunication,
            claim: None,
        }
    }
}

/// A claim that was extracted but not confirmed, returned rather than written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingClaim {
    pub outward_act_ref: String,
    pub approved_claim_ref: String,
    pub audience: Vec<String>,
    /// Why it was not recorded, in a form a surface can show verbatim.
    pub awaiting: String,
}

/// What recording a statement produced.
#[derive(Debug, Clone)]
pub struct RecordedStatement {
    pub disclosure: OutwardActDisclosure,
    /// Every assertion written, one per person in the room.
    ///
    /// A list rather than the first: a room is several audiences and each gets
    /// its own row, so returning one would leave a caller unable to reference
    /// the others.
    pub assertion_use_ids: Vec<String>,
    /// Set when a claim was extracted but not confirmed.
    pub pending: Option<PendingClaim>,
}

/// Record a statement made on an observed channel.
///
/// # Refuses a controlled channel
///
/// A channel that *could* have been prepared must not arrive here. Recording an
/// email as "observed" would mark it as something nobody could have gated, which
/// is exactly the claim the `observed` flag exists to make truthfully. The store
/// refuses it too; this refuses earlier, with a message naming the right call.
pub fn record_observed_statement(
    store: &OutwardAssertionStore,
    scope: &OutwardScope,
    statement: &ObservedStatement,
    now: &str,
) -> Result<RecordedStatement> {
    if statement.channel.is_controlled() {
        anyhow::bail!(
            "channel `{}` is controlled, so this act could have been recorded BEFORE it \
             happened. Use `prepare_dispatch`; marking it observed would claim nothing could \
             have gated it.",
            statement.channel.as_str()
        );
    }

    // The exact words are the payload. Content-addressed like any other, so the
    // record names an immutable revision of what was actually said rather than
    // whatever a transcript is edited to later.
    let exact_payload_artifact_ref = store
        .store_payload(scope, statement.spoken_text.as_bytes())
        .context("storing the spoken text for its disclosure record")?;

    let disclosure = store.record_observed_act(
        scope,
        &PrepareOutwardAct {
            idempotency_key: format!("observed:{}", statement.statement_key),
            program_id: statement.program_id.clone(),
            engagement_id: statement.engagement_id.clone(),
            exact_payload_artifact_ref,
            effective_sender: statement.effective_speaker.clone(),
            intended_audience: statement.audience.clone(),
            channel: statement.channel,
            consequence_class: statement.consequence_class.as_str().to_string(),
        },
        now,
    )?;

    let Some(claim) = statement.claim.as_ref() else {
        return Ok(RecordedStatement {
            disclosure,
            assertion_use_ids: Vec::new(),
            pending: None,
        });
    };

    // Two reasons a claim cannot be written, and both surface rather than drop.
    //
    // An empty audience is the one worth spelling out: the assertion rows are
    // per person, so a confirmed claim with nobody to aim it at would write
    // ZERO rows and return success — a claim silently absent from the reverse
    // lookup, which is the exact failure this store exists to prevent.
    let blocked = if !claim.confirmation.may_be_asserted() {
        Some("owner confirmation that this claim was actually made".to_string())
    } else if statement.audience.is_empty() {
        Some(
            "an audience: nobody is recorded as having heard this, so the claim cannot be \
             aimed at anyone"
                .to_string(),
        )
    } else {
        None
    };

    if let Some(awaiting) = blocked {
        return Ok(RecordedStatement {
            pending: Some(PendingClaim {
                outward_act_ref: disclosure.outward_act_ref.clone(),
                approved_claim_ref: claim.approved_claim_ref.clone(),
                audience: statement.audience.clone(),
                awaiting,
            }),
            disclosure,
            assertion_use_ids: Vec::new(),
        });
    }

    // One assertion per audience member: the store's uniqueness rule is
    // `(act, claim, audience)`, and a room is several audiences. Recording one
    // row for "the room" would make a later per-person correction impossible to
    // aim.
    let mut assertion_use_ids = Vec::with_capacity(statement.audience.len());
    for member in &statement.audience {
        let recorded = store.record_assertion_use(
            scope,
            &disclosure.outward_act_ref,
            &claim.approved_claim_ref,
            member,
            &claim.evidence_refs,
            &claim.supersedes,
            now,
        )?;
        assertion_use_ids.push(recorded.assertion_use_id);
    }

    Ok(RecordedStatement {
        disclosure,
        assertion_use_ids,
        pending: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    fn store() -> (tempfile::TempDir, OutwardAssertionStore, OutwardScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = OutwardAssertionStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, OutwardScope::new("anonymous", "default"))
    }

    fn statement(key: &str, claim: Option<ExtractedClaim>) -> ObservedStatement {
        ObservedStatement {
            channel: OutwardChannel::Meeting,
            statement_key: key.to_string(),
            effective_speaker: "company-assistant".to_string(),
            audience: vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string(),
            ],
            spoken_text: "our revenue last quarter was 400k".to_string(),
            engagement_id: Some("eng-1".to_string()),
            program_id: None,
            consequence_class: ConsequenceClass::BoundedCommunication,
            claim,
        }
    }

    fn confirmed() -> ExtractedClaim {
        ExtractedClaim {
            approved_claim_ref: "claim-revenue".to_string(),
            evidence_refs: vec!["evidence-books".to_string()],
            confirmation: ClaimConfirmation::OwnerConfirmed,
            supersedes: Vec::new(),
        }
    }

    /// The act is recorded whether or not anyone understood what was claimed:
    /// the record of a disclosure must not depend on how well it was read.
    #[test]
    fn a_statement_with_no_claim_still_records_the_act() {
        let (_tmp, store, scope) = store();
        let recorded = record_observed_statement(&store, &scope, &statement("seg-1", None), "t0")
            .expect("record");

        assert!(
            recorded.disclosure.observed,
            "the observed mark is load-bearing"
        );
        assert!(recorded.assertion_use_ids.is_empty());
        assert!(recorded.pending.is_none());
        assert_eq!(
            recorded.disclosure.intended_audience,
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string()
            ]
        );
    }

    /// A confirmed claim is asserted once PER AUDIENCE MEMBER. A room is several
    /// audiences, and one row for "the room" would make a later per-person
    /// correction impossible to aim.
    #[test]
    fn a_confirmed_claim_is_asserted_once_per_person_in_the_room() {
        let (_tmp, store, scope) = store();
        let recorded =
            record_observed_statement(&store, &scope, &statement("seg-1", Some(confirmed())), "t0")
                .expect("record");

        assert_eq!(
            recorded.assertion_use_ids.len(),
            2,
            "one row per person in the room"
        );
        assert!(recorded.pending.is_none());

        let affected = store
            .disclosures_carrying_claim(&scope, "claim-revenue")
            .expect("reverse lookup");
        let mut audiences: Vec<String> = affected.iter().map(|row| row.audience.clone()).collect();
        audiences.sort();
        assert_eq!(
            audiences,
            vec![
                "alice@example.com".to_string(),
                "bob@example.com".to_string()
            ],
            "each person in the room can be corrected individually"
        );
    }

    /// An unconfirmed extraction is a model's reading of a room. Writing it as an
    /// assertion would put a guess into the graph that correction propagation
    /// chases people over.
    #[test]
    fn an_unconfirmed_extraction_is_pending_rather_than_asserted() {
        let (_tmp, store, scope) = store();
        let mut claim = confirmed();
        claim.confirmation = ClaimConfirmation::Extracted;

        let recorded =
            record_observed_statement(&store, &scope, &statement("seg-1", Some(claim)), "t0")
                .expect("record");

        assert!(
            recorded.assertion_use_ids.is_empty(),
            "nothing may be asserted on a model's reading alone"
        );
        let pending = recorded
            .pending
            .expect("the extraction is surfaced, not dropped");
        assert_eq!(pending.approved_claim_ref, "claim-revenue");
        assert!(!pending.awaiting.is_empty());

        // And it is genuinely absent from the reverse lookup.
        assert!(store
            .disclosures_carrying_claim(&scope, "claim-revenue")
            .expect("reverse lookup")
            .is_empty());

        // The act itself is still on the record.
        assert!(store
            .load_act(&scope, &recorded.disclosure.outward_act_ref)
            .expect("load")
            .is_some());
        assert!(!ClaimConfirmation::Extracted.may_be_asserted());
        assert!(ClaimConfirmation::OwnerConfirmed.may_be_asserted());
    }

    /// A transcript is exactly the kind of input that gets replayed — after a
    /// crash, a model change, or a re-run. Re-processing must resume the same
    /// record rather than record the same sentence twice.
    #[test]
    fn reprocessing_a_transcript_resumes_rather_than_duplicates() {
        let (_tmp, store, scope) = store();
        let first =
            record_observed_statement(&store, &scope, &statement("seg-1", Some(confirmed())), "t0")
                .expect("first");
        let again =
            record_observed_statement(&store, &scope, &statement("seg-1", Some(confirmed())), "t1")
                .expect("replay");

        assert_eq!(
            first.disclosure.outward_act_ref, again.disclosure.outward_act_ref,
            "one statement is one act"
        );
        assert_eq!(
            again.disclosure.prepared_at, "t0",
            "the replay must not rewrite when it happened"
        );

        let affected = store
            .disclosures_carrying_claim(&scope, "claim-revenue")
            .expect("reverse lookup");
        assert_eq!(affected.len(), 2, "two people, not four rows");
    }

    /// A room where financials were read out is not bounded communication, and
    /// understating it is the direction that matters — the class is what a later
    /// reviewer reads to decide how much a disclosure cost.
    #[test]
    fn a_caller_may_classify_a_statement_above_the_default() {
        let (_tmp, store, scope) = store();

        let ordinary = ObservedStatement::spoken(
            OutwardChannel::Meeting,
            "seg-1",
            "company-assistant",
            vec!["alice@example.com".to_string()],
            "tuesday at three works",
        );
        assert_eq!(
            ordinary.consequence_class,
            ConsequenceClass::BoundedCommunication,
            "the default suits words said to people already in the room"
        );

        let mut disclosed = ObservedStatement::spoken(
            OutwardChannel::Meeting,
            "seg-2",
            "company-assistant",
            vec!["alice@example.com".to_string()],
            "our revenue last quarter was 400k",
        );
        disclosed.consequence_class = ConsequenceClass::ConfidentialDisclosure;

        let recorded = record_observed_statement(&store, &scope, &disclosed, "t0").expect("record");
        assert_eq!(
            recorded.disclosure.consequence_class,
            ConsequenceClass::ConfidentialDisclosure.as_str(),
            "the caller's judgement is what lands on the record"
        );
    }

    /// A channel that COULD have been prepared must not be recorded as observed:
    /// that would claim nothing could have gated it.
    #[test]
    fn a_controlled_channel_is_refused_with_the_right_alternative() {
        let (_tmp, store, scope) = store();
        let mut controlled = statement("seg-1", None);
        controlled.channel = OutwardChannel::Email;

        let error = record_observed_statement(&store, &scope, &controlled, "t0")
            .expect_err("email could have been prepared");
        let message = error.to_string();
        assert!(message.contains("controlled"));
        assert!(
            message.contains("prepare_dispatch"),
            "a refusal must name the call that would have worked"
        );
    }

    /// A confirmed claim with nobody to aim it at would write ZERO assertion rows
    /// and return success — a claim silently absent from the reverse lookup,
    /// which is the exact failure this store exists to prevent.
    #[test]
    fn a_confirmed_claim_with_no_audience_is_pending_rather_than_silently_dropped() {
        let (_tmp, store, scope) = store();
        let mut unheard = statement("seg-1", Some(confirmed()));
        unheard.audience = Vec::new();

        let recorded = record_observed_statement(&store, &scope, &unheard, "t0").expect("record");

        assert!(recorded.assertion_use_ids.is_empty());
        let pending = recorded
            .pending
            .expect("a claim nobody can be told about must be surfaced, not dropped");
        assert!(pending.awaiting.contains("audience"));

        // The act still happened and is still on the record.
        assert!(store
            .load_act(&scope, &recorded.disclosure.outward_act_ref)
            .expect("load")
            .is_some());
    }

    /// A retraction reaches everyone who heard it, which is the whole reason the
    /// per-person rows exist.
    #[test]
    fn correcting_a_claim_raises_one_obligation_per_person_who_heard_it() {
        let (_tmp, store, scope) = store();
        record_observed_statement(&store, &scope, &statement("seg-1", Some(confirmed())), "t0")
            .expect("record");

        let obligations = store
            .raise_correction_obligations(&scope, "claim-revenue", "correction-1", "t1")
            .expect("raise");
        assert_eq!(
            obligations.len(),
            2,
            "both people in the room are owed the correction"
        );
    }
}
