//! Terms that appeared in a conversation — deal close §6A.
//!
//! Plan: `docs/plans/2026-08-07-opc-deal-close.md` §6A.
//! Doc: `docs/components/magician/commitments.md`.
//!
//! §6A: *"Nothing in this set negotiates, and that exclusion is correct. But
//! being unable to negotiate is not a reason to be **blind** to negotiation
//! happening."*
//!
//! When terms appear — a number, a timeline, a condition, an offer — they are
//! recorded as a **fact awaiting owner confirmation**.
//!
//! # The two rules that are the whole feature
//!
//! 1. **It never asserts a commitment outward.** Recording that they offered
//!    something is not agreeing to it, and the agent may not restate an
//!    unconfirmed commitment to anyone.
//! 2. **Anything the agent believes *we* have committed to requires owner
//!    confirmation** before any other module treats it as true.
//!
//! Both are enforced by [`Commitment::may_be_restated_outward`], which is false
//! for everything except a confirmed entry. There is no way to ask this type for
//! permission and get a yes on a guess.
//!
//! # Why `stated_by_us` unconfirmed is the interesting row
//!
//! §6A: *"a `stated_by_us` entry the owner never confirmed is exactly the thing
//! worth finding before the other side does."* The agent believes we promised
//! something and nobody has checked. [`Commitments::unconfirmed_from_us`] is
//! that query, and it exists because the alternative is finding out when the
//! counterparty quotes it back.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;
use crate::magician_v2::evidence::completion_journal::{
    EvidenceCompletionJournal, EvidenceDecisionCompletion, EvidenceDecisionScope,
    EvidenceDecisionTarget,
};
use crate::magician_v2::evidence::store_cursor::{StoreFoldCursor, StoreFoldIndex};
use crate::magician_v2::execution::file_edit::transaction::acquire_record_decision_lock;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

#[cfg(test)]
mod tests;

const FIELD_SEP: char = '\u{1f}';
/// Names this register in a cancelled-fold refusal.
const COMMITMENT_REGISTER: &str = "commitment register";
const COMMITMENT_DECISION_INDEX_SCHEMA: u32 = 1;
const MAX_COMMITMENT_DECISION_INDEX_BYTES: u64 = 16 * 1024;

/// Which side said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentDirection {
    /// They offered it. Recording it is not accepting it.
    OfferedToUs,
    /// We appear to have said it. **This is the one that needs checking** —
    /// the agent believing we promised something is not the same as our having
    /// promised it.
    StatedByUs,
}

impl CommitmentDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OfferedToUs => "offered_to_us",
            Self::StatedByUs => "stated_by_us",
        }
    }
}

/// Where a recorded term stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentStatus {
    /// Recorded, nobody has checked it. The default and the only status a
    /// machine may assign.
    Unconfirmed,
    /// The owner said yes, this is real.
    Confirmed,
    /// A later term replaced it.
    Superseded,
    /// It was taken back.
    Withdrawn,
}

impl CommitmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfirmed => "unconfirmed",
            Self::Confirmed => "confirmed",
            Self::Superseded => "superseded",
            Self::Withdrawn => "withdrawn",
        }
    }

    /// Whether this term is currently in force.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Unconfirmed | Self::Confirmed)
    }
}

/// One set of terms, as they appeared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commitment {
    pub commitment_id: String,
    /// The relationship the terms appeared in — an engagement, an account, a
    /// panel. Terms get said in every kind of conversation, not only a deal.
    pub audience: AudienceRef,
    /// The message or transcript it came from, so the owner can read the words
    /// rather than the summary before confirming anything.
    pub source_ref: String,
    pub direction: CommitmentDirection,
    /// The terms, as stated. Free text: a taxonomy of deal terms would be a
    /// modelling exercise, and the owner confirms by reading, not by filtering.
    pub terms: String,
    pub stated_at: DateTime<Utc>,
    pub status: CommitmentStatus,
    pub recorded_at: DateTime<Utc>,
    /// Who confirmed it, when somebody did. A confirmation with no name is not
    /// a confirmation.
    pub confirmed_by: Option<String>,
    pub confirmed_at: Option<DateTime<Utc>>,
    /// The commitment that replaced this one.
    pub superseded_by: Option<String>,
    /// Monotonic optimistic-concurrency revision. Historical recorded rows
    /// deserialize at revision one; every accepted transition increments it.
    #[serde(default = "initial_revision")]
    pub revision: u64,
}

const fn initial_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentDecisionVerb {
    Record,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentDecisionDisposition {
    Applied,
    AlreadyApplied,
}

/// Receipt persisted in the same append-only register record as the
/// authoritative commitment transition. The package audit ledger is a
/// rebuildable projection of these receipts, never a second authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentDecisionReceipt {
    pub receipt_id: String,
    pub decision_id: String,
    pub commitment_id: String,
    /// Complete relationship address for bounded, cross-shard receipt recovery.
    /// This is `Option` only so receipts written before the decision index was
    /// introduced remain deserializable; every new receipt stores `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<AudienceRef>,
    pub verb: CommitmentDecisionVerb,
    pub expected_revision: u64,
    pub resulting_revision: u64,
    /// Digest of every semantically relevant request field. A decision id is
    /// an idempotency key, not permission to substitute a different request on
    /// retry.
    pub request_fingerprint: String,
    /// Named confirmer for `Confirm`; `None` for the mechanical `Record` verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub disposition: CommitmentDecisionDisposition,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentDecisionOutcome {
    pub commitment: Commitment,
    pub receipt: CommitmentDecisionReceipt,
}

impl Commitment {
    /// Whether the agent may state this to anyone outside.
    ///
    /// **False unless confirmed.** Both of §6A's rules reduce to this one
    /// predicate, so there is a single place to get it wrong and a single place
    /// to check it.
    ///
    /// Being unconfirmed is not a technicality. An agent restating a term the
    /// owner never agreed to has, in effect, negotiated — which is the exclusion
    /// §6A exists to preserve.
    pub fn may_be_restated_outward(&self) -> bool {
        matches!(self.status, CommitmentStatus::Confirmed)
    }

