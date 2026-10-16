//! Projection of completed evidence decisions into `review_receipt` rows.
//!
//! Gate E1 of the apps-platform register
//! (`docs/plans/2026-09-03-apps_platform_open-gates.md`), and the back half of
//! the evidence increment's blocking reopen: *"the package decision actions
//! create reviewable source records; they do not forge trusted-desktop
//! signatures or mark themselves applied."*
//!
//! # The split this preserves
//!
//! The claims-review package writes a `review_decision` row and stops. That row
//! stays **immutable at `recorded`** for its whole life — nothing here writes
//! back to it. Outcome, canonical decision id, and actor live only in the
//! `review_receipt` row this module projects, and that separation is load
//! bearing: the signed envelope names an exact `review_decision` revision and
//! payload digest, so stamping the outcome onto the source head would change
//! the digest the envelope was signed over and make an exact retry — the one
//! thing the destination guarantees — impossible after the first projection.
//!
//! # Why the journal, and not the decision indexes
//!
//! [`super::completion_journal`] exists for this consumer. A hash-ordered index
//! directory is not a cursor: an index may be prepared before its receipt, and
//! a decision taken tomorrow may hash behind today's. Both faults lose rows
//! silently, which is the one failure a receipt projector may not have. The
//! journal assigns its order at completion, so "consumed through seq N" is
//! lossless and this module never enumerates a register.
//!
//! # One row per `(family, decision id)`
//!
//! The ledger's key is the journal's key, exactly. A decision id is
//! client-supplied and each register namespaces its own, so the same id in both
//! registers is two decisions — ordinary rather than exotic, and something
//! [`super::completion_journal`] deliberately admits as two entries. Keyed on
//! the bare id those two legal completions would address one row, the second
//! would be refused as a disagreement with the first, and — because the cursor
//! only advances after a whole page persists — the drain would stop at that seq
//! for the life of the scope. Both halves of the key travel together everywhere,
//! including into [`ReviewReceiptProjector::receipt`].
//!
//! # It reads the stored receipt, never the replay view
//!
//! `recover_*` returns a receipt with its disposition rewritten to
//! `already_applied`, because that is what a *replaying caller* is being told.
//! A projection re-reading the same completion has to produce the same row, so
//! it reads what the register actually stored. Those recovery entry points also
//! journal, and a projector that journalled every entry it drained would append
//! to the log it is draining and never terminate.
//!
//! # What it can and cannot say
//!
//! Only receipted transitions are journalled, so every row this mints says
//! `applied`. A refusal mints no receipt and no journal entry, and a replay
//! earns no second row — the row already there is the answer. `idempotent` and
//! `refused` are restated in the outcome vocabulary anyway, value for value
//! with the entity, because a projector that quietly narrowed the enum would be
//! the place the two definitions drift apart.
//!
//! # Two cursors, because minting a row is not publishing it
//!
//! A row on disk is a host artifact; the console reads the package's
//! `review_receipt` entity, and no workflow's `may_mutate` names that entity —
//! the package cannot write its own receipts, so a host publisher has to. That
//! publisher is a *second* consumer of the same journal order, and it needs its
//! own position: the projection cursor moves as soon as a row is durable here,
//! and a publication that borrowed it would call rows published that never
//! reached the store. So `pending_publications` walks the journal from its own
//! cursor and stops at the first completion whose row has not been minted yet,
//! and `record_published` refuses to rewind or to pass the projection it
//! publishes.
//!
//! # One publication cursor per destination store, never one per scope
//!
//! The projection is scope-wide because the registers are; a *publication* is
//! not. It lands in one installation's entity store, and an installation is a
//! store with its own lifetime — a reinstall mints a new id over an empty one.
//! A cursor keyed by scope alone therefore said "published" about a store it
//! could not name: the first installation to publish consumed the whole
//! scope's backlog, and because the cursor never rewinds, every other
//! installation in the scope — including the one a reinstall just created —
//! was permanently told those rows were already delivered to it. So the
//! publication half of this ledger is keyed by
//! [`ReviewReceiptPublisher`], and each destination catches up over the one
//! journal from its own position. The projection cursor stays scope-wide,
//! because a row on disk really is minted once for the scope.
//!
//! A ledger written under the old key therefore starts every destination at
//! zero, and that is the safe direction: a republished row collides with the
//! named record already in the store and the publisher treats the collision as
//! success, so the catch-up costs writes rather than correctness.
//!
//! # Fail closed means stall, loudly
//!
//! A journal entry is a promise that an authoritative receipt is durable at
//! that exact address. If the receipt is missing, or does not carry the receipt
//! id and request fingerprint the entry described, the drain stops **at** that
//! entry with an error and the cursor does not pass it. Skipping would drop a
//! completed decision from the record forever; stopping is visible and every
//! later drain retries it.
//!
//! The cursor does advance *up to* it. Every row before the failure is already
//! durable, so keeping the position is exactly as lossless as the page-at-a-
//! time write — and dropping it made one permanently unbindable entry
//! re-project, and re-fold both registers for, every entry ahead of it on every
//! later decision in the scope. A stall is meant to be a stall, not a tax.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::commitments::{
    CommitmentDecisionDisposition, CommitmentScope, Commitments,
};
use crate::magician_v2::execution::file_edit::transaction::acquire_record_decision_lock;

use super::completion_journal::{
    EvidenceCompletionCursor, EvidenceCompletionEntry, EvidenceCompletionJournal,
    EvidenceDecisionScope, EvidenceDecisionTarget,
};
use super::outward_assertions::OutwardScope;
use super::transcript_ingestion::{
    ClaimDecisionDisposition, ClaimDecisionReceipt, TranscriptIngestion,
};

const REVIEW_RECEIPT_PROJECTION_SCHEMA: u32 = 1;

/// Ceiling on one drain page. Above this a caller is asking for an unbounded
/// read, which is refused rather than clamped, for the same reason the journal
/// refuses it: a silently shortened page looks exactly like the end of the log.
pub const MAX_REVIEW_RECEIPT_PAGE: usize = 128;

/// Pages one [`ReviewReceiptProjector::drain_all`] will consume before handing
/// control back. The drain runs on the tail of a destination call, so a scope
/// with a long unprojected backlog must not hold that call open until it is
/// finished; whatever is left is picked up by the next decision or the next
/// sweep, and the cursor makes resuming free.
const MAX_DRAIN_PAGES: usize = 32;

/// Bounded size of one persisted row or cursor document.
const MAX_REVIEW_RECEIPT_BYTES: u64 = 8 * 1024;

/// Ceiling on one publication page, for the same reason the drain has one: a
/// caller asking for more is asking for an unbounded read, and a silently
/// shortened page is indistinguishable from the end of the ledger.
pub const MAX_REVIEW_RECEIPT_PUBLICATION_PAGE: usize = 64;

/// The package entity a projected row is published into.
///
/// Restated here rather than read from the manifest, exactly as the two
/// vocabularies above are: this module owns what a `review_receipt` says, so it
/// owns where the row goes, and it must be able to say so without loading the
/// package that will read it.
pub const REVIEW_RECEIPT_ENTITY: &str = "review_receipt";

/// What a projected row names as the actor when the register recorded none.
///
/// `record_commitment` is the one verb whose canonical receipt carries no `by`:
/// it moves a claim somebody already confirmed into the commitment register,
/// and the register refuses to name a second decider for a step no second
/// person took. The entity's `actor_ref` is non-nullable, so the row has to say
/// something — and the one thing it may not say is a person's name it does not
/// have. Borrowing the envelope's signer here would put a name on a mechanical
/// step, which is precisely the forgery this increment's reopen is about.
pub const REVIEW_RECEIPT_UNNAMED_ACTOR: &str = "register:mechanical-record";

/// The closed target vocabulary of the package's `review_receipt` entity.
///
/// Value for value with the entity, and restated here rather than derived from
/// the manifest for the same reason the meetings destination restates its own:
/// the projector must be able to say what happened without loading the package
/// that will read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReceiptTargetKind {
    Claim,
    Commitment,
    /// Staged manual-attribution ingest. Declared by the entity and never
    /// minted here: ingest completions are E3's consumer, and the claims
    /// journal carries no ingest family to mint one from.
    Ingest,
}

impl ReviewReceiptTargetKind {
    /// The directory a row of this kind is filed under, and the first half of a
    /// row's key.
    ///
    /// Deliberately the same strings serde writes into the row's own
    /// `target_kind`, so an address and the content at it read alike on disk —
    /// the arrangement the journal already uses for its per-family locators.
    /// The vocabulary is closed, so nothing a caller supplies reaches the path.
    fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Commitment => "commitment",
            Self::Ingest => "ingest",
        }
    }

    /// The kind a completion of this target projects into — the journal's
    /// family in the entity's vocabulary.
    ///
    /// The single mapping between the two, so the kind a row is *filed under*
    /// and the kind a later reader *looks under* cannot drift apart. `Ingest`
    /// has no family here because the claims journal carries none to map from.
    fn of(target: &EvidenceDecisionTarget) -> Self {
        match target {
            EvidenceDecisionTarget::TranscriptClaim { .. } => Self::Claim,
            EvidenceDecisionTarget::Commitment { .. } => Self::Commitment,
        }
    }
}

/// The closed outcome vocabulary of the package's `review_receipt` entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewReceiptOutcome {
    Applied,
    Idempotent,
    /// A refusal never reaches a register, so it mints no receipt and no
    /// journal entry. Nothing in a journal-driven projection can produce this.
    Refused,
}

/// One projected `review_receipt` row.
///
/// The field set is exactly the claims-review package's `review_receipt`
/// entity. Every value is copied from an authoritative destination receipt or a
/// closed enum, so the row carries no claim text, no transcript, and nothing
/// the register did not already decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedReviewReceipt {
    pub receipt_id: String,
    pub decision_id: String,
    pub target_kind: ReviewReceiptTargetKind,
    pub target_id: String,
    pub outcome: ReviewReceiptOutcome,
    pub actor_ref: String,
    pub destination_revision: u64,
    pub applied_at: DateTime<Utc>,
    /// Nullable in the entity and always absent here: only a refusal carries
    /// one, and a refusal is never journalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl ProjectedReviewReceipt {
    /// The app-store record id this row occupies.
    ///
    /// The same `(family, decision id)` key that addresses the row on disk, so
    /// a publisher and the ledger can never disagree about which decision a row
    /// belongs to — and, for the reason the module note gives, so the one id
    /// both registers may legitimately hold does not name a single store
    /// record. It is derived rather than borrowed because a store record id is
    /// an opaque ASCII token and neither `receipt_id` nor `decision_id` is
    /// constrained to that alphabet — a decision id may legitimately carry a
    /// colon, which a record id may not. Naming the row instead of letting the
    /// store mint one is what makes a second publication of one decision a
    /// refused collision rather than a second row describing the same decision.
    pub fn package_record_id(&self) -> String {
        format!(
            "receipt-{}-{}",
            self.target_kind.as_str(),
            stable_id(&self.decision_id)
        )
    }
}

/// What one drain call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReviewReceiptDrain {
    /// Rows this call created.
    pub minted: usize,
    /// Journal entries whose row was already durable and identical — a healed
    /// journal entry replays its completion at a fresh seq, so this is the
    /// ordinary case rather than an anomaly.
    pub already_projected: usize,
    pub cursor: EvidenceCompletionCursor,
    pub has_more: bool,
}

/// One projected row awaiting publication, with the journal position it holds.
///
/// The position travels with the row so a publisher that stops halfway records
/// exactly what it published, rather than the page it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReviewReceipt {
    pub row: ProjectedReviewReceipt,
    /// The cursor to record once THIS row is durable in the package.
    pub cursor_after: EvidenceCompletionCursor,
}

/// A bounded page of rows awaiting publication, in journal order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewReceiptPublication {
    pub pending: Vec<PendingReviewReceipt>,
    /// Whether rows remain beyond this page — including a page cut short
    /// because the projection has not reached the next completion yet.
    pub has_more: bool,
}

/// The destination store one publication cursor tracks: the app installation
/// the rows are written into.
///
/// A publication cursor is a claim that rows reached A STORE, and every
/// installation is a different store with its own lifetime. Naming that store
/// is what makes the claim checkable, and the type exists so the name cannot be
/// omitted at a call site — the scope-keyed cursor it replaces was omitting it
/// silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReceiptPublisher {
    installation_id: String,
}

impl ReviewReceiptPublisher {
    /// Refuse an unnamed destination rather than fall back to a shared cursor.
    ///
    /// An empty id is not a store, and the one address every unnamed publisher
    /// would share is exactly the defect this type removes: rows marked
    /// delivered to a destination nobody can identify are rows no destination
    /// is ever offered again.
    pub fn installation(installation_id: &str) -> Result<Self> {
        if installation_id.trim().is_empty() {
            anyhow::bail!(
                "a review receipt publisher must name the installation it publishes into"
            );
        }
        Ok(Self {
            installation_id: installation_id.to_owned(),
        })
    }

    /// The cursor file this destination owns.
    ///
    /// Hashed for the reason a row's address is: an installation id reaches
    /// here as text, and no character of it may influence a path.
    fn cursor_file(&self) -> String {
        format!("{}.json", stable_id(&self.installation_id))
    }

    /// The advisory lock a publish to THIS destination takes.
    ///
    /// Per destination, not per scope: the read-compare-write it guards is over
    /// one cursor file, and a publish into one installation has no reason to
    /// wait behind a publish into another.
    fn lock_id(&self) -> String {
        format!("publication-{}", stable_id(&self.installation_id))
    }
}

/// Durable cursor document. Its schema version governs the whole ledger; the
/// rows themselves stay exactly the entity's field set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewReceiptCursorFile {
    schema_version: u32,
    consumed_through: u64,
}

/// The scope's `review_receipt` ledger and its journal cursor.
#[derive(Debug, Clone)]
pub struct ReviewReceiptProjector {
    workspace_layout: ArtifactV2Workspace,
}

impl ReviewReceiptProjector {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// The projector over the same workspace root as a claims register, so a
    /// destination holding one register can reach it without being handed a
    /// second copy of the layout to disagree with.
    pub fn over(ingestion: &TranscriptIngestion) -> Self {
        Self::new(ingestion.workspace_layout().clone())
    }

    fn root(&self, scope: &EvidenceDecisionScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("review_receipts")
    }

    fn row_path(
        &self,
        scope: &EvidenceDecisionScope,
        target_kind: ReviewReceiptTargetKind,
        decision_id: &str,
    ) -> PathBuf {
        // Keyed by `(family, decision)` — the journal's own idempotency key —
        // so a decision has exactly one row *in its register* and the two
        // registers' independently-namespaced ids cannot collide on one path.
        // The id is hashed, so its characters cannot influence the address.
        self.root(scope)
            .join("rows")
            .join(target_kind.as_str())
            .join(format!("{}.json", stable_id(decision_id)))
    }

    fn cursor_path(&self, scope: &EvidenceDecisionScope) -> PathBuf {
        self.root(scope).join("cursor.json")
    }

    fn publication_cursor_path(
        &self,
        scope: &EvidenceDecisionScope,
        publisher: &ReviewReceiptPublisher,
    ) -> PathBuf {
        // Under a directory rather than beside the projection cursor, so the
        // per-destination cursors are visibly a set and a scope-wide one cannot
        // be reintroduced next to them by accident.
        self.root(scope)
            .join("published")
            .join(publisher.cursor_file())
    }

    /// How far this scope's projection has consumed the completion journal.
    pub fn cursor(&self, scope: &EvidenceDecisionScope) -> Result<EvidenceCompletionCursor> {
        self.read_cursor(scope)
    }