    /// Whether this is the row §6A says to go looking for: we appear to have
    /// promised something and nobody has checked.
    pub fn needs_owner_check(&self) -> bool {
        self.direction == CommitmentDirection::StatedByUs
            && self.status == CommitmentStatus::Unconfirmed
    }
}

/// What a caller supplies. `status` is **not** among them: a machine may only
/// record `Unconfirmed`, and letting a caller hand in `Confirmed` would make
/// owner confirmation something the agent could grant itself.
#[derive(Debug, Clone)]
pub struct RecordCommitment {
    pub audience: AudienceRef,
    pub source_ref: String,
    pub direction: CommitmentDirection,
    pub terms: String,
    pub stated_at: DateTime<Utc>,
}

/// Exact request identity used for read-only decision-receipt recovery.
/// Supplying the domain request rather than a caller-computed digest keeps the
/// fingerprint vocabulary destination-owned.
#[derive(Debug, Clone, Copy)]
pub enum CommitmentDecisionReceiptRequest<'a> {
    Record {
        request: &'a RecordCommitment,
        expected_claim_revision: u64,
    },
    Confirm {
        audience: &'a AudienceRef,
        commitment_id: &'a str,
        expected_revision: u64,
        by: &'a str,
    },
}

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitmentScope {
    pub principal: String,
    pub workspace: String,
}

impl CommitmentScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum CommitmentRecord {
    /// Historical wire shape. Keep this tuple variant so every existing JSONL
    /// row remains readable byte-for-byte.
    Recorded(Commitment),
    RecordedWithReceipt {
        commitment: Commitment,
        receipt: CommitmentDecisionReceipt,
    },
    Confirmed {
        commitment_id: String,
        by: String,
        at: DateTime<Utc>,
    },
    ConfirmedWithReceipt {
        commitment_id: String,
        by: String,
        at: DateTime<Utc>,
        receipt: CommitmentDecisionReceipt,
    },
    Superseded {
        commitment_id: String,
        by_commitment: String,
        at: DateTime<Utc>,
    },
    Withdrawn {
        commitment_id: String,
        at: DateTime<Utc>,
    },
}

/// Scope-wide decision binding. Audience registers remain independently
/// sharded, so this deterministic locator is what makes one decision id one
/// request across the complete scope rather than merely within one audience.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitmentDecisionIndex {
    schema_version: u32,
    decision_id: String,
    audience: AudienceRef,
    commitment_id: String,
    verb: CommitmentDecisionVerb,
    expected_revision: u64,
    request_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receipt: Option<CommitmentDecisionReceipt>,
}

/// The commitment register, per audience.
#[derive(Debug, Clone)]
pub struct Commitments {
    workspace_layout: ArtifactV2Workspace,
}

impl Commitments {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn path(&self, scope: &CommitmentScope, audience: &AudienceRef) -> PathBuf {
        self.root(scope)
            .join(format!("{}.jsonl", stable_id(&audience.as_key())))
    }

    fn root(&self, scope: &CommitmentScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("commitments")
    }

    fn decision_index_path(&self, scope: &CommitmentScope, decision_id: &str) -> PathBuf {
        self.root(scope)
            .join("decisions")
            .join(format!("{}.json", stable_id(decision_id)))
    }

    /// Record a receipted transition in the scope's completion journal.
    ///
    /// The decision index above is keyed by `blake3(decision_id)`, so walking
    /// it yields hash order — an order a later decision can be inserted into,
    /// behind a reader. That is why the removed scope-wide receipt page was
    /// lossy and why this journal exists; see
    /// [`crate::magician_v2::evidence::completion_journal`].
    ///
    /// Called on the first-write branch **and** on the replay branch. The
    /// register row is authoritative and lands first, so a crash between it
    /// and this call would otherwise lose the completion outright; the replay
    /// is where that is repaired, and journalling is idempotent by decision id
    /// so an ordinary retry adds nothing.
    fn journal_completion(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        receipt: &CommitmentDecisionReceipt,
        now: DateTime<Utc>,
    ) -> Result<()> {
        EvidenceCompletionJournal::new(self.workspace_layout.clone())
            .record_completion(
                &EvidenceDecisionScope::new(scope.principal.clone(), scope.workspace.clone()),
                &EvidenceDecisionCompletion {
                    decision_id: receipt.decision_id.clone(),
                    target: EvidenceDecisionTarget::Commitment {
                        audience: audience.clone(),
                        commitment_id: receipt.commitment_id.clone(),
                    },
                    receipt_id: receipt.receipt_id.clone(),
                    request_fingerprint: receipt.request_fingerprint.clone(),
                    completed_at: receipt.recorded_at,
                },
                now,
            )
            .with_context(|| {
                format!(
                    "journalling completed commitment decision `{}`",
                    receipt.decision_id
                )
            })?;
        Ok(())
    }

    /// Record terms as they appeared.
    ///
    /// Always `Unconfirmed`. There is no parameter for status and no path that
    /// produces a confirmed row without a named person confirming it.
    pub fn record(
        &self,
        scope: &CommitmentScope,
        request: &RecordCommitment,
        now: DateTime<Utc>,
    ) -> Result<Commitment> {
        if !request.audience.is_named() {
            anyhow::bail!("a commitment must name the relationship it appeared in");
        }
        if request.terms.trim().is_empty() {
            anyhow::bail!("a commitment with no terms is nothing to confirm");
        }
        if request.source_ref.trim().is_empty() {
            anyhow::bail!(
                "a commitment must name its source: the owner confirms by reading the words, \
                 not the summary"
            );
        }

        let commitment_id = derive_commitment_id(scope, request);
        let path = self.path(scope, &request.audience);
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let commitment_lock_id = format!("commitment-{}", stable_id(&commitment_id));
        let _commitment_guard =
            acquire_record_decision_lock(lock_root, &commitment_lock_id, "commitment")?;
        if let Some(existing) = self.load(scope, &request.audience, &commitment_id)? {
            return Ok(existing);
        }

        let commitment = Commitment {
            commitment_id,
            audience: request.audience.clone(),
            source_ref: request.source_ref.clone(),
            direction: request.direction,
            terms: request.terms.clone(),
            stated_at: request.stated_at,
            status: CommitmentStatus::Unconfirmed,
            recorded_at: now,
            confirmed_by: None,
            confirmed_at: None,
            superseded_by: None,
            revision: initial_revision(),
        };
        self.append(&path, &CommitmentRecord::Recorded(commitment.clone()))?;
        Ok(commitment)
    }