    /// The projected row for one decision of one register, when it has been
    /// projected.
    ///
    /// The target kind is not a filter, it is half the key: the registers
    /// namespace decision ids independently, so an id on its own names a row in
    /// neither of them or in both.
    pub fn receipt(
        &self,
        scope: &EvidenceDecisionScope,
        target_kind: ReviewReceiptTargetKind,
        decision_id: &str,
    ) -> Result<Option<ProjectedReviewReceipt>> {
        self.read_row(&self.row_path(scope, target_kind, decision_id))
    }

    /// Project one bounded page of completions, then advance the cursor.
    ///
    /// The cursor is written **after** the page's rows are durable, not per
    /// row. A crash mid-page therefore re-projects the rows it already minted,
    /// and that is free: every field of a row derives from the stored receipt,
    /// so a re-projection is byte-identical and mints nothing.
    pub fn drain(
        &self,
        scope: &EvidenceDecisionScope,
        ingestion: &TranscriptIngestion,
        commitments: &Commitments,
        limit: usize,
    ) -> Result<ReviewReceiptDrain> {
        if limit == 0 || limit > MAX_REVIEW_RECEIPT_PAGE {
            anyhow::bail!(
                "review receipt drain size must be between 1 and {MAX_REVIEW_RECEIPT_PAGE}"
            );
        }
        let root = self.root(scope);
        // The only lock this projector takes, and it takes nothing else while
        // holding it: the journal page and both register reads below are
        // lock-free folds. That keeps this a leaf, so a drain running on the
        // tail of a destination call can never close a cycle with the register
        // locks that call has already released.
        let _guard =
            acquire_record_decision_lock(&root, "projection", "review receipt projection")?;

        let cursor = self.read_cursor(scope)?;
        let page = EvidenceCompletionJournal::new(self.workspace_layout.clone())
            .page_after(scope, cursor, limit)?;
        let outward = OutwardScope::new(scope.principal.clone(), scope.workspace.clone());
        // Resolved AFTER the page is cut, and that order is load bearing: a
        // register writes its receipt before it journals the completion, so a
        // log read that follows the page holds a receipt for every entry in it.
        // Resolved first, it could miss the newest entry and stall the drain on
        // a receipt that is in fact durable.
        let claim_receipts = PageClaimReceipts::for_page(ingestion, &outward, &page.entries)?;
        let mut drained = ReviewReceiptDrain {
            cursor,
            has_more: page.has_more,
            ..ReviewReceiptDrain::default()
        };
        // The prefix that has actually landed, tracked per entry rather than
        // per page, so a stall can keep it. See `stall_at`.
        let mut consumed = cursor;
        for entry in &page.entries {
            match self
                .project(
                    scope,
                    &outward,
                    &claim_receipts,
                    ingestion,
                    commitments,
                    entry,
                )
                .and_then(|projected| self.persist(scope, &projected))
            {
                Ok(true) => drained.minted += 1,
                Ok(false) => drained.already_projected += 1,
                Err(error) => return Err(self.stall_at(scope, cursor, consumed, error)),
            }
            consumed = EvidenceCompletionCursor::consumed_through(entry.seq);
        }
        if !page.entries.is_empty() {
            self.write_cursor(scope, page.next_cursor)?;
            drained.cursor = page.next_cursor;
        }
        Ok(drained)
    }

    /// Keep the progress a stalled page made, then report the stall.
    ///
    /// `persist` had already returned for every entry through `consumed`, so
    /// those rows are durable — which is exactly what the cursor claims, and it
    /// stays true of the prefix whatever the next entry does. Discarding it
    /// made one unbindable entry re-project every entry ahead of it, each a
    /// fold of its register, on every later decision in the scope, for as long
    /// as the entry stayed unbindable. The drain stalls by design; this is what
    /// keeps the stall from also being a per-decision tax.
    ///
    /// The cursor still never passes the failing entry, so nothing is skipped.
    fn stall_at(
        &self,
        scope: &EvidenceDecisionScope,
        held: EvidenceCompletionCursor,
        consumed: EvidenceCompletionCursor,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if consumed <= held {
            return error;
        }
        match self.write_cursor(scope, consumed) {
            Ok(()) => error,
            // The stall is the answer either way — it recurs on the next call —
            // so the store fault rides along rather than replacing it.
            Err(cursor_error) => error.context(format!(
                "and the projection cursor could not be advanced over the entries that did \
                 project: {cursor_error:#}"
            )),
        }
    }

    /// Drain until the journal is caught up or the page budget is spent.
    pub fn drain_all(
        &self,
        scope: &EvidenceDecisionScope,
        ingestion: &TranscriptIngestion,
        commitments: &Commitments,
    ) -> Result<ReviewReceiptDrain> {
        let mut total = ReviewReceiptDrain::default();
        for _ in 0..MAX_DRAIN_PAGES {
            let page = self.drain(scope, ingestion, commitments, MAX_REVIEW_RECEIPT_PAGE)?;
            total.minted += page.minted;
            total.already_projected += page.already_projected;
            total.cursor = page.cursor;
            total.has_more = page.has_more;
            if !page.has_more {
                break;
            }
        }
        Ok(total)
    }

    /// How far this scope's rows have been published into ONE installation.
    pub fn publication_cursor(
        &self,
        scope: &EvidenceDecisionScope,
        publisher: &ReviewReceiptPublisher,
    ) -> Result<EvidenceCompletionCursor> {
        self.read_cursor_at(&self.publication_cursor_path(scope, publisher))
    }

    /// The projected rows one installation has not been given yet, in journal
    /// order.
    ///
    /// Bounded, resumable, and lossless in the same way the drain is, over the
    /// same order and with its own position. A publisher pages until the page
    /// is empty; each row carries the cursor to record once the store holds it.
    ///
    /// The position is the *publisher's*, so two installations of the same
    /// package each receive the whole scope's ledger and neither consumes the
    /// other's backlog.
    ///
    /// A completion whose row has not been minted yet ends the page. It is not
    /// skipped: the cursor is one position, so passing a hole would mark that
    /// decision published forever, and the projection is minting it anyway —
    /// the next call sees it. `has_more` therefore stays true when a page is
    /// cut short this way, so a caller cannot read the short page as caught up.
    pub fn pending_publications(
        &self,
        scope: &EvidenceDecisionScope,
        publisher: &ReviewReceiptPublisher,
        limit: usize,
    ) -> Result<ReviewReceiptPublication> {
        if limit == 0 || limit > MAX_REVIEW_RECEIPT_PUBLICATION_PAGE {
            anyhow::bail!(
                "review receipt publication page size must be between 1 and \
                 {MAX_REVIEW_RECEIPT_PUBLICATION_PAGE}"
            );
        }
        let cursor = self.publication_cursor(scope, publisher)?;
        let page = EvidenceCompletionJournal::new(self.workspace_layout.clone())
            .page_after(scope, cursor, limit)?;
        let mut pending = Vec::with_capacity(page.entries.len());
        for entry in &page.entries {
            let target_kind = ReviewReceiptTargetKind::of(&entry.target);
            let Some(row) = self.receipt(scope, target_kind, &entry.decision_id)? else {
                return Ok(ReviewReceiptPublication {
                    pending,
                    has_more: true,
                });
            };
            pending.push(PendingReviewReceipt {
                row,
                cursor_after: EvidenceCompletionCursor::consumed_through(entry.seq),
            });
        }
        Ok(ReviewReceiptPublication {
            has_more: page.has_more,
            pending,
        })
    }

    /// Record that every row through `cursor` is durable in ONE installation.
    ///
    /// Refuses to rewind, and refuses to pass the projection it publishes. The
    /// cursor is a claim that those rows reached THIS publisher's store, and a
    /// claim that runs ahead of the truth loses exactly the rows it skipped —
    /// the one failure a receipt ledger may not have. A publisher that got
    /// through three of five rows records the third and stops; the rest are
    /// offered again on the next call. Nothing recorded here says anything
    /// about any other installation's store.
    pub fn record_published(
        &self,
        scope: &EvidenceDecisionScope,
        publisher: &ReviewReceiptPublisher,
        cursor: EvidenceCompletionCursor,
    ) -> Result<()> {
        let root = self.root(scope);
        // Its own lock, not the projection's: a publish must not serialise
        // behind a drain, and taking neither while holding the other keeps both
        // leaves. The projection cursor read below is lock-free, and reading it
        // stale can only read it LOW — the drain writes it after its rows are
        // durable — which refuses this call and retries, never over-advances.
        let _guard = acquire_record_decision_lock(
            &root,
            &publisher.lock_id(),
            "review receipt publication",
        )?;
        let held = self.publication_cursor(scope, publisher)?;
        if cursor < held {
            anyhow::bail!(
                "a review receipt publication cursor may not rewind from {} to {}",
                held.seq(),
                cursor.seq()
            );
        }
        if cursor > self.read_cursor(scope)? {
            anyhow::bail!(
                "a review receipt publication cursor may not pass the projection it publishes"
            );
        }
        self.write_cursor_at(
            self.publication_cursor_path(scope, publisher),
            cursor,
            "persisting the review receipt publication cursor",
        )
    }

    /// Turn one completion into the row the package's receipts view reads.
    fn project(
        &self,
        scope: &EvidenceDecisionScope,
        outward: &OutwardScope,
        claim_receipts: &PageClaimReceipts,
        ingestion: &TranscriptIngestion,
        commitments: &Commitments,
        entry: &EvidenceCompletionEntry,
    ) -> Result<ProjectedReviewReceipt> {
        // One derivation, shared with the address side: a row filed under a
        // kind its reader would not look under is a row nobody ever publishes.
        let target_kind = ReviewReceiptTargetKind::of(&entry.target);
        match &entry.target {
            EvidenceDecisionTarget::TranscriptClaim { claim_id } => {
                let receipt = claim_receipts
                    .receipt(ingestion, outward, claim_id, &entry.decision_id)?
                    .with_context(|| missing_receipt(&entry.decision_id))?;
                bind_entry_to_receipt(
                    entry,
                    &receipt.decision_id,
                    &receipt.receipt_id,
                    &receipt.request_fingerprint,
                )?;
                Ok(ProjectedReviewReceipt {
                    receipt_id: receipt.receipt_id,
                    decision_id: receipt.decision_id,
                    target_kind,
                    target_id: receipt.claim_id,
                    outcome: match receipt.disposition {
                        ClaimDecisionDisposition::Applied => ReviewReceiptOutcome::Applied,
                        ClaimDecisionDisposition::AlreadyApplied => {
                            ReviewReceiptOutcome::Idempotent
                        },
                    },
                    actor_ref: receipt.by,
                    destination_revision: receipt.resulting_revision,
                    applied_at: receipt.recorded_at,
                    error_code: None,
                })
            },
            EvidenceDecisionTarget::Commitment {
                audience,
                commitment_id,
            } => {
                let commitment_scope =
                    CommitmentScope::new(scope.principal.clone(), scope.workspace.clone());
                // Per entry, unlike the claim side, because the cost is not the
                // same shape: a commitment receipt lives in the shard of the
                // one relationship it was made in, so this fold is bounded by
                // that relationship's history rather than by the workspace's.
                // The register also exposes no bulk receipt read to batch it
                // with, and inventing one here would mean a second parser for
                // its log — the one thing a projection may not have.
                let receipt = commitments
                    .journalled_decision_receipt(
                        &commitment_scope,
                        audience,
                        commitment_id,
                        &entry.decision_id,
                    )?
                    .with_context(|| missing_receipt(&entry.decision_id))?;
                bind_entry_to_receipt(
                    entry,
                    &receipt.decision_id,
                    &receipt.receipt_id,
                    &receipt.request_fingerprint,
                )?;
                Ok(ProjectedReviewReceipt {
                    receipt_id: receipt.receipt_id,
                    decision_id: receipt.decision_id,
                    target_kind,
                    target_id: receipt.commitment_id,
                    outcome: match receipt.disposition {
                        CommitmentDecisionDisposition::Applied => ReviewReceiptOutcome::Applied,
                        CommitmentDecisionDisposition::AlreadyApplied => {
                            ReviewReceiptOutcome::Idempotent
                        },
                    },
                    actor_ref: receipt
                        .by
                        .unwrap_or_else(|| REVIEW_RECEIPT_UNNAMED_ACTOR.to_owned()),
                    destination_revision: receipt.resulting_revision,
                    applied_at: receipt.recorded_at,
                    error_code: None,
                })
            },
        }
    }

    /// Write the row for this decision, or leave the identical one already
    /// there. Returns whether THIS call created it.
    ///
    /// A row that disagrees with what the register now says is corruption, not
    /// an update: the receipt it was projected from is immutable, so the two
    /// cannot legitimately differ, and overwriting would erase the evidence of
    /// whichever one is wrong.
    fn persist(
        &self,
        scope: &EvidenceDecisionScope,
        receipt: &ProjectedReviewReceipt,
    ) -> Result<bool> {
        let path = self.row_path(scope, receipt.target_kind, &receipt.decision_id);
        if let Some(existing) = self.read_row(&path)? {
            if &existing != receipt {
                anyhow::bail!(
                    "{} decision `{}` already has a review receipt row that disagrees with its \
                     authoritative record",
                    receipt.target_kind.as_str(),
                    receipt.decision_id
                );
            }
            return Ok(false);
        }
        let bytes = serde_json::to_vec(receipt)?;
        if bytes.len() as u64 > MAX_REVIEW_RECEIPT_BYTES {
            anyhow::bail!("projected review receipt exceeds its bounded size ceiling");
        }
        self.workspace_layout
            .write_atomic_path_sync(&path, &bytes)
            .context("persisting projected review receipt")?;
        Ok(true)
    }

    fn read_row(&self, path: &Path) -> Result<Option<ProjectedReviewReceipt>> {
        let Some(metadata) = self.workspace_layout.metadata_path_sync(path)? else {
            return Ok(None);
        };
        if metadata.len() > MAX_REVIEW_RECEIPT_BYTES {
            anyhow::bail!("projected review receipt exceeds its bounded size ceiling");
        }
        Ok(Some(self.workspace_layout.read_json_path_sync(path)?))
    }

    fn read_cursor(&self, scope: &EvidenceDecisionScope) -> Result<EvidenceCompletionCursor> {
        self.read_cursor_at(&self.cursor_path(scope))
    }

    fn read_cursor_at(&self, path: &Path) -> Result<EvidenceCompletionCursor> {
        let Some(metadata) = self.workspace_layout.metadata_path_sync(path)? else {
            return Ok(EvidenceCompletionCursor::START);
        };
        if metadata.len() > MAX_REVIEW_RECEIPT_BYTES {
            anyhow::bail!("review receipt cursor exceeds its bounded size ceiling");
        }
        let held: ReviewReceiptCursorFile = self.workspace_layout.read_json_path_sync(path)?;
        // Never fall back to the start cursor. Re-projecting is harmless, but a
        // cursor this build cannot read is a cursor whose meaning it does not
        // know, and continuing from a guess is how a consumer skips rows.
        if held.schema_version != REVIEW_RECEIPT_PROJECTION_SCHEMA {
            anyhow::bail!("review receipt cursor declares an unknown schema version");
        }
        Ok(EvidenceCompletionCursor::consumed_through(
            held.consumed_through,
        ))
    }

    fn write_cursor(
        &self,
        scope: &EvidenceDecisionScope,
        cursor: EvidenceCompletionCursor,
    ) -> Result<()> {
        self.write_cursor_at(
            self.cursor_path(scope),
            cursor,
            "persisting the review receipt projection cursor",
        )
    }