    /// Record a commitment and its destination receipt under one
    /// cross-process decision lock. An identical decision id is a clean replay;
    /// a stale source revision or substituted decision id fails closed.
    pub fn record_at_claim_revision(
        &self,
        scope: &CommitmentScope,
        request: &RecordCommitment,
        expected_claim_revision: u64,
        decision_id: &str,
        now: DateTime<Utc>,
    ) -> Result<CommitmentDecisionOutcome> {
        self.record_at_claim_revision_guarded(
            scope,
            request,
            expected_claim_revision,
            decision_id,
            now,
            || Ok(()),
        )
    }

    /// Guarded form of [`Self::record_at_claim_revision`]. Exact receipt
    /// replay bypasses the callback. A new authoritative record invokes it
    /// while both destination locks are held and immediately before the first
    /// durable index or register write.
    pub fn record_at_claim_revision_guarded<F>(
        &self,
        scope: &CommitmentScope,
        request: &RecordCommitment,
        expected_claim_revision: u64,
        decision_id: &str,
        now: DateTime<Utc>,
        admit_write: F,
    ) -> Result<CommitmentDecisionOutcome>
    where
        F: FnOnce() -> Result<()>,
    {
        validate_decision_id(decision_id)?;
        if expected_claim_revision == 0 {
            anyhow::bail!("expected claim revision must be positive");
        }
        validate_record_request(request)?;
        let commitment_id = derive_commitment_id(scope, request);
        let path = self.path(scope, &request.audience);
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let decision_lock_id = format!("decision-{}", stable_id(decision_id));
        let commitment_lock_id = format!("commitment-{}", stable_id(&commitment_id));
        let _decision_guard =
            acquire_record_decision_lock(lock_root, &decision_lock_id, "commitment decision")?;
        let _commitment_guard =
            acquire_record_decision_lock(lock_root, &commitment_lock_id, "commitment")?;
        let mut index = self.decision_index_for_request(
            scope,
            decision_id,
            CommitmentDecisionReceiptRequest::Record {
                request,
                expected_claim_revision,
            },
        )?;
        let index_is_prepared =
            if let Some(existing) = self.read_decision_index(scope, decision_id)? {
                validate_decision_index_match(&existing, &index)?;
                index = existing;
                true
            } else {
                false
            };

        let recovered = if index_is_prepared {
            self.recover_indexed_receipt(scope, &mut index)?
        } else {
            self.receipt_for_decision(scope, &request.audience, decision_id)?
                // Annotated for the same reason as its sibling above: neither
                // the `?` nor the `Ok` pins an error type here.
                .map(|receipt| -> Result<CommitmentDecisionReceipt> {
                    validate_receipt_against_index(&receipt, &index)?;
                    Ok(enrich_receipt_from_index(receipt, &index))
                })
                .transpose()?
        };
        if let Some(receipt) = recovered {
            let commitment = self
                .load(scope, &request.audience, &commitment_id)?
                .context("receipt points at a missing commitment")?;
            self.journal_completion(scope, &request.audience, &receipt, now)?;
            return Ok(CommitmentDecisionOutcome {
                commitment,
                receipt: replay_receipt(receipt),
            });
        }
        if self
            .load(scope, &request.audience, &commitment_id)?
            .is_some()
        {
            anyhow::bail!(
                "commitment `{commitment_id}` already exists under a different decision; refusing to mint a second receipt"
            );
        }
        let commitment = Commitment {
            commitment_id: commitment_id.clone(),
            audience: request.audience.clone(),
            source_ref: request.source_ref.clone(),
            direction: request.direction,
            terms: request.terms.clone(),
            stated_at: request.stated_at,
            status: CommitmentStatus::Unconfirmed,
            recorded_at: now,
            confirmed_by: None,
            confirmed_at: None,
            superseded_by: None,
            revision: initial_revision(),
        };
        let receipt = commitment_receipt(
            decision_id,
            &commitment_id,
            CommitmentDecisionVerb::Record,
            expected_claim_revision,
            commitment.revision,
            &request.audience,
            None,
            index.request_fingerprint.clone(),
            now,
        );
        admit_write()?;
        if !index_is_prepared {
            self.write_decision_index(scope, &index)?;
        }
        self.append(
            &path,
            &CommitmentRecord::RecordedWithReceipt {
                commitment: commitment.clone(),
                receipt: receipt.clone(),
            },
        )?;
        index.receipt = Some(receipt.clone());
        self.write_decision_index(scope, &index)?;
        self.journal_completion(scope, &request.audience, &receipt, now)?;
        Ok(CommitmentDecisionOutcome {
            commitment,
            receipt,
        })
    }