    fn write_cursor_at(
        &self,
        path: PathBuf,
        cursor: EvidenceCompletionCursor,
        context: &'static str,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(&ReviewReceiptCursorFile {
            schema_version: REVIEW_RECEIPT_PROJECTION_SCHEMA,
            consumed_through: cursor.seq(),
        })?;
        self.workspace_layout
            .write_atomic_path_sync(path, &bytes)
            .context(context)?;
        Ok(())
    }
}

/// How one drain page resolves its claim receipts.
///
/// [`TranscriptIngestion::journalled_claim_receipt`] is an exact read, but the
/// claims log is one log per *scope* rather than one per claim — so resolving a
/// page entry by entry folds and re-parses the whole workspace's claim history
/// once per entry. That is invisible in the steady state, where a destination
/// call journals exactly one completion and the page holds exactly one entry;
/// it is the whole cost of the call for a scope that has fallen behind, and the
/// drain runs on the owner's request.
///
/// [`TranscriptIngestion::decision_receipts`] — which exists for this consumer:
/// "rebuilding the package-owned audit projection" — costs two folds for the
/// whole page however many entries it holds (one to validate the heads, one to
/// read the receipts off them). So it loses to the exact read at one entry,
/// breaks even at two and wins from three, and the threshold below is that
/// break-even rather than a tuning knob. It is taken *at* the break-even
/// because the bulk read also checks every receipted event against its prior
/// head before a projector may observe it, which the exact read does not — the
/// direction a fail-closed ledger wants to be wrong in. Peak memory does not
/// grow either way: the exact read already parses the whole log into a `Vec`
/// once per entry, and this holds only the receipts.
enum PageClaimReceipts {
    /// Ask the register for each entry's receipt, one exact read at a time.
    PerEntry,
    /// One resolution of the whole page, indexed by decision id.
    Indexed(HashMap<String, ClaimDecisionReceipt>),
}

impl PageClaimReceipts {
    fn for_page(
        ingestion: &TranscriptIngestion,
        scope: &OutwardScope,
        entries: &[EvidenceCompletionEntry],
    ) -> Result<Self> {
        let claim_entries = entries
            .iter()
            .filter(|entry| matches!(entry.target, EvidenceDecisionTarget::TranscriptClaim { .. }))
            .count();
        if claim_entries < 2 {
            return Ok(Self::PerEntry);
        }
        let mut indexed = HashMap::with_capacity(claim_entries);
        for receipt in ingestion.decision_receipts(scope)? {
            // First in log order wins, which is the rule the register's own
            // scan follows. Decision ids are unique within a register, so this
            // only decides a case that would already be corruption — and it
            // decides it the same way the exact read would.
            indexed
                .entry(receipt.decision_id.clone())
                .or_insert(receipt);
        }
        Ok(Self::Indexed(indexed))
    }

    /// The receipt one entry addresses, bound to the claim that entry names.
    ///
    /// The binding is the register's own rule — `journalled_claim_receipt`
    /// refuses a locator pointing at another claim's receipt — restated here
    /// because the indexed path reads the receipt stream directly and would
    /// otherwise drop it. A decision id is an idempotency key, never permission
    /// to project a row about a different claim.
    fn receipt(
        &self,
        ingestion: &TranscriptIngestion,
        scope: &OutwardScope,
        claim_id: &str,
        decision_id: &str,
    ) -> Result<Option<ClaimDecisionReceipt>> {
        let Self::Indexed(indexed) = self else {
            return ingestion.journalled_claim_receipt(scope, claim_id, decision_id);
        };
        let Some(receipt) = indexed.get(decision_id) else {
            return Ok(None);
        };
        if receipt.claim_id != claim_id {
            anyhow::bail!(
                "claim decision `{decision_id}` is recorded against claim `{}`, not `{claim_id}`",
                receipt.claim_id
            );
        }
        Ok(Some(receipt.clone()))
    }
}

/// The journal is a locator, so the receipt it points at has to be the receipt
/// it described. A decision id is an idempotency key, never permission to
/// substitute a different request — the rule both registers already enforce,
/// carried across the journal hop so a projected row cannot describe a command
/// nobody signed.
fn bind_entry_to_receipt(
    entry: &EvidenceCompletionEntry,
    decision_id: &str,
    receipt_id: &str,
    request_fingerprint: &str,
) -> Result<()> {
    if entry.decision_id != decision_id
        || entry.receipt_id != receipt_id
        || entry.request_fingerprint != request_fingerprint
    {
        anyhow::bail!(
            "completed decision `{}` does not match the authoritative receipt it addresses",
            entry.decision_id
        );
    }
    Ok(())
}

fn missing_receipt(decision_id: &str) -> String {
    format!(
        "completed decision `{decision_id}` names a receipt its register does not hold; refusing \
         to project a receipt row over a hole"
    )
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::audience::AudienceRef;
    use crate::magician_v2::commitments::{CommitmentDirection, RecordCommitment};
    use crate::magician_v2::evidence::completion_journal::EvidenceDecisionCompletion;
    use crate::magician_v2::evidence::transcript_ingestion::{
        OwnerDecision, SpeakerAttribution, TranscriptSource, TranscriptUtterance,
    };

    struct Fixture {
        _tmp: tempfile::TempDir,
        layout: ArtifactV2Workspace,
        projector: ReviewReceiptProjector,
        ingestion: TranscriptIngestion,
        commitments: Commitments,
        journal: EvidenceCompletionJournal,
        scope: EvidenceDecisionScope,
        outward: OutwardScope,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().expect("temp dir");
        let layout = ArtifactV2Workspace::new(tmp.path());
        Fixture {
            _tmp: tmp,
            projector: ReviewReceiptProjector::new(layout.clone()),
            ingestion: TranscriptIngestion::new(layout.clone()),
            commitments: Commitments::new(layout.clone()),
            journal: EvidenceCompletionJournal::new(layout.clone()),
            layout,
            scope: EvidenceDecisionScope::new("anonymous", "default"),
            outward: OutwardScope::new("anonymous", "default"),
        }
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    /// Keyed, because a page holding more than one claim decision needs more
    /// than one transcript: ingesting the same key with different words is
    /// refused, on purpose.
    fn source(transcript_key: &str) -> TranscriptSource {
        let mut source = TranscriptSource::observed(
            transcript_key,
            "founder@example.com",
            vec!["alice@example.com".to_string()],
            "transcript-extractor",
            at("2026-09-04T09:00:00Z"),
        );
        source.audience = Some(AudienceRef::engagement("eng-1"));
        source.engagement_id = Some("eng-1".to_string());
        source
    }

    /// The installation a publication test publishes into. Every publication
    /// call names one, because the cursor belongs to the destination store
    /// rather than to the scope.
    fn publisher() -> ReviewReceiptPublisher {
        ReviewReceiptPublisher::installation("installation:claims-review")
            .expect("a named installation")
    }

    /// One claim, confirmed through the receipt-bearing path — which is what
    /// journals the completion this projector exists to consume.
    fn confirmed_claim(fixture: &Fixture, decision_id: &str) -> String {
        confirmed_claim_from(
            fixture,
            "call-2026-09-04",
            "we can start in March",
            decision_id,
        )
    }

    fn confirmed_claim_from(
        fixture: &Fixture,
        transcript_key: &str,
        spoken: &str,
        decision_id: &str,
    ) -> String {
        let ingested = fixture
            .ingestion
            .ingest_transcript(
                &fixture.outward,
                &source(transcript_key),
                &[TranscriptUtterance::said(
                    "seg-1",
                    SpeakerAttribution::Ours("founder@example.com".to_string()),
                    spoken,
                )],
                at("2026-09-04T10:00:00Z"),
            )
            .expect("ingest");
        let claim_id = ingested.claims[0].claim_id.clone();
        fixture
            .ingestion
            .confirm_claim_at_revision(
                &fixture.outward,
                &claim_id,
                1,
                decision_id,
                &OwnerDecision::by("owner@example.com"),
                at("2026-09-04T10:01:00Z"),
            )
            .expect("confirm");
        claim_id
    }

    #[test]
    fn a_completed_claim_decision_projects_one_applied_row() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "decision-one");

        let drained = fixture
            .projector
            .drain(
                &fixture.scope,
                &fixture.ingestion,
                &fixture.commitments,
                MAX_REVIEW_RECEIPT_PAGE,
            )
            .expect("drain");
        assert_eq!(drained.minted, 1);
        assert!(!drained.has_more);

        let row = fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "decision-one",
            )
            .expect("read row")
            .expect("row exists");
        assert_eq!(row.decision_id, "decision-one");
        assert_eq!(row.target_kind, ReviewReceiptTargetKind::Claim);
        assert_eq!(row.target_id, claim_id);
        assert_eq!(row.outcome, ReviewReceiptOutcome::Applied);
        assert_eq!(row.actor_ref, "owner@example.com");
        assert_eq!(row.destination_revision, 2);
        assert!(row.error_code.is_none());
    }

    /// The signed source head is the app's row and stays `recorded`; the
    /// outcome lives only here. Nothing in the projector may reach back into a
    /// `review_decision`, so the ledger it writes is addressed by the canonical
    /// decision id under its register's family, and nothing else.
    #[test]
    fn the_row_is_addressed_by_the_canonical_decision_id() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        assert!(fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "decision-one"
            )
            .expect("read row")
            .is_some());
        assert!(fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "some-other-decision",
            )
            .expect("read row")
            .is_none());
    }

    /// The cursor is the whole point: a second drain over an unchanged journal
    /// re-reads nothing and re-mints nothing.
    #[test]
    fn a_second_drain_over_an_unchanged_journal_is_empty() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        let first = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("first drain");
        let second = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("second drain");
        assert_eq!(first.minted, 1);
        assert_eq!(second.minted, 0);
        assert_eq!(second.already_projected, 0);
        assert_eq!(second.cursor, first.cursor);
    }

    /// A journal whose locator was lost heals by re-journalling the same
    /// completion at a fresh, higher seq — which is exactly why a reader that
    /// had already passed the original position still sees it. The projector
    /// therefore sees one completion twice, on purpose, and the row it would
    /// write the second time is the row already there: it mints nothing, and it
    /// does not treat the repeat as corruption.
    #[test]
    fn a_healed_journal_entry_is_projected_again_and_mints_nothing() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "decision-one");
        let first = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("first drain");
        assert_eq!(first.minted, 1);

        // Drop the locators, which is what a crash between the locator write
        // and the log append leaves behind. `record_completion` then cannot see
        // its own entry and mints a second one.
        std::fs::remove_dir_all(
            fixture
                .layout
                .scope_root("anonymous", "default")
                .join("decision_journal")
                .join("locators"),
        )
        .expect("drop the journal locators");

        let receipt = fixture
            .ingestion
            .journalled_claim_receipt(&fixture.outward, &claim_id, "decision-one")
            .expect("read receipt")
            .expect("receipt exists");
        let healed = fixture
            .journal
            .record_completion(
                &fixture.scope,
                &EvidenceDecisionCompletion {
                    decision_id: receipt.decision_id.clone(),
                    target: EvidenceDecisionTarget::TranscriptClaim {
                        claim_id: receipt.claim_id.clone(),
                    },
                    receipt_id: receipt.receipt_id.clone(),
                    request_fingerprint: receipt.request_fingerprint.clone(),
                    completed_at: receipt.recorded_at,
                },
                at("2026-09-04T10:05:00Z"),
            )
            .expect("heal the journal");
        assert_eq!(healed.seq, 2);

        let again = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("second drain");
        assert_eq!(again.minted, 0);
        assert_eq!(again.already_projected, 1);
        assert_eq!(again.cursor, EvidenceCompletionCursor::consumed_through(2));
    }

    /// The journal promises an authoritative receipt is durable at the address
    /// it names. A promise it cannot keep stops the drain instead of advancing
    /// past it, because a skipped completion is lost forever.
    #[test]
    fn a_completion_without_its_receipt_stalls_the_cursor() {
        let fixture = fixture();
        fixture
            .journal
            .record_completion(
                &fixture.scope,
                &EvidenceDecisionCompletion {
                    decision_id: "decision-orphan".to_owned(),
                    target: EvidenceDecisionTarget::TranscriptClaim {
                        claim_id: "claim-orphan".to_owned(),
                    },
                    receipt_id: "receipt-orphan".to_owned(),
                    request_fingerprint: "fingerprint-orphan".to_owned(),
                    completed_at: at("2026-09-04T10:00:00Z"),
                },
                at("2026-09-04T10:00:00Z"),
            )
            .expect("journal an orphan completion");

        assert!(fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .is_err());
        assert_eq!(
            fixture.projector.cursor(&fixture.scope).expect("cursor"),
            EvidenceCompletionCursor::START
        );
    }

    /// A stall must not also be a tax. The cursor's claim is about the prefix,
    /// and every row before the failing entry is already durable — so throwing
    /// that prefix away made the next decision in the scope re-project it, and
    /// re-fold both registers for it, and fail at the same entry again, for as
    /// long as the entry stayed unbindable. The cursor still stops short of the
    /// failing entry, so nothing is skipped.
    #[test]
    fn a_stalled_page_keeps_the_entries_that_did_project() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .journal
            .record_completion(
                &fixture.scope,
                &EvidenceDecisionCompletion {
                    decision_id: "decision-orphan".to_owned(),
                    target: EvidenceDecisionTarget::TranscriptClaim {
                        claim_id: "claim-orphan".to_owned(),
                    },
                    receipt_id: "receipt-orphan".to_owned(),
                    request_fingerprint: "fingerprint-orphan".to_owned(),
                    completed_at: at("2026-09-04T10:02:00Z"),
                },
                at("2026-09-04T10:02:00Z"),
            )
            .expect("journal an orphan completion behind a projectable one");

        assert!(fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .is_err());
        assert_eq!(
            fixture.projector.cursor(&fixture.scope).expect("cursor"),
            EvidenceCompletionCursor::consumed_through(1),
            "the entry that projected is consumed; the one that stalled is not"
        );
        assert!(fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "decision-one"
            )
            .expect("read row")
            .is_some());

        // And it stays exactly there: the stall repeats, without dragging the
        // prefix through both registers again.
        assert!(fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .is_err());
        assert_eq!(
            fixture.projector.cursor(&fixture.scope).expect("cursor"),
            EvidenceCompletionCursor::consumed_through(1)
        );
    }

    /// The pin for the batched claim read. A page holding more than one claim
    /// decision resolves its receipts in one pass instead of folding the
    /// scope-wide claims log once per entry — and each row must still point at
    /// its own claim. A batch that handed the wrong receipt to an entry would
    /// mint a row asserting a decision about a claim nobody decided, which is
    /// worse than the cost it removes.
    #[test]
    fn a_page_of_several_claim_decisions_projects_each_to_its_own_claim() {
        let fixture = fixture();
        let first = confirmed_claim_from(
            &fixture,
            "call-2026-09-04",
            "we can start in March",
            "decision-one",
        );
        let second = confirmed_claim_from(
            &fixture,
            "call-2026-09-05",
            "we can hold the price through Q3",
            "decision-two",
        );
        assert_ne!(first, second, "two transcripts, two claims");

        let drained = fixture
            .projector
            .drain(
                &fixture.scope,
                &fixture.ingestion,
                &fixture.commitments,
                MAX_REVIEW_RECEIPT_PAGE,
            )
            .expect("one page holding both completions");
        assert_eq!(drained.minted, 2);
        assert_eq!(
            drained.cursor,
            EvidenceCompletionCursor::consumed_through(2)
        );

        for (decision_id, claim_id) in [("decision-one", &first), ("decision-two", &second)] {
            let row = fixture
                .projector
                .receipt(&fixture.scope, ReviewReceiptTargetKind::Claim, decision_id)
                .expect("read row")
                .expect("row exists");
            assert_eq!(&row.target_id, claim_id, "each row keeps its own claim");
            assert_eq!(row.decision_id, decision_id);
            assert_eq!(row.outcome, ReviewReceiptOutcome::Applied);
            assert_eq!(row.actor_ref, "owner@example.com");
            assert_eq!(row.destination_revision, 2);
        }
    }

    /// A commitment recorded from a claim is the one verb the register takes
    /// with nobody's name on it. The row says exactly that instead of borrowing
    /// the signer of the command that caused it.
    #[test]
    fn a_mechanical_commitment_record_names_no_person() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "decision-one");
        let recorded = fixture
            .ingestion
            .record_commitment_from_claim_at_revision(
                &fixture.commitments,
                &fixture.outward,
                &claim_id,
                2,
                "decision-two",
                at("2026-09-04T10:02:00Z"),
            )
            .expect("record the term");

        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        let row = fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Commitment,
                "decision-two",
            )
            .expect("read row")
            .expect("row exists");
        assert_eq!(row.target_kind, ReviewReceiptTargetKind::Commitment);
        assert_eq!(row.target_id, recorded.commitment.commitment_id);
        assert_eq!(row.actor_ref, REVIEW_RECEIPT_UNNAMED_ACTOR);
        assert_eq!(row.outcome, ReviewReceiptOutcome::Applied);
    }

    /// Both families share one cursor, because a cursor over one of them is not
    /// a cursor over the scope.
    #[test]
    fn one_drain_covers_both_decision_families() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "decision-one");
        fixture
            .ingestion
            .record_commitment_from_claim_at_revision(
                &fixture.commitments,
                &fixture.outward,
                &claim_id,
                2,
                "decision-two",
                at("2026-09-04T10:02:00Z"),
            )
            .expect("record the term");

        let drained = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        assert_eq!(drained.minted, 2);
        assert!(fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "decision-one"
            )
            .expect("read row")
            .is_some());
        assert!(fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Commitment,
                "decision-two",
            )
            .expect("read row")
            .is_some());
    }

    /// The pin for `(family, decision id)`. Neither register can see the
    /// other's decision ids — the claims register replays by folding its claim
    /// log, the commitment register by reading its own index — so a client is
    /// free to reuse one idempotency key across them, and the journal
    /// deliberately holds the pair as two completions.
    ///
    /// Keyed on the bare id the second completion would land on the first's
    /// row, `persist` would read the differing `target_kind`/`target_id` as
    /// corruption and bail, and the cursor — written only after a whole page
    /// persists — would never pass that seq. Both receipts are immutable, so
    /// nothing could ever resolve the disagreement: every later decision in the
    /// scope would go unprojected for good, behind a warning the destination
    /// swallows.
    #[test]
    fn one_decision_id_in_both_registers_projects_two_rows() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "shared-decision");
        let recorded = fixture
            .ingestion
            .record_commitment_from_claim_at_revision(
                &fixture.commitments,
                &fixture.outward,
                &claim_id,
                2,
                "shared-decision",
                at("2026-09-04T10:02:00Z"),
            )
            .expect("the commitment register's own `shared-decision` is a second decision");

        let drained = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("two legal completions must project, not collide");
        assert_eq!(drained.minted, 2);
        assert_eq!(
            drained.cursor,
            EvidenceCompletionCursor::consumed_through(2)
        );

        let claim_row = fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "shared-decision",
            )
            .expect("read the claim row")
            .expect("the claim row exists");
        let commitment_row = fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Commitment,
                "shared-decision",
            )
            .expect("read the commitment row")
            .expect("the commitment row exists");
        assert_eq!(claim_row.target_id, claim_id);
        assert_eq!(
            commitment_row.target_id, recorded.commitment.commitment_id,
            "each row points at its own register's target"
        );
        assert_ne!(claim_row.receipt_id, commitment_row.receipt_id);
        // The same collision one layer up: two rows that shared a store record
        // id would let a publisher overwrite one decision with the other, or
        // refuse the second forever.
        assert_ne!(
            claim_row.package_record_id(),
            commitment_row.package_record_id(),
            "and each occupies its own record in the package store"
        );
    }

    /// The closed door: a journal-driven projection can never claim a refusal
    /// or an ingest, because neither reaches a register and neither mints a
    /// completion. Both stay in the vocabulary so it cannot drift from the
    /// entity, and E3 owns the ingest half.
    #[test]
    fn a_journalled_projection_never_claims_a_refusal_or_an_ingest() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        let row = fixture
            .projector
            .receipt(
                &fixture.scope,
                ReviewReceiptTargetKind::Claim,
                "decision-one",
            )
            .expect("read row")
            .expect("row exists");
        assert_ne!(row.outcome, ReviewReceiptOutcome::Refused);
        assert_ne!(row.target_kind, ReviewReceiptTargetKind::Ingest);
    }

    /// The unreceipted commitment transitions mint no receipt, so there is
    /// nothing for a receipt projector to publish and it must not invent a row
    /// saying a decision was applied that nobody decided.
    #[test]
    fn an_unreceipted_transition_projects_nothing() {
        let fixture = fixture();
        fixture
            .commitments
            .record(
                &CommitmentScope::new("anonymous", "default"),
                &RecordCommitment {
                    audience: AudienceRef::engagement("eng-1"),
                    source_ref: "claim:seed".to_owned(),
                    direction: CommitmentDirection::StatedByUs,
                    terms: "we can start in March".to_owned(),
                    stated_at: at("2026-09-04T09:00:00Z"),
                },
                at("2026-09-04T10:00:00Z"),
            )
            .expect("record a term without a decision receipt");

        let drained = fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        assert_eq!(drained.minted, 0);
        assert_eq!(
            fixture.projector.cursor(&fixture.scope).expect("cursor"),
            EvidenceCompletionCursor::START
        );
    }

    #[test]
    fn an_unbounded_page_is_refused_rather_than_clamped() {
        let fixture = fixture();
        assert!(fixture
            .projector
            .drain(
                &fixture.scope,
                &fixture.ingestion,
                &fixture.commitments,
                MAX_REVIEW_RECEIPT_PAGE + 1,
            )
            .is_err());
        assert!(fixture
            .projector
            .drain(&fixture.scope, &fixture.ingestion, &fixture.commitments, 0)
            .is_err());
    }

    /// The reason the whole publication half exists: a minted row is a host
    /// file, and the console reads the package entity. A row nobody hands to
    /// the store leaves `/receipts` empty for the life of the installation.
    #[test]
    fn a_minted_row_is_offered_for_publication_with_its_journal_position() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");

        let page = fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
            )
            .expect("pending");
        assert_eq!(page.pending.len(), 1);
        assert!(!page.has_more);
        let pending = &page.pending[0];
        assert_eq!(pending.row.decision_id, "decision-one");
        assert_eq!(
            pending.cursor_after,
            EvidenceCompletionCursor::consumed_through(1)
        );
        assert_eq!(
            pending.row.package_record_id(),
            format!("receipt-claim-{}", stable_id("decision-one")),
            "the store row is named for its family and its decision"
        );
    }

    /// A recorded publication is done. Offering it again would be harmless in
    /// the store — a named row collides — but a cursor that never advanced
    /// would republish the whole ledger on every decision forever.
    #[test]
    fn a_recorded_publication_is_not_offered_again() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        let page = fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
            )
            .expect("pending");
        fixture
            .projector
            .record_published(&fixture.scope, &publisher(), page.pending[0].cursor_after)
            .expect("record the publication");

        assert_eq!(
            fixture
                .projector
                .publication_cursor(&fixture.scope, &publisher())
                .expect("publication cursor"),
            EvidenceCompletionCursor::consumed_through(1)
        );
        let again = fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
            )
            .expect("pending");
        assert!(again.pending.is_empty());
        assert!(!again.has_more);
    }

    /// The publisher derives a row's family from the journal entry, and the
    /// projector derived it from the register it applied to. If those two ever
    /// disagreed the ledger would stall silently, so both families are walked
    /// here in one page.
    #[test]
    fn one_publication_page_covers_both_decision_families() {
        let fixture = fixture();
        let claim_id = confirmed_claim(&fixture, "decision-one");
        fixture
            .ingestion
            .record_commitment_from_claim_at_revision(
                &fixture.commitments,
                &fixture.outward,
                &claim_id,
                2,
                "decision-two",
                at("2026-09-04T10:02:00Z"),
            )
            .expect("record the term");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");

        let page = fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
            )
            .expect("pending");
        assert_eq!(page.pending.len(), 2);
        assert_eq!(
            page.pending[0].row.target_kind,
            ReviewReceiptTargetKind::Claim
        );
        assert_eq!(
            page.pending[1].row.target_kind,
            ReviewReceiptTargetKind::Commitment
        );
        assert_eq!(
            page.pending[1].cursor_after,
            EvidenceCompletionCursor::consumed_through(2)
        );
    }

    /// Publication may never run ahead of projection. A completion whose row is
    /// not minted yet ends the page still marked incomplete, so a caller cannot
    /// read the short page as caught up and advance over the hole.
    #[test]
    fn publication_stops_at_a_completion_the_projection_has_not_reached() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain(
                &fixture.scope,
                &fixture.ingestion,
                &fixture.commitments,
                MAX_REVIEW_RECEIPT_PAGE,
            )
            .expect("project the first decision only");
        fixture
            .journal
            .record_completion(
                &fixture.scope,
                &EvidenceDecisionCompletion {
                    decision_id: "decision-unprojected".to_owned(),
                    target: EvidenceDecisionTarget::TranscriptClaim {
                        claim_id: "claim-unprojected".to_owned(),
                    },
                    receipt_id: "receipt-unprojected".to_owned(),
                    request_fingerprint: "fingerprint-unprojected".to_owned(),
                    completed_at: at("2026-09-04T10:03:00Z"),
                },
                at("2026-09-04T10:03:00Z"),
            )
            .expect("journal a completion the projector has not consumed");

        let page = fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE,
            )
            .expect("pending");
        assert_eq!(page.pending.len(), 1);
        assert_eq!(page.pending[0].row.decision_id, "decision-one");
        assert!(
            page.has_more,
            "a page cut short by an unprojected completion is not a caught-up page"
        );
    }

    /// The cursor is a claim that those rows reached the store. Moving it over
    /// rows nobody published loses them for good, and moving it backwards
    /// republishes a ledger that is already correct.
    #[test]
    fn a_publication_cursor_may_not_rewind_or_pass_the_projection() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");

        assert!(
            fixture
                .projector
                .record_published(
                    &fixture.scope,
                    &publisher(),
                    EvidenceCompletionCursor::consumed_through(2),
                )
                .is_err(),
            "seq 2 is not projected, so nothing could have published it"
        );
        fixture
            .projector
            .record_published(
                &fixture.scope,
                &publisher(),
                EvidenceCompletionCursor::consumed_through(1),
            )
            .expect("publishing the projected row is allowed");
        assert!(
            fixture
                .projector
                .record_published(
                    &fixture.scope,
                    &publisher(),
                    EvidenceCompletionCursor::START
                )
                .is_err(),
            "a published row stays published"
        );
        assert_eq!(
            fixture
                .projector
                .publication_cursor(&fixture.scope, &publisher())
                .expect("publication cursor"),
            EvidenceCompletionCursor::consumed_through(1)
        );
    }

    #[test]
    fn an_unbounded_publication_page_is_refused_rather_than_clamped() {
        let fixture = fixture();
        assert!(fixture
            .projector
            .pending_publications(
                &fixture.scope,
                &publisher(),
                MAX_REVIEW_RECEIPT_PUBLICATION_PAGE + 1,
            )
            .is_err());
        assert!(fixture
            .projector
            .pending_publications(&fixture.scope, &publisher(), 0)
            .is_err());
    }

    /// The entity name is what the publisher writes into; the package declares
    /// it and no workflow may mutate it, so a typo here is a receipts view that
    /// stays empty rather than an error anyone sees.
    #[test]
    fn the_published_entity_is_the_packages_declared_receipt_entity() {
        assert_eq!(REVIEW_RECEIPT_ENTITY, "review_receipt");
    }

    /// The defect this key exists to prevent. Two installations in one scope —
    /// a fork of the claims-review package, or the same package installed
    /// twice — are two entity stores. A cursor keyed by scope alone let the
    /// first one to publish drain the scope's whole backlog into itself and
    /// then tell every other installation those rows were already delivered,
    /// which they were not and, because the cursor never rewinds, never could
    /// be.
    #[test]
    fn one_installations_publication_never_speaks_for_another() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        let first =
            ReviewReceiptPublisher::installation("installation:one").expect("a named installation");
        let second =
            ReviewReceiptPublisher::installation("installation:two").expect("a named installation");

        let page = fixture
            .projector
            .pending_publications(&fixture.scope, &first, MAX_REVIEW_RECEIPT_PUBLICATION_PAGE)
            .expect("pending");
        fixture
            .projector
            .record_published(&fixture.scope, &first, page.pending[0].cursor_after)
            .expect("record the publication");

        assert_eq!(
            fixture
                .projector
                .publication_cursor(&fixture.scope, &second)
                .expect("publication cursor"),
            EvidenceCompletionCursor::START,
            "a store that received nothing holds no position"
        );
        let still_pending = fixture
            .projector
            .pending_publications(&fixture.scope, &second, MAX_REVIEW_RECEIPT_PUBLICATION_PAGE)
            .expect("pending");
        assert_eq!(still_pending.pending.len(), 1);
        assert_eq!(still_pending.pending[0].row.decision_id, "decision-one");
    }

    /// A reinstall mints a new installation id over an empty entity store, so
    /// the scope's receipt history has to be offered to it. Under the scope-wide
    /// cursor it never was: the cursor already sat at the journal head, and
    /// `/receipts` stayed empty for the life of the new installation.
    #[test]
    fn a_reinstalled_installation_is_offered_the_whole_history() {
        let fixture = fixture();
        confirmed_claim(&fixture, "decision-one");
        fixture
            .projector
            .drain_all(&fixture.scope, &fixture.ingestion, &fixture.commitments)
            .expect("drain");
        let before = ReviewReceiptPublisher::installation("installation:before-reinstall")
            .expect("a named installation");
        let page = fixture
            .projector
            .pending_publications(&fixture.scope, &before, MAX_REVIEW_RECEIPT_PUBLICATION_PAGE)
            .expect("pending");
        fixture
            .projector
            .record_published(&fixture.scope, &before, page.pending[0].cursor_after)
            .expect("record the publication");

        let after = ReviewReceiptPublisher::installation("installation:after-reinstall")
            .expect("a named installation");
        let replayed = fixture
            .projector
            .pending_publications(&fixture.scope, &after, MAX_REVIEW_RECEIPT_PUBLICATION_PAGE)
            .expect("pending");
        assert_eq!(replayed.pending.len(), 1);
        assert_eq!(replayed.pending[0].row.decision_id, "decision-one");
    }

    /// An unnamed destination is refused rather than defaulted. A publisher
    /// that fell back to a shared address would reintroduce the scope-wide
    /// cursor under a different name.
    #[test]
    fn a_publisher_must_name_the_installation_it_publishes_into() {
        assert!(ReviewReceiptPublisher::installation("").is_err());
        assert!(ReviewReceiptPublisher::installation("   ").is_err());
    }
}