    /// The owner says a term is real.
    ///
    /// `by` is required and must name somebody: a confirmation with no name is
    /// not a confirmation, and an empty string would let an automated caller
    /// satisfy the one control this feature has.
    pub fn confirm(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<Commitment> {
        if by.trim().is_empty() {
            anyhow::bail!(
                "a confirmation must name who confirmed it; an unnamed confirmation is how an \
                agent would grant itself the one control this register has"
            );
        }
        let path = self.path(scope, audience);
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let commitment_lock_id = format!("commitment-{}", stable_id(commitment_id));
        let _commitment_guard =
            acquire_record_decision_lock(lock_root, &commitment_lock_id, "commitment")?;
        let Some(commitment) = self.load(scope, audience, commitment_id)? else {
            anyhow::bail!("no commitment `{commitment_id}` on `{}`", audience.as_key());
        };
        if !commitment.status.is_live() {
            anyhow::bail!(
                "commitment `{commitment_id}` is {} and cannot be confirmed",
                commitment.status.as_str()
            );
        }
        if commitment.status == CommitmentStatus::Confirmed {
            return Ok(commitment);
        }
        self.append(
            &path,
            &CommitmentRecord::Confirmed {
                commitment_id: commitment_id.to_string(),
                by: by.to_string(),
                at: now,
            },
        )?;
        self.load(scope, audience, commitment_id)?
            .context("commitment vanished immediately after confirmation")
    }

    /// Revision-bound, receipt-bearing owner confirmation. This is the shared
    /// transition used by the claims-decision destination and HTTP surface.
    pub fn confirm_at_revision(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        expected_revision: u64,
        decision_id: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<CommitmentDecisionOutcome> {
        self.confirm_at_revision_guarded(
            scope,
            audience,
            commitment_id,
            expected_revision,
            decision_id,
            by,
            now,
            || Ok(()),
        )
    }

    /// Guarded form of [`Self::confirm_at_revision`]. Exact receipt replay
    /// bypasses the callback. Every new confirmation invokes it with both
    /// destination locks held immediately before the first durable write.
    #[allow(clippy::too_many_arguments)]
    pub fn confirm_at_revision_guarded<F>(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        expected_revision: u64,
        decision_id: &str,
        by: &str,
        now: DateTime<Utc>,
        admit_write: F,
    ) -> Result<CommitmentDecisionOutcome>
    where
        F: FnOnce() -> Result<()>,
    {
        validate_decision_id(decision_id)?;
        if !is_safe_scope_id(commitment_id) {
            anyhow::bail!("commitment id failed the safe-identifier check");
        }
        if expected_revision == 0 {
            anyhow::bail!("expected commitment revision must be positive");
        }
        if !audience.is_named() {
            anyhow::bail!("a commitment confirmation must name its audience");
        }
        if by.trim().is_empty() {
            anyhow::bail!("a confirmation must name who confirmed it");
        }
        let path = self.path(scope, audience);
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let decision_lock_id = format!("decision-{}", stable_id(decision_id));
        let commitment_lock_id = format!("commitment-{}", stable_id(commitment_id));
        let _decision_guard =
            acquire_record_decision_lock(lock_root, &decision_lock_id, "commitment decision")?;
        let _commitment_guard =
            acquire_record_decision_lock(lock_root, &commitment_lock_id, "commitment")?;
        let mut index = self.decision_index_for_request(
            scope,
            decision_id,
            CommitmentDecisionReceiptRequest::Confirm {
                audience,
                commitment_id,
                expected_revision,
                by,
            },
        )?;
        let index_is_prepared =
            if let Some(existing) = self.read_decision_index(scope, decision_id)? {
                validate_decision_index_match(&existing, &index)?;
                index = existing;
                true
            } else {
                false
            };
        let recovered = if index_is_prepared {
            self.recover_indexed_receipt(scope, &mut index)?
        } else {
            self.receipt_for_decision(scope, audience, decision_id)?
                // Annotated because the closure's `?` and its `Ok` do not
                // between them pin an error type, and `Result` here is
                // `anyhow::Result` rather than something inferable from the
                // surrounding expression.
                .map(|receipt| -> Result<CommitmentDecisionReceipt> {
                    validate_receipt_against_index(&receipt, &index)?;
                    Ok(enrich_receipt_from_index(receipt, &index))
                })
                .transpose()?
        };
        if let Some(receipt) = recovered {
            let commitment = self
                .load(scope, audience, commitment_id)?
                .context("receipt points at a missing commitment")?;
            self.journal_completion(scope, audience, &receipt, now)?;
            return Ok(CommitmentDecisionOutcome {
                commitment,
                receipt: replay_receipt(receipt),
            });
        }
        let commitment = self
            .load(scope, audience, commitment_id)?
            .with_context(|| {
                format!("no commitment `{commitment_id}` on `{}`", audience.as_key())
            })?;
        if commitment.revision != expected_revision {
            anyhow::bail!(
                "stale commitment revision: expected {expected_revision}, current {}",
                commitment.revision
            );
        }
        if commitment.status != CommitmentStatus::Unconfirmed {
            anyhow::bail!(
                "commitment `{commitment_id}` is {} and cannot accept a new confirmation",
                commitment.status.as_str()
            );
        }
        let resulting_revision = expected_revision
            .checked_add(1)
            .context("commitment revision overflow")?;
        let receipt = commitment_receipt(
            decision_id,
            commitment_id,
            CommitmentDecisionVerb::Confirm,
            expected_revision,
            resulting_revision,
            audience,
            Some(by),
            index.request_fingerprint.clone(),
            now,
        );
        admit_write()?;
        if !index_is_prepared {
            self.write_decision_index(scope, &index)?;
        }
        self.append(
            &path,
            &CommitmentRecord::ConfirmedWithReceipt {
                commitment_id: commitment_id.to_owned(),
                by: by.to_owned(),
                at: now,
                receipt: receipt.clone(),
            },
        )?;
        index.receipt = Some(receipt.clone());
        self.write_decision_index(scope, &index)?;
        let commitment = self
            .load(scope, audience, commitment_id)?
            .context("commitment vanished immediately after confirmation")?;
        self.journal_completion(scope, audience, &receipt, now)?;
        Ok(CommitmentDecisionOutcome {
            commitment,
            receipt,
        })
    }

    /// A later term replaced this one.
    pub fn supersede(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        by_commitment: &str,
        now: DateTime<Utc>,
    ) -> Result<Commitment> {
        if commitment_id == by_commitment {
            anyhow::bail!("a commitment cannot supersede itself");
        }
        self.transition(
            scope,
            audience,
            commitment_id,
            CommitmentRecord::Superseded {
                commitment_id: commitment_id.to_string(),
                by_commitment: by_commitment.to_string(),
                at: now,
            },
        )
    }

    /// It was taken back.
    pub fn withdraw(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Commitment> {
        self.transition(
            scope,
            audience,
            commitment_id,
            CommitmentRecord::Withdrawn {
                commitment_id: commitment_id.to_string(),
                at: now,
            },
        )
    }

    fn transition(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        record: CommitmentRecord,
    ) -> Result<Commitment> {
        let path = self.path(scope, audience);
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let commitment_lock_id = format!("commitment-{}", stable_id(commitment_id));
        let _commitment_guard =
            acquire_record_decision_lock(lock_root, &commitment_lock_id, "commitment")?;
        let Some(commitment) = self.load(scope, audience, commitment_id)? else {
            anyhow::bail!("no commitment `{commitment_id}` on `{}`", audience.as_key());
        };
        if !commitment.status.is_live() {
            return Ok(commitment);
        }
        self.append(&path, &record)?;
        self.load(scope, audience, commitment_id)?
            .context("commitment vanished immediately after a transition")
    }

    /// Everything recorded on one audience, oldest first.
    pub fn for_audience(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
    ) -> Result<Vec<Commitment>> {
        self.fold_path(
            &self.path(scope, audience),
            StoreFoldCursor::new(COMMITMENT_REGISTER),
        )
    }

    /// [`Self::for_audience`], abandoned when `cancellation` fires.
    ///
    /// For the caller that has stopped waiting. Isolating this fold on a
    /// blocking worker keeps a large shard off the async runtime, but dropping
    /// the join handle when an outer timeout fires does not stop the thread —
    /// it finishes folding a register nobody will read. A cancelled fold
    /// returns [`crate::magician_v2::evidence::StoreFoldCancelled`] and no
    /// rows; see that module for why a partial register is not a smaller
    /// answer but a different one.
    pub fn for_audience_until_cancelled(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Commitment>> {
        self.fold_path(
            &self.path(scope, audience),
            StoreFoldCursor::cancelled_by(COMMITMENT_REGISTER, cancellation),
        )
    }

    fn fold_path(&self, path: &PathBuf, mut cursor: StoreFoldCursor) -> Result<Vec<Commitment>> {
        // Before the read, so a fold that was already abandoned does no I/O,
        // and again before the parse, which is the one stretch inside
        // `magician_v2::jsonl` the per-record check cannot reach into.
        cursor.checkpoint()?;
        let Some(raw) = self.read_if_present(path)? else {
            return Ok(Vec::new());
        };
        cursor.checkpoint()?;

        // The index is the `seen` set and the transition lookup at once — they
        // were always the same question, and asking it linearly made the fold
        // quadratic in the size of the shard.
        let mut index = StoreFoldIndex::new();
        let mut out: Vec<Commitment> = Vec::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in crate::magician_v2::jsonl::parse_log_lines::<CommitmentRecord>(&raw, path)? {
            cursor.admit()?;
            match record {
                CommitmentRecord::Recorded(commitment) => {
                    let commitment_id = commitment.commitment_id.clone();
                    index.push_head(&mut out, commitment_id, commitment);
                },
                CommitmentRecord::RecordedWithReceipt {
                    commitment,
                    receipt,
                } => {
                    validate_record_receipt(&commitment, &receipt)?;
                    let commitment_id = commitment.commitment_id.clone();
                    index.push_head(&mut out, commitment_id, commitment);
                },
                CommitmentRecord::Confirmed {
                    commitment_id,
                    by,
                    at,
                } => {
                    if let Some(held) = index.head_mut(&mut out, &commitment_id) {
                        if held.status == CommitmentStatus::Unconfirmed {
                            held.status = CommitmentStatus::Confirmed;
                            held.confirmed_by = Some(by);
                            held.confirmed_at = Some(at);
                            held.revision = held.revision.saturating_add(1);
                        }
                    }
                },
                CommitmentRecord::ConfirmedWithReceipt {
                    commitment_id,
                    by,
                    at,
                    receipt,
                } => {
                    if let Some(held) = index.head_mut(&mut out, &commitment_id) {
                        validate_confirm_receipt(held, &commitment_id, &by, &receipt)?;
                        if held.status == CommitmentStatus::Unconfirmed {
                            held.status = CommitmentStatus::Confirmed;
                            held.confirmed_by = Some(by);
                            held.confirmed_at = Some(at);
                            held.revision = receipt.resulting_revision;
                        }
                    }
                },
                CommitmentRecord::Superseded {
                    commitment_id,
                    by_commitment,
                    ..
                } => {
                    if let Some(held) = index.head_mut(&mut out, &commitment_id) {
                        if held.status.is_live() {
                            held.status = CommitmentStatus::Superseded;
                            held.superseded_by = Some(by_commitment);
                            held.revision = held.revision.saturating_add(1);
                        }
                    }
                },
                CommitmentRecord::Withdrawn { commitment_id, .. } => {
                    if let Some(held) = index.head_mut(&mut out, &commitment_id) {
                        if held.status.is_live() {
                            held.status = CommitmentStatus::Withdrawn;
                            held.revision = held.revision.saturating_add(1);
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    pub fn load(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
    ) -> Result<Option<Commitment>> {
        Ok(self
            .for_audience(scope, audience)?
            .into_iter()
            .find(|held| held.commitment_id == commitment_id))
    }

    /// **The query §6A exists for.** Terms the agent believes *we* stated, that
    /// nobody has confirmed.
    ///
    /// *"Exactly the thing worth finding before the other side does."*
    pub fn unconfirmed_from_us(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
    ) -> Result<Vec<Commitment>> {
        Ok(self
            .for_audience(scope, audience)?
            .into_iter()
            .filter(Commitment::needs_owner_check)
            .collect())
    }

    /// Terms the agent may actually repeat to someone outside.
    ///
    /// Confirmed only. A caller composing a message should build from this and
    /// nothing else.
    pub fn restatable(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
    ) -> Result<Vec<Commitment>> {
        Ok(self
            .for_audience(scope, audience)?
            .into_iter()
            .filter(Commitment::may_be_restated_outward)
            .collect())
    }

    /// Recover one exact durable decision receipt without re-entering the
    /// first-write CAS path. The deterministic scope-wide index identifies at
    /// most one audience shard; the supplied domain request is recomputed and
    /// compared in full before any receipt is returned.
    ///
    /// # Why a recovery path takes a clock and writes
    ///
    /// The register row is authoritative and lands before its completion
    /// journal entry, so a crash in between leaves a completion no cursor would
    /// ever surface. The mutation path repairs that on its replay branch — but
    /// only if it is re-entered, and this route exists precisely for the caller
    /// who no longer may. Returning the receipt and journalling nothing would
    /// make the hole permanent, which is the one failure a receipt projector
    /// may not have.
    ///
    /// The append does not weaken the boundary this route guards: it repairs
    /// the index of a decision every check below proves already durable, and
    /// mints neither a receipt nor a register row.
    /// [`EvidenceCompletionJournal::record_completion`] is idempotent by
    /// `(family, decision id)`, so the ordinary already-journalled case
    /// appends nothing.
    pub fn recover_decision_receipt(
        &self,
        scope: &CommitmentScope,
        decision_id: &str,
        request: CommitmentDecisionReceiptRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<Option<CommitmentDecisionReceipt>> {
        validate_decision_id(decision_id)?;
        let candidate = self.decision_index_for_request(scope, decision_id, request)?;
        let Some(index) = self.read_decision_index(scope, decision_id)? else {
            // Compatibility for receipt-bearing rows created before the
            // scope-wide locator existed. This remains a one-shard exact read.
            let Some(receipt) =
                self.receipt_for_decision(scope, &candidate.audience, decision_id)?
            else {
                return Ok(None);
            };
            validate_receipt_against_index(&receipt, &candidate)?;
            let receipt = enrich_receipt_from_index(receipt, &candidate);
            self.journal_completion(scope, &candidate.audience, &receipt, now)?;
            return Ok(Some(replay_receipt(receipt)));
        };
        validate_decision_index_match(&index, &candidate)?;
        let authoritative = self.receipt_for_decision(scope, &index.audience, decision_id)?;
        match (index.receipt.as_ref(), authoritative) {
            (None, None) => Ok(None),
            (Some(_), None) => {
                anyhow::bail!("commitment decision index points at a missing authoritative receipt")
            },
            (indexed, Some(receipt)) => {
                validate_receipt_against_index(&receipt, &index)?;
                let receipt = enrich_receipt_from_index(receipt, &index);
                if let Some(indexed) = indexed {
                    validate_receipt_against_index(indexed, &index)?;
                }
                if indexed.is_some_and(|indexed| {
                    enrich_receipt_from_index(indexed.clone(), &index) != receipt
                }) {
                    anyhow::bail!(
                        "commitment decision index receipt differs from its authoritative record"
                    );
                }
                // Only once the receipt is proved durable and proved to be the
                // one this request describes. Journalling before that would
                // announce a completion that did not happen, which is worse
                // than the hole being repaired.
                self.journal_completion(scope, &index.audience, &receipt, now)?;
                Ok(Some(replay_receipt(receipt)))
            },
        }
    }

    /// The exact stored receipt one completion-journal entry addresses.
    ///
    /// [`Self::recover_decision_receipt`] cannot serve a projector: it demands
    /// the whole original request — the confirmer's name, the expected revision
    /// — and a cursor carries a completion rather than the command that caused
    /// it. So this read binds what a journal entry *does* know: the audience
    /// shard the receipt was written under and the commitment it has to sit on.
    /// The audience is required for the same reason the journal entry carries
    /// it; without the shard, an exact read becomes a fold of every shard in
    /// the scope.
    ///
    /// Two deliberate differences from the recovery path, both required by the
    /// projector rather than convenient for it: it returns the record as
    /// stored, not the replay view whose disposition reads `already_applied`,
    /// and it does not journal — a consumer that appended an entry for every
    /// entry it drained would extend the log it is draining, forever.
    pub fn journalled_decision_receipt(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        commitment_id: &str,
        decision_id: &str,
    ) -> Result<Option<CommitmentDecisionReceipt>> {
        validate_decision_id(decision_id)?;
        if !audience.is_named() {
            anyhow::bail!(
                "a commitment receipt read must name the relationship its shard is keyed by"
            );
        }
        let Some(receipt) = self.receipt_for_decision(scope, audience, decision_id)? else {
            return Ok(None);
        };
        if receipt.commitment_id != commitment_id {
            anyhow::bail!(
                "commitment decision `{decision_id}` is recorded against commitment `{}`, not \
                 `{commitment_id}`",
                receipt.commitment_id
            );
        }
        Ok(Some(receipt))
    }

    fn decision_index_for_request(
        &self,
        scope: &CommitmentScope,
        decision_id: &str,
        request: CommitmentDecisionReceiptRequest<'_>,
    ) -> Result<CommitmentDecisionIndex> {
        let (audience, commitment_id, verb, expected_revision, request_fingerprint, by) =
            match request {
                CommitmentDecisionReceiptRequest::Record {
                    request,
                    expected_claim_revision,
                } => {
                    if expected_claim_revision == 0 {
                        anyhow::bail!("expected claim revision must be positive");
                    }
                    validate_record_request(request)?;
                    let commitment_id = derive_commitment_id(scope, request);
                    let fingerprint = record_request_fingerprint(
                        &commitment_id,
                        request,
                        expected_claim_revision,
                    );
                    (
                        request.audience.clone(),
                        commitment_id,
                        CommitmentDecisionVerb::Record,
                        expected_claim_revision,
                        fingerprint,
                        None,
                    )
                },
                CommitmentDecisionReceiptRequest::Confirm {
                    audience,
                    commitment_id,
                    expected_revision,
                    by,
                } => {
                    if !audience.is_named() || !is_safe_scope_id(commitment_id) {
                        anyhow::bail!("commitment receipt target is invalid");
                    }
                    if expected_revision == 0 || by.trim().is_empty() {
                        anyhow::bail!("commitment receipt confirmation identity is invalid");
                    }
                    (
                        audience.clone(),
                        commitment_id.to_owned(),
                        CommitmentDecisionVerb::Confirm,
                        expected_revision,
                        confirm_request_fingerprint(audience, commitment_id, expected_revision, by),
                        Some(by.to_owned()),
                    )
                },
            };
        Ok(CommitmentDecisionIndex {
            schema_version: COMMITMENT_DECISION_INDEX_SCHEMA,
            decision_id: decision_id.to_owned(),
            audience,
            commitment_id,
            verb,
            expected_revision,
            request_fingerprint,
            by,
            receipt: None,
        })
    }

    fn recover_indexed_receipt(
        &self,
        scope: &CommitmentScope,
        index: &mut CommitmentDecisionIndex,
    ) -> Result<Option<CommitmentDecisionReceipt>> {
        let authoritative =
            self.receipt_for_decision(scope, &index.audience, &index.decision_id)?;
        let Some(receipt) = authoritative else {
            if index.receipt.is_some() {
                anyhow::bail!(
                    "commitment decision index points at a missing authoritative receipt"
                );
            }
            return Ok(None);
        };
        validate_receipt_against_index(&receipt, index)?;
        let receipt = enrich_receipt_from_index(receipt, index);
        if let Some(indexed) = index.receipt.as_ref() {
            validate_receipt_against_index(indexed, index)?;
            if enrich_receipt_from_index(indexed.clone(), index) != receipt {
                anyhow::bail!(
                    "commitment decision index receipt differs from its authoritative record"
                );
            }
        }
        if index.receipt.is_some() {
            return Ok(Some(receipt));
        }
        index.receipt = Some(receipt.clone());
        self.write_decision_index(scope, index)?;
        Ok(Some(receipt))
    }

    fn read_decision_index(
        &self,
        scope: &CommitmentScope,
        decision_id: &str,
    ) -> Result<Option<CommitmentDecisionIndex>> {
        let path = self.decision_index_path(scope, decision_id);
        let Some(metadata) = self.workspace_layout.metadata_path_sync(&path)? else {
            return Ok(None);
        };
        if metadata.len() > MAX_COMMITMENT_DECISION_INDEX_BYTES {
            anyhow::bail!("commitment decision index exceeds its bounded size ceiling");
        }
        let index: CommitmentDecisionIndex = self.workspace_layout.read_json_path_sync(&path)?;
        if index.schema_version != COMMITMENT_DECISION_INDEX_SCHEMA
            || index.decision_id != decision_id
        {
            anyhow::bail!("commitment decision index identity is corrupt");
        }
        Ok(Some(index))
    }

    fn write_decision_index(
        &self,
        scope: &CommitmentScope,
        index: &CommitmentDecisionIndex,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(index)?;
        if bytes.len() as u64 > MAX_COMMITMENT_DECISION_INDEX_BYTES {
            anyhow::bail!("commitment decision index exceeds its bounded size ceiling");
        }
        self.workspace_layout
            .write_atomic_path_sync(self.decision_index_path(scope, &index.decision_id), &bytes)
            .context("persisting commitment decision index")?;
        Ok(())
    }

    fn append(&self, path: &PathBuf, record: &CommitmentRecord) -> Result<()> {
        let lock_root = path
            .parent()
            .context("commitment register lost its parent")?;
        let append_lock_id = format!("log-{}", stable_id(&path.to_string_lossy()));
        let _append_guard =
            acquire_record_decision_lock(lock_root, &append_lock_id, "commitment log")?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" fails open —
        // guards pass vacuously and removals report success while removing
        // nothing. Shared semantics live in `magician_v2::jsonl`.
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }

    fn receipt_for_decision(
        &self,
        scope: &CommitmentScope,
        audience: &AudienceRef,
        decision_id: &str,
    ) -> Result<Option<CommitmentDecisionReceipt>> {
        let path = self.path(scope, audience);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(None);
        };
        for record in crate::magician_v2::jsonl::parse_log_lines::<CommitmentRecord>(&raw, &path)? {
            let receipt = match record {
                CommitmentRecord::RecordedWithReceipt { receipt, .. }
                | CommitmentRecord::ConfirmedWithReceipt { receipt, .. } => Some(receipt),
                CommitmentRecord::Recorded(_) | CommitmentRecord::Confirmed { .. } => None,
                CommitmentRecord::Superseded { .. } | CommitmentRecord::Withdrawn { .. } => None,
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
}

fn validate_record_request(request: &RecordCommitment) -> Result<()> {
    if !request.audience.is_named() {
        anyhow::bail!("a commitment must name the relationship it appeared in");
    }
    if request.terms.trim().is_empty() {
        anyhow::bail!("a commitment with no terms is nothing to confirm");
    }
    if request.source_ref.trim().is_empty() {
        anyhow::bail!("a commitment must name its source");
    }
    Ok(())
}

fn validate_decision_id(decision_id: &str) -> Result<()> {
    if decision_id.trim().is_empty() || decision_id.len() > 192 || !is_safe_scope_id(decision_id) {
        anyhow::bail!("decision id must be a non-empty safe identifier no longer than 192 bytes");
    }
    Ok(())
}

fn validate_decision_index_match(
    held: &CommitmentDecisionIndex,
    wanted: &CommitmentDecisionIndex,
) -> Result<()> {
    if held.schema_version != COMMITMENT_DECISION_INDEX_SCHEMA
        || held.decision_id != wanted.decision_id
        || held.audience != wanted.audience
        || held.commitment_id != wanted.commitment_id
        || held.verb != wanted.verb
        || held.expected_revision != wanted.expected_revision
        || held.request_fingerprint != wanted.request_fingerprint
        || held.by != wanted.by
    {
        anyhow::bail!("decision id replay substituted its commitment request");
    }
    Ok(())
}

fn validate_receipt_against_index(
    receipt: &CommitmentDecisionReceipt,
    index: &CommitmentDecisionIndex,
) -> Result<()> {
    let explicit_audience_matches = receipt
        .audience
        .as_ref()
        .is_none_or(|audience| audience == &index.audience);
    let explicit_actor_matches = receipt
        .by
        .as_ref()
        .is_none_or(|by| index.by.as_ref().is_some_and(|expected| expected == by));
    let verb_name = match receipt.verb {
        CommitmentDecisionVerb::Record => "record",
        CommitmentDecisionVerb::Confirm => "confirm",
    };
    let expected_receipt_id = format!(
        "commitment-receipt-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{verb_name}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            receipt.decision_id,
            receipt.commitment_id,
            receipt.expected_revision,
            receipt.resulting_revision,
            receipt.request_fingerprint,
        ))
    );
    if receipt.decision_id != index.decision_id
        || receipt.commitment_id != index.commitment_id
        || !explicit_audience_matches
        || receipt.verb != index.verb
        || receipt.expected_revision == 0
        || receipt.expected_revision != index.expected_revision
        || receipt.request_fingerprint != index.request_fingerprint
        || !explicit_actor_matches
        || receipt.disposition != CommitmentDecisionDisposition::Applied
        || receipt.receipt_id != expected_receipt_id
    {
        anyhow::bail!("commitment decision receipt does not match its scope-wide index");
    }
    match receipt.verb {
        CommitmentDecisionVerb::Record if receipt.resulting_revision != initial_revision() => {
            anyhow::bail!("commitment record receipt has an invalid resulting revision");
        },
        CommitmentDecisionVerb::Confirm
            if receipt.resulting_revision
                != receipt
                    .expected_revision
                    .checked_add(1)
                    .context("commitment receipt revision overflow")? =>
        {
            anyhow::bail!("commitment confirmation receipt has an invalid resulting revision");
        },
        _ => {},
    }
    Ok(())
}

fn enrich_receipt_from_index(
    mut receipt: CommitmentDecisionReceipt,
    index: &CommitmentDecisionIndex,
) -> CommitmentDecisionReceipt {
    if receipt.audience.is_none() {
        receipt.audience = Some(index.audience.clone());
    }
    if receipt.by.is_none() {
        receipt.by = index.by.clone();
    }
    receipt
}

fn validate_record_receipt(
    commitment: &Commitment,
    receipt: &CommitmentDecisionReceipt,
) -> Result<()> {
    let request = RecordCommitment {
        audience: commitment.audience.clone(),
        source_ref: commitment.source_ref.clone(),
        direction: commitment.direction,
        terms: commitment.terms.clone(),
        stated_at: commitment.stated_at,
    };
    let expected_fingerprint = record_request_fingerprint(
        &commitment.commitment_id,
        &request,
        receipt.expected_revision,
    );
    let index = CommitmentDecisionIndex {
        schema_version: COMMITMENT_DECISION_INDEX_SCHEMA,
        decision_id: receipt.decision_id.clone(),
        audience: commitment.audience.clone(),
        commitment_id: commitment.commitment_id.clone(),
        verb: CommitmentDecisionVerb::Record,
        expected_revision: receipt.expected_revision,
        request_fingerprint: expected_fingerprint,
        by: None,
        receipt: None,
    };
    validate_receipt_against_index(receipt, &index)?;
    if commitment.status != CommitmentStatus::Unconfirmed
        || commitment.revision != initial_revision()
    {
        anyhow::bail!("receipted commitment record contains a non-initial state");
    }
    Ok(())
}

fn validate_confirm_receipt(
    held: &Commitment,
    commitment_id: &str,
    by: &str,
    receipt: &CommitmentDecisionReceipt,
) -> Result<()> {
    let index = CommitmentDecisionIndex {
        schema_version: COMMITMENT_DECISION_INDEX_SCHEMA,
        decision_id: receipt.decision_id.clone(),
        audience: held.audience.clone(),
        commitment_id: commitment_id.to_owned(),
        verb: CommitmentDecisionVerb::Confirm,
        expected_revision: held.revision,
        request_fingerprint: confirm_request_fingerprint(
            &held.audience,
            commitment_id,
            held.revision,
            by,
        ),
        by: Some(by.to_owned()),
        receipt: None,
    };
    validate_receipt_against_index(receipt, &index)?;
    if held.status != CommitmentStatus::Unconfirmed {
        anyhow::bail!("commitment confirmation receipt does not transition an unconfirmed head");
    }
    Ok(())
}

fn commitment_receipt(
    decision_id: &str,
    commitment_id: &str,
    verb: CommitmentDecisionVerb,
    expected_revision: u64,
    resulting_revision: u64,
    audience: &AudienceRef,
    by: Option<&str>,
    request_fingerprint: String,
    recorded_at: DateTime<Utc>,
) -> CommitmentDecisionReceipt {
    let material = format!(
        "{decision_id}{FIELD_SEP}{commitment_id}{FIELD_SEP}{}{FIELD_SEP}{expected_revision}{FIELD_SEP}{resulting_revision}{FIELD_SEP}{request_fingerprint}",
        match verb {
            CommitmentDecisionVerb::Record => "record",
            CommitmentDecisionVerb::Confirm => "confirm",
        }
    );
    CommitmentDecisionReceipt {
        receipt_id: format!("commitment-receipt-{}", stable_id(&material)),
        decision_id: decision_id.to_owned(),
        commitment_id: commitment_id.to_owned(),
        audience: Some(audience.clone()),
        verb,
        expected_revision,
        resulting_revision,
        request_fingerprint,
        by: by.map(ToOwned::to_owned),
        disposition: CommitmentDecisionDisposition::Applied,
        recorded_at,
    }
}

fn record_request_fingerprint(
    commitment_id: &str,
    request: &RecordCommitment,
    expected_claim_revision: u64,
) -> String {
    stable_id(&format!(
        "record{FIELD_SEP}{commitment_id}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{expected_claim_revision}",
        request.audience.as_key(),
        request.source_ref,
        request.direction.as_str(),
        request.terms,
        request.stated_at.to_rfc3339(),
    ))
}

fn confirm_request_fingerprint(
    audience: &AudienceRef,
    commitment_id: &str,
    expected_revision: u64,
    by: &str,
) -> String {
    stable_id(&format!(
        "confirm{FIELD_SEP}{}{FIELD_SEP}{commitment_id}{FIELD_SEP}{expected_revision}{FIELD_SEP}{by}",
        audience.as_key(),
    ))
}

fn replay_receipt(mut receipt: CommitmentDecisionReceipt) -> CommitmentDecisionReceipt {
    receipt.disposition = CommitmentDecisionDisposition::AlreadyApplied;
    receipt
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// Derived from `(audience, source, direction, terms)`.
///
/// The **source** is in the key deliberately: the same words said twice in two
/// different messages are two commitments, because which message it came from is
/// what the owner reads before confirming. Terms are normalised for whitespace
/// and case, since re-extraction rarely returns them character-identical.
fn derive_commitment_id(scope: &CommitmentScope, request: &RecordCommitment) -> String {
    let terms = request
        .terms
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "cmt-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            request.audience.as_key(),
            request.source_ref,
            format!(
                "{}{FIELD_SEP}{}",
                request.direction.as_str(),
                terms.to_ascii_lowercase()
            ),
        ))
    )
}
