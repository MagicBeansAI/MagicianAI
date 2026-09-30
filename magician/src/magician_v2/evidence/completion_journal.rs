//! The monotonic completion journal for evidence decisions.
//!
//! Gate E2 of the apps-platform register
//! (`docs/plans/2026-09-03-apps_platform_open-gates.md`), and a required reopen
//! *before* the receipt projector (E1) can exist.
//!
//! # Why the decision indexes cannot be the cursor
//!
//! A scope-wide commitment receipt page was removed during the evidence
//! increment's review. It paged the per-decision index files, which are named
//! `blake3(decision_id)` — and a hash-ordered directory is not a cursor:
//!
//! 1. **An index is prepared before its receipt exists.** The commitment store
//!    writes the scope-wide locator, then the register row, then the locator
//!    again carrying the receipt. A reader that walked the directory in between
//!    would see a decision with no outcome, page past it, and never come back.
//! 2. **A new hash can sort BEHIND the cursor.** Decision ids are opaque, so
//!    the hash of a decision taken tomorrow is as likely to precede today's as
//!    to follow it. Anything that lands behind a hash cursor is lost silently,
//!    which is the one failure a receipt projector may not have.
//!
//! Both faults have the same shape: the *order* was derived from the identity
//! instead of from the completion. This journal derives it from the completion.
//! A sequence number is assigned at append time, under a lock, from the journal
//! itself — so nothing can ever be inserted behind a reader, and "I consumed
//! through seq N" is a lossless cursor.
//!
//! # What it is, exactly
//!
//! One append-only, densely-numbered log per `(principal, workspace)` scope,
//! shared by every evidence decision family (the transcript-claims register and
//! the per-audience commitment register), because a cursor that only covers one
//! family is not a cursor over the scope. Sequence numbers start at one and
//! increase by exactly one. A gap is corruption and every read refuses it:
//! silently skipping a missing seq is precisely the loss the journal exists to
//! make impossible.
//!
//! # The journal is a locator, never an authority
//!
//! An entry says *"decision D completed, and its receipt lives at this exact
//! address"*. It does not carry the outcome. The authoritative receipt stays in
//! the register record it was written with, and a consumer reads it there —
//! the same rule that keeps the package audit ledger a projection. A journal
//! that carried the outcome would be a second copy of the decision, free to
//! disagree with the register.
//!
//! # Write ordering, and what a crash leaves behind
//!
//! The register row is authoritative and is written FIRST; the journal entry
//! follows. Journal-first would announce completions that never completed,
//! which fails open for the projector. Register-first can instead lose an
//! entry if the process dies in between — so every decision path also journals
//! on its **replay** branch, where [`Self::record_completion`] is idempotent
//! by `(family, decision id)`. A retry of an interrupted decision therefore
//! heals the journal rather than duplicating it, and the healed entry gets a
//! fresh (higher) seq, which is exactly why a reader that had already passed
//! the original position still sees it.
//!
//! Consequence, in the open: a decision whose register row landed but whose
//! journal append failed is reported to the caller as an error. The caller
//! retries, gets `already_applied`, and the journal is repaired. That is the
//! fail-closed reading — the alternative is a completed decision no projector
//! will ever hear about.
//!
//! An elapsed proposal lifetime does not close that retry, which is worth
//! saying because the opposite reads as obvious and is wrong. Every guarded
//! register method answers a durable receipt on its **replay branch before it
//! consults the caller's admission callback** — that callback guards a new
//! durable write, and a repair is not one — and the signed apply seam derives
//! its decision id from the claim, verb, decider, note and verdict rather than
//! from the proposal, so a freshly signed envelope for the same decision lands
//! on that same replay branch instead of arriving as a second decision. The
//! ordinary repair therefore stays available for as long as the app-side
//! source authority the apply seam re-checks still matches.
//!
//! When it no longer matches, the decision is not re-submittable by any
//! envelope, and a read-shaped recovery entry point is the only route left to
//! its receipt — `TranscriptIngestion::recover_claim_decision` and
//! `Commitments::recover_decision_receipt`. Those journal too, for the same
//! reason and under the same idempotence: a recovery route that returned the
//! receipt and appended nothing would leave exactly the permanent hole this
//! section promises cannot exist, and seq numbers would stay dense over it
//! because they are assigned at append, so nothing downstream would notice.
//! Neither has a caller outside its own tests today — they are a door held
//! open for that case, not the route the repair usually takes, and naming them
//! here is about the rule they follow, not about their reach. The rule is
//! written down because this module cannot enforce it: a caller that never
//! appends is indistinguishable, from inside the journal, from a scope where
//! nothing has completed.
//!
//! # Three boundaries worth stating
//!
//! * **The idempotency key is `(family, decision id)`, never the id alone.**
//!   A decision id is a client-supplied idempotency key and each register
//!   namespaces its own: the claims register replays one by folding its claim
//!   log, the commitment register by reading its per-scope decision index, and
//!   nothing makes the two agree on an id. Keying the journal on the bare id
//!   would let the second family's decision inherit the first's entry — whose
//!   target can never match — so its authoritative register row would land and
//!   every journal call for it, first write and replay alike, would fail
//!   forever. The journal is the one thing here that spans both families, so
//!   the namespace is its to own; a register cannot fix this from its side.
//! * **One lock per scope is the total order.** Appending takes a single
//!   cross-process lock at the journal root, so a claim decision and a
//!   commitment decision in the same scope serialise against each other. That
//!   contention *is* the product: a per-family order is not an order over the
//!   scope. Every caller takes its register locks first and this one last, so
//!   the acquisition order is one-directional.
//! * **Only receipted transitions are journalled.** The unreceipted
//!   `record`/`confirm`/`supersede`/`withdraw` paths mint no receipt, so there
//!   is nothing for a receipt projector to project and an entry for one would
//!   be a completion pointing at an address that holds nothing.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;
use crate::magician_v2::execution::file_edit::transaction::acquire_record_decision_lock;
use crate::magician_v2::jsonl;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;

const COMPLETION_JOURNAL_SCHEMA: u32 = 1;

/// Entries per segment file. The journal is addressed rather than folded:
/// seq `n` lives in segment `(n - 1) / SPAN` at position `(n - 1) % SPAN`, so a
/// page starting at any cursor opens one file instead of replaying history from
/// byte zero. E4 is reopening exactly that unbounded fold for the older stores;
/// there is no reason to build a new one already carrying the defect.
const JOURNAL_SEGMENT_SPAN: u64 = 512;

/// Zero-padded width of a segment file name, so lexical order — which is what
/// `jsonl::list_log_paths` sorts by — is numeric order.
const SEGMENT_NAME_WIDTH: usize = 12;

const MAX_LOCATOR_BYTES: u64 = 8 * 1024;
const MAX_JOURNAL_ENTRY_BYTES: usize = 4 * 1024;
const MAX_DECISION_ID_BYTES: usize = 192;

/// Ceiling on one page. A caller asking for more than this is asking for an
/// unbounded read, which is refused rather than quietly clamped: a silently
/// shortened page looks identical to the end of the journal.
pub const MAX_COMPLETION_PAGE_ENTRIES: usize = 512;

/// Scope for a journal call. Structurally `(principal, workspace)`, like every
/// other store here, but its own type: the journal spans the claims and
/// commitment registers, so borrowing either family's scope would make one of
/// them look like the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceDecisionScope {
    pub principal: String,
    pub workspace: String,
}

impl EvidenceDecisionScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// Which register minted a decision id — the first half of the journal's
/// idempotency key, per the module note.
///
/// Its own type rather than a borrowed target, because the key has to be
/// addressable without a target: a locator is written and looked up by
/// `(family, decision id)` before the entry it points at exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecisionFamily {
    TranscriptClaim,
    Commitment,
}

impl DecisionFamily {
    /// The locator directory this family's hints live under. Deliberately the
    /// same strings [`EvidenceDecisionTarget`] serialises its `family` tag as,
    /// so a locator path and the entry it addresses read alike on disk.
    fn as_str(self) -> &'static str {
        match self {
            Self::TranscriptClaim => "transcript_claim",
            Self::Commitment => "commitment",
        }
    }
}

/// The exact address of the authoritative receipt a completed decision wrote.
///
/// Closed by family, and per-family complete: there is no way to describe a
/// commitment completion without the audience it was sharded under, because
/// recovering that receipt without the shard means folding every shard in the
/// scope — the scan this journal exists to remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum EvidenceDecisionTarget {
    /// The pending transcript-claims register. One log per scope, so the claim
    /// id is the whole address.
    TranscriptClaim { claim_id: String },
    /// The commitment register, sharded per audience.
    Commitment {
        audience: AudienceRef,
        commitment_id: String,
    },
}

impl EvidenceDecisionTarget {
    fn family(&self) -> DecisionFamily {
        match self {
            Self::TranscriptClaim { .. } => DecisionFamily::TranscriptClaim,
            Self::Commitment { .. } => DecisionFamily::Commitment,
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::TranscriptClaim { claim_id } => {
                if !is_safe_scope_id(claim_id) {
                    anyhow::bail!("journalled claim id failed the safe-identifier check");
                }
            },
            Self::Commitment {
                audience,
                commitment_id,
            } => {
                if !audience.is_named() {
                    anyhow::bail!(
                        "a journalled commitment completion must name the audience its receipt \
                         is sharded under"
                    );
                }
                if !is_safe_scope_id(commitment_id) {
                    anyhow::bail!("journalled commitment id failed the safe-identifier check");
                }
            },
        }
        Ok(())
    }
}

/// One completed decision, in completion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCompletionEntry {
    pub schema_version: u32,
    /// Dense and strictly increasing from one, assigned at append time. This
    /// is the whole primitive: an identity-derived order can be inserted into,
    /// a completion-derived order cannot.
    pub seq: u64,
    pub decision_id: String,
    pub target: EvidenceDecisionTarget,
    pub receipt_id: String,
    /// The receipt's request fingerprint, so a consumer can bind the entry to
    /// the receipt it recovers. A decision id is an idempotency key, not
    /// permission to substitute a different request — the same rule the
    /// registers already enforce, carried across the journal hop.
    pub request_fingerprint: String,
    /// When the register accepted the decision (the receipt's own time).
    pub completed_at: DateTime<Utc>,
    /// When this entry was appended. Later than `completed_at` when a retry
    /// healed an interrupted journal write, and the gap between them is the
    /// honest record of that.
    pub journaled_at: DateTime<Utc>,
}

/// What a register hands the journal once its receipt is durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceDecisionCompletion {
    pub decision_id: String,
    pub target: EvidenceDecisionTarget,
    pub receipt_id: String,
    pub request_fingerprint: String,
    pub completed_at: DateTime<Utc>,
}

/// How far a consumer has read. Serializable because a projector has to keep it
/// across restarts; opaque because "consumed through seq N" is the only thing
/// it is allowed to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvidenceCompletionCursor(u64);

impl EvidenceCompletionCursor {
    /// Before the first entry. Seq numbers start at one, so zero is the empty
    /// cursor and needs no separate `Option`.
    pub const START: Self = Self(0);

    pub fn consumed_through(seq: u64) -> Self {
        Self(seq)
    }

    pub fn seq(self) -> u64 {
        self.0
    }
}

/// A bounded page of completions, in journal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceCompletionPage {
    pub entries: Vec<EvidenceCompletionEntry>,
    /// Where to resume. Equal to the supplied cursor when the page is empty.
    pub next_cursor: EvidenceCompletionCursor,
    /// Whether the journal holds more beyond `next_cursor` right now.
    pub has_more: bool,
}

/// The locator that makes [`EvidenceCompletionJournal::record_completion`]
/// idempotent without folding the journal.
///
/// It is written BEFORE the line it points at, and it is believed only when the
/// line at that seq carries the same decision id *and the same family*. A crash
/// between the two leaves a locator whose seq holds someone else's entry, or no
/// entry at all — both of which read as "not journalled yet", so the retry mints
/// a fresh seq and rewrites the locator. The line is the sole authority; this is
/// a hint, and it is addressed per family so one family's hint can never
/// overwrite the other's for a shared id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionLocator {
    schema_version: u32,
    decision_id: String,
    seq: u64,
}

/// The scope's completion journal.
#[derive(Debug, Clone)]
pub struct EvidenceCompletionJournal {
    workspace_layout: ArtifactV2Workspace,
}

impl EvidenceCompletionJournal {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// The journal deliberately applies **no scope refusal of its own**.
    ///
    /// `scope_root` already normalises both segments, so containment is not in
    /// question — and a stricter check here would be worse than useless: it
    /// would refuse a scope whose register had just accepted the decision,
    /// leaving a completed decision that no retry can ever journal and no
    /// cursor can ever carry. The journal must address exactly the scope
    /// directory the register wrote to, which is what sharing this resolver
    /// guarantees.
    fn root(&self, scope: &EvidenceDecisionScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("decision_journal")
    }

    fn segments_dir(&self, scope: &EvidenceDecisionScope) -> PathBuf {
        self.root(scope).join("segments")
    }

    fn segment_path(&self, scope: &EvidenceDecisionScope, segment: u64) -> PathBuf {
        self.segments_dir(scope).join(format!(
            "{segment:0width$}.jsonl",
            width = SEGMENT_NAME_WIDTH
        ))
    }

    fn locator_path(
        &self,
        scope: &EvidenceDecisionScope,
        family: DecisionFamily,
        decision_id: &str,
    ) -> PathBuf {
        self.root(scope)
            .join("locators")
            .join(family.as_str())
            .join(format!("{}.json", stable_id(decision_id)))
    }

    /// Journal one completed decision, idempotently by `(family, decision id)`.
    ///
    /// Call this AFTER the authoritative receipt is durable, on both the
    /// first-write and the replay branch of a decision path — see the module
    /// note on write ordering. Replaying returns the entry that already exists;
    /// reusing a decision id for a *different* completion **of the same family**
    /// fails closed, while the same id in the other family is a second decision
    /// because that is what the registers already treat it as.
    pub fn record_completion(
        &self,
        scope: &EvidenceDecisionScope,
        completion: &EvidenceDecisionCompletion,
        now: DateTime<Utc>,
    ) -> Result<EvidenceCompletionEntry> {
        validate_completion(completion)?;
        let family = completion.target.family();
        let root = self.root(scope);
        let _journal_guard =
            acquire_record_decision_lock(&root, "journal", "evidence completion journal")?;

        if let Some(existing) = self.journalled_entry(scope, family, &completion.decision_id)? {
            validate_entry_matches(&existing, completion)?;
            return Ok(existing);
        }

        let seq = self
            .head_seq(scope)?
            .checked_add(1)
            .context("completion journal sequence overflow")?;
        let entry = EvidenceCompletionEntry {
            schema_version: COMPLETION_JOURNAL_SCHEMA,
            seq,
            decision_id: completion.decision_id.clone(),
            target: completion.target.clone(),
            receipt_id: completion.receipt_id.clone(),
            request_fingerprint: completion.request_fingerprint.clone(),
            completed_at: completion.completed_at,
            journaled_at: now,
        };
        let line = serde_json::to_vec(&entry)?;
        if line.len() > MAX_JOURNAL_ENTRY_BYTES {
            anyhow::bail!("completion journal entry exceeds its bounded size ceiling");
        }

        // Locator first, line second. The reverse order would leave an
        // unreferenced line after a crash, and finding it again would mean the
        // scope-wide fold this journal replaces.
        self.write_locator(
            scope,
            family,
            &CompletionLocator {
                schema_version: COMPLETION_JOURNAL_SCHEMA,
                decision_id: completion.decision_id.clone(),
                seq,
            },
        )?;
        jsonl::append_log_line(
            &self.workspace_layout,
            &self.segment_path(scope, segment_of(seq)),
            &line,
        )?;
        Ok(entry)
    }

    /// The entries strictly after `cursor`, at most `limit` of them.
    ///
    /// Fails closed on a gap, on a cursor past the head, and on a journal
    /// directory holding anything but its own segments. A page that silently
    /// skipped a seq would be indistinguishable from a page that ended.
    pub fn page_after(
        &self,
        scope: &EvidenceDecisionScope,
        cursor: EvidenceCompletionCursor,
        limit: usize,
    ) -> Result<EvidenceCompletionPage> {
        if limit == 0 || limit > MAX_COMPLETION_PAGE_ENTRIES {
            anyhow::bail!(
                "completion journal page size must be between 1 and {MAX_COMPLETION_PAGE_ENTRIES}"
            );
        }
        let head = self.head_seq(scope)?;
        if cursor.0 > head {
            // The journal only ever grows, so a cursor beyond the head means
            // the journal was truncated or replaced under a live reader.
            // Restarting from the head would skip everything in between and
            // restarting from zero would replay it; neither is a decision this
            // layer may take silently.
            anyhow::bail!(
                "completion cursor {} is ahead of journal head {head}; the journal it was taken \
                 from is not the journal being read",
                cursor.0
            );
        }
        if cursor.0 == head {
            return Ok(EvidenceCompletionPage {
                entries: Vec::new(),
                next_cursor: cursor,
                has_more: false,
            });
        }

        let mut expected = cursor.0 + 1;
        let mut entries = Vec::with_capacity(limit.min((head - cursor.0) as usize));
        let mut segment = segment_of(expected);
        while entries.len() < limit && expected <= head {
            let held = self.read_segment(scope, segment)?;
            let base = segment * JOURNAL_SEGMENT_SPAN;
            for entry in held {
                if entry.seq < expected {
                    continue;
                }
                if entry.seq != expected {
                    anyhow::bail!(
                        "completion journal skips seq {expected}; refusing to page across a gap"
                    );
                }
                expected += 1;
                entries.push(entry);
                if entries.len() == limit {
                    break;
                }
            }
            if entries.len() == limit {
                break;
            }
            // Only a full segment may be followed by another one, so a short
            // interior segment is a hole rather than the end of the journal.
            if expected <= head && expected <= base + JOURNAL_SEGMENT_SPAN {
                anyhow::bail!(
                    "completion journal segment {segment} ends before seq {expected} but the \
                     journal claims head {head}; refusing to page across a gap"
                );
            }
            segment += 1;
        }

        let next_cursor = entries
            .last()
            .map_or(cursor, |entry| EvidenceCompletionCursor(entry.seq));
        Ok(EvidenceCompletionPage {
            has_more: next_cursor.0 < head,
            next_cursor,
            entries,
        })
    }

    /// The highest journalled seq, or the START cursor when nothing has
    /// completed in this scope.
    pub fn head(&self, scope: &EvidenceDecisionScope) -> Result<EvidenceCompletionCursor> {
        Ok(EvidenceCompletionCursor(self.head_seq(scope)?))
    }

    fn head_seq(&self, scope: &EvidenceDecisionScope) -> Result<u64> {
        let segments = self.segment_indexes(scope)?;
        let Some(&last) = segments.last() else {
            return Ok(0);
        };
        let held = self.read_segment(scope, last)?;
        if !held.is_empty() {
            return Ok(last * JOURNAL_SEGMENT_SPAN + held.len() as u64);
        }
        if last == 0 {
            return Ok(0);
        }
        // A segment file exists only because an append targeted it, and an
        // append targets segment k only once k-1 holds a full span. An empty
        // last segment is a torn first append into it — legitimate — but only
        // if its predecessor really is full. Otherwise the head this would
        // report is a seq nobody ever wrote.
        let previous = self.read_segment(scope, last - 1)?;
        if previous.len() as u64 != JOURNAL_SEGMENT_SPAN {
            anyhow::bail!(
                "completion journal segment {} is short but segment {last} exists; refusing to \
                 report a head across a gap",
                last - 1
            );
        }
        Ok(last * JOURNAL_SEGMENT_SPAN)
    }

    /// The entry a locator points at, when the locator is telling the truth.
    fn journalled_entry(
        &self,
        scope: &EvidenceDecisionScope,
        family: DecisionFamily,
        decision_id: &str,
    ) -> Result<Option<EvidenceCompletionEntry>> {
        let Some(locator) = self.read_locator(scope, family, decision_id)? else {
            return Ok(None);
        };
        let segment = segment_of(locator.seq);
        let held = self.read_segment(scope, segment)?;
        let position = (locator.seq - segment * JOURNAL_SEGMENT_SPAN - 1) as usize;
        // Every miss means the same thing — the line this locator was written
        // for never landed — and all are healed by minting a fresh seq. The
        // locator is never permitted to contradict the log.
        let Some(entry) = held.get(position) else {
            return Ok(None);
        };
        // The family is checked against the *log*, not against the locator's
        // own directory, because the directory only proves who wrote the hint.
        // The seq it names can meanwhile have been taken by the other family's
        // decision carrying the same client-supplied id, and believing that
        // entry would mean answering a claim's replay with a commitment.
        if entry.seq != locator.seq
            || entry.decision_id != decision_id
            || entry.target.family() != family
        {
            return Ok(None);
        }
        Ok(Some(entry.clone()))
    }

    fn read_locator(
        &self,
        scope: &EvidenceDecisionScope,
        family: DecisionFamily,
        decision_id: &str,
    ) -> Result<Option<CompletionLocator>> {
        let path = self.locator_path(scope, family, decision_id);
        let Some(metadata) = self.workspace_layout.metadata_path_sync(&path)? else {
            return Ok(None);
        };
        if metadata.len() > MAX_LOCATOR_BYTES {
            anyhow::bail!("completion journal locator exceeds its bounded size ceiling");
        }
        let locator: CompletionLocator = self.workspace_layout.read_json_path_sync(&path)?;
        if locator.schema_version != COMPLETION_JOURNAL_SCHEMA
            || locator.decision_id != decision_id
            || locator.seq == 0
        {
            anyhow::bail!("completion journal locator identity is corrupt");
        }
        Ok(Some(locator))
    }

    fn write_locator(
        &self,
        scope: &EvidenceDecisionScope,
        family: DecisionFamily,
        locator: &CompletionLocator,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(locator)?;
        if bytes.len() as u64 > MAX_LOCATOR_BYTES {
            anyhow::bail!("completion journal locator exceeds its bounded size ceiling");
        }
        self.workspace_layout
            .write_atomic_path_sync(
                self.locator_path(scope, family, &locator.decision_id),
                &bytes,
            )
            .context("persisting completion journal locator")?;
        Ok(())
    }

    /// One segment, validated to be exactly the dense block it claims to be.
    ///
    /// Every read goes through here, so "seq `n` is at position
    /// `(n - 1) % SPAN` of segment `(n - 1) / SPAN`" is checked rather than
    /// assumed — which is what lets the locator lookup index straight into the
    /// block instead of searching it.
    fn read_segment(
        &self,
        scope: &EvidenceDecisionScope,
        segment: u64,
    ) -> Result<Vec<EvidenceCompletionEntry>> {
        let path = self.segment_path(scope, segment);
        let Some(raw) = jsonl::read_log_if_present(&self.workspace_layout, &path)? else {
            return Ok(Vec::new());
        };
        let held: Vec<EvidenceCompletionEntry> = jsonl::parse_log_lines(&raw, &path)?;
        if held.len() as u64 > JOURNAL_SEGMENT_SPAN {
            anyhow::bail!(
                "completion journal segment {segment} holds more than its {JOURNAL_SEGMENT_SPAN} \
                 entries"
            );
        }
        let base = segment * JOURNAL_SEGMENT_SPAN;
        for (position, entry) in held.iter().enumerate() {
            if entry.schema_version != COMPLETION_JOURNAL_SCHEMA {
                anyhow::bail!("completion journal entry declares an unknown schema version");
            }
            if entry.seq != base + position as u64 + 1 {
                anyhow::bail!(
                    "completion journal segment {segment} is not dense at position {position}; \
                     refusing to read a journal whose sequence is not monotonic"
                );
            }
            entry.target.validate().with_context(|| {
                format!(
                    "completion journal entry {} names an invalid target",
                    entry.seq
                )
            })?;
        }
        Ok(held)
    }

    fn segment_indexes(&self, scope: &EvidenceDecisionScope) -> Result<Vec<u64>> {
        let dir = self.segments_dir(scope);
        let paths = jsonl::list_log_paths(&self.workspace_layout, &dir)?;
        let mut indexes = Vec::with_capacity(paths.len());
        for path in &paths {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("completion journal segment name is unreadable")?;
            if stem.len() != SEGMENT_NAME_WIDTH || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
                anyhow::bail!(
                    "`{stem}` is not a completion journal segment; refusing to page a journal \
                     directory that holds something else"
                );
            }
            indexes.push(
                stem.parse::<u64>()
                    .context("completion journal segment index")?,
            );
        }
        // `list_log_paths` sorts by name and the width is fixed, so lexical
        // order is numeric order. Contiguity from zero is what makes "segment
        // k holds seqs k*SPAN+1..=(k+1)*SPAN" true for every k, and that
        // identity is the addressing scheme.
        for (position, index) in indexes.iter().enumerate() {
            if *index != position as u64 {
                anyhow::bail!(
                    "completion journal is missing segment {position}; refusing to page across a \
                     gap"
                );
            }
        }
        Ok(indexes)
    }
}

fn segment_of(seq: u64) -> u64 {
    debug_assert!(seq >= 1, "journal sequence numbers start at one");
    seq.saturating_sub(1) / JOURNAL_SEGMENT_SPAN
}

fn validate_completion(completion: &EvidenceDecisionCompletion) -> Result<()> {
    if completion.decision_id.trim().is_empty()
        || completion.decision_id.len() > MAX_DECISION_ID_BYTES
        || !is_safe_scope_id(&completion.decision_id)
    {
        anyhow::bail!(
            "journalled decision id must be a non-empty safe identifier no longer than \
             {MAX_DECISION_ID_BYTES} bytes"
        );
    }
    if completion.receipt_id.trim().is_empty()
        || completion.receipt_id.len() > MAX_DECISION_ID_BYTES
        || completion.request_fingerprint.trim().is_empty()
        || completion.request_fingerprint.len() > MAX_DECISION_ID_BYTES
    {
        anyhow::bail!(
            "a journalled completion must carry the receipt id and request fingerprint that bind \
             it to its authoritative record"
        );
    }
    completion.target.validate()
}

/// A decision id identifies one completion *within its register*. A second,
/// different completion under the same id and family is a substitution, which
/// that register already refuses at its own boundary and which the journal
/// refuses again rather than projecting whichever copy it happens to hold.
/// Across families the id is two different keys, so this never sees the pair.
fn validate_entry_matches(
    held: &EvidenceCompletionEntry,
    completion: &EvidenceDecisionCompletion,
) -> Result<()> {
    if held.decision_id != completion.decision_id
        || held.target != completion.target
        || held.receipt_id != completion.receipt_id
        || held.request_fingerprint != completion.request_fingerprint
        || held.completed_at != completion.completed_at
    {
        anyhow::bail!(
            "decision `{}` is already journalled for a different completion",
            completion.decision_id
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn journal() -> (
        tempfile::TempDir,
        EvidenceCompletionJournal,
        EvidenceDecisionScope,
    ) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let journal = EvidenceCompletionJournal::new(ArtifactV2Workspace::new(tmp.path()));
        (
            tmp,
            journal,
            EvidenceDecisionScope::new("anonymous", "default"),
        )
    }

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 10, minute, 0).unwrap()
    }

    fn completion(decision_id: &str) -> EvidenceDecisionCompletion {
        EvidenceDecisionCompletion {
            decision_id: decision_id.to_string(),
            target: EvidenceDecisionTarget::TranscriptClaim {
                claim_id: format!("claim-{decision_id}"),
            },
            receipt_id: format!("receipt-{decision_id}"),
            request_fingerprint: format!("fingerprint-{decision_id}"),
            completed_at: at(0),
        }
    }

    /// The same decision id in the other register. Both families mint their own
    /// ids and a client picks them, so this pairing is ordinary, not exotic.
    fn commitment_completion(decision_id: &str) -> EvidenceDecisionCompletion {
        EvidenceDecisionCompletion {
            decision_id: decision_id.to_string(),
            target: EvidenceDecisionTarget::Commitment {
                audience: AudienceRef::engagement("engagement-1"),
                commitment_id: format!("cmt-{decision_id}"),
            },
            receipt_id: format!("commitment-receipt-{decision_id}"),
            request_fingerprint: format!("commitment-fingerprint-{decision_id}"),
            completed_at: at(0),
        }
    }

    #[test]
    fn an_empty_journal_pages_to_nothing_rather_than_failing() {
        let (_tmp, journal, scope) = journal();
        let page = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("page an empty journal");
        assert!(page.entries.is_empty());
        assert_eq!(page.next_cursor, EvidenceCompletionCursor::START);
        assert!(!page.has_more);
        assert_eq!(journal.head(&scope).expect("head").seq(), 0);
    }

    /// The reopen's own hazard, as a test. Decision ids are hashed to name
    /// their index files, so a directory walk returns them in hash order —
    /// which is unrelated to the order they completed in, and is why a new
    /// decision could sort behind a live cursor and be lost. The journal's
    /// order is assigned at append time instead.
    #[test]
    fn journal_order_is_completion_order_and_not_hash_order() {
        let (_tmp, journal, scope) = journal();
        let appended: Vec<String> = (1..=8).map(|n| format!("decision-{n}")).collect();
        for (index, decision_id) in appended.iter().enumerate() {
            let entry = journal
                .record_completion(&scope, &completion(decision_id), at(index as u32))
                .expect("journal a completion");
            assert_eq!(entry.seq, index as u64 + 1, "seq is dense from one");
        }

        let mut by_hash = appended.clone();
        by_hash.sort_by_key(|decision_id| stable_id(decision_id));
        assert_ne!(
            by_hash, appended,
            "the fixture is only meaningful while hash order really does disagree with \
             completion order; blake3 is fixed, so this is deterministic, not flaky"
        );

        let page = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("page");
        let paged: Vec<String> = page
            .entries
            .iter()
            .map(|entry| entry.decision_id.clone())
            .collect();
        assert_eq!(paged, appended, "the cursor reads completion order");
        assert_eq!(page.next_cursor.seq(), 8);
        assert!(!page.has_more);
    }

    /// The second half of the same hazard: a decision journalled after a
    /// reader has already paged still lands ahead of that reader's cursor.
    #[test]
    fn a_later_completion_lands_ahead_of_a_live_cursor() {
        let (_tmp, journal, scope) = journal();
        journal
            .record_completion(&scope, &completion("decision-first"), at(0))
            .expect("first");
        let first = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("first page");
        assert_eq!(first.entries.len(), 1);

        journal
            .record_completion(&scope, &completion("decision-second"), at(1))
            .expect("second");
        let second = journal
            .page_after(&scope, first.next_cursor, 10)
            .expect("second page");
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].decision_id, "decision-second");
        assert_eq!(second.entries[0].seq, 2);
    }

    /// The repair the write-ordering note promises, read from the consumer's
    /// side. `decision-lost` completed first and its journal append never
    /// landed; a projector then drained everything the journal did hold. The
    /// repair's seq is minted at append rather than derived from when the
    /// register accepted the decision, and that is the only reason it lands
    /// ahead of the drained cursor instead of behind it, where a
    /// completion-time order would have put it and where nothing would ever
    /// read it again.
    #[test]
    fn a_repaired_completion_lands_ahead_of_a_cursor_that_already_passed_it() {
        let (_tmp, journal, scope) = journal();
        let mut lost = completion("decision-lost");
        lost.completed_at = at(0);
        let mut later = completion("decision-later");
        later.completed_at = at(1);
        journal
            .record_completion(&scope, &later, at(1))
            .expect("the decision whose journal write did land");
        let drained = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("a projector drains the journal");
        assert_eq!(drained.next_cursor.seq(), 1);
        assert!(!drained.has_more);

        let repaired = journal
            .record_completion(&scope, &lost, at(9))
            .expect("the interrupted decision's retry repairs its entry");
        assert_eq!(
            repaired.seq, 2,
            "the seq is minted at append, not at completion"
        );
        assert_eq!(
            repaired.completed_at,
            at(0),
            "the entry still reports when the register accepted the decision"
        );
        assert_eq!(repaired.journaled_at, at(9), "and when the repair landed");

        let resumed = journal
            .page_after(&scope, drained.next_cursor, 10)
            .expect("resume from the drained cursor");
        assert_eq!(
            resumed
                .entries
                .iter()
                .map(|entry| entry.decision_id.as_str())
                .collect::<Vec<_>>(),
            vec!["decision-lost"],
            "a repaired completion reaches a reader that had already passed the position its \
             completion time would have given it"
        );
    }

    #[test]
    fn a_page_is_bounded_and_resumes_exactly_where_it_stopped() {
        let (_tmp, journal, scope) = journal();
        for n in 1..=5 {
            journal
                .record_completion(&scope, &completion(&format!("decision-{n}")), at(n))
                .expect("journal");
        }
        let first = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 2)
            .expect("bounded page");
        assert_eq!(first.entries.len(), 2);
        assert_eq!(first.next_cursor.seq(), 2);
        assert!(first.has_more);

        let second = journal
            .page_after(&scope, first.next_cursor, 2)
            .expect("page");
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        let third = journal
            .page_after(&scope, second.next_cursor, 2)
            .expect("page");
        assert_eq!(third.entries.len(), 1);
        assert!(!third.has_more);
    }

    /// Segment addressing has to be invisible: a cursor that stops mid-segment
    /// and resumes across the boundary reads a dense run.
    #[test]
    fn the_sequence_stays_dense_across_a_segment_boundary() {
        let (_tmp, journal, scope) = journal();
        let total = JOURNAL_SEGMENT_SPAN + 3;
        for n in 1..=total {
            journal
                .record_completion(&scope, &completion(&format!("decision-{n}")), at(0))
                .expect("journal");
        }
        assert_eq!(journal.head(&scope).expect("head").seq(), total);

        let page = journal
            .page_after(
                &scope,
                EvidenceCompletionCursor::consumed_through(JOURNAL_SEGMENT_SPAN - 1),
                MAX_COMPLETION_PAGE_ENTRIES,
            )
            .expect("page across the boundary");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![
                JOURNAL_SEGMENT_SPAN,
                JOURNAL_SEGMENT_SPAN + 1,
                JOURNAL_SEGMENT_SPAN + 2,
                JOURNAL_SEGMENT_SPAN + 3,
            ]
        );
        assert_eq!(page.next_cursor.seq(), total);
        assert!(!page.has_more);
    }

    #[test]
    fn journalling_the_same_decision_twice_is_one_entry() {
        let (_tmp, journal, scope) = journal();
        let first = journal
            .record_completion(&scope, &completion("decision-1"), at(0))
            .expect("first");
        let replay = journal
            .record_completion(&scope, &completion("decision-1"), at(5))
            .expect("replay");
        assert_eq!(first, replay, "a replay returns the entry that exists");
        assert_eq!(journal.head(&scope).expect("head").seq(), 1);
    }

    #[test]
    fn reusing_a_decision_id_for_a_different_completion_fails_closed() {
        let (_tmp, journal, scope) = journal();
        journal
            .record_completion(&scope, &completion("decision-1"), at(0))
            .expect("first");
        let mut substituted = completion("decision-1");
        substituted.receipt_id = "receipt-somebody-else".to_string();
        let error = journal
            .record_completion(&scope, &substituted, at(1))
            .expect_err("a substituted completion must fail closed");
        assert!(error.to_string().contains("different completion"));
    }

    /// The two registers namespace decision ids independently — a client is
    /// free to reuse one idempotency key across them, and neither register can
    /// see the other's. Keying the journal on the bare id made the second
    /// decision fail *after* its authoritative row was durable, and fail the
    /// same way on every retry, so the register and the API disagreed forever.
    #[test]
    fn one_decision_id_in_both_registers_is_two_completions() {
        let (_tmp, journal, scope) = journal();
        let claim = journal
            .record_completion(&scope, &completion("d1"), at(0))
            .expect("the claims register journals `d1`");
        let commitment = journal
            .record_completion(&scope, &commitment_completion("d1"), at(1))
            .expect("the commitment register's own `d1` is a second decision, not a collision");
        assert_eq!((claim.seq, commitment.seq), (1, 2));

        let page = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("page");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| &entry.target)
                .collect::<Vec<_>>(),
            vec![&claim.target, &commitment.target],
            "the projector sees both completions, each pointing at its own register"
        );

        // And each stays independently idempotent afterwards.
        assert_eq!(
            journal
                .record_completion(&scope, &completion("d1"), at(2))
                .expect("claim replay"),
            claim
        );
        assert_eq!(
            journal
                .record_completion(&scope, &commitment_completion("d1"), at(3))
                .expect("commitment replay"),
            commitment
        );
        assert_eq!(journal.head(&scope).expect("head").seq(), 2);
    }

    /// The crash window between the locator write and the line append. The
    /// locator points at a seq that never landed, so the retry mints a fresh
    /// one instead of trusting the hint — the completion is journalled once,
    /// not lost and not doubled.
    #[test]
    fn a_locator_whose_line_never_landed_heals_on_retry() {
        let (_tmp, journal, scope) = journal();
        journal
            .write_locator(
                &scope,
                DecisionFamily::TranscriptClaim,
                &CompletionLocator {
                    schema_version: COMPLETION_JOURNAL_SCHEMA,
                    decision_id: "decision-interrupted".to_string(),
                    seq: 1,
                },
            )
            .expect("stage an interrupted locator");
        assert_eq!(journal.head(&scope).expect("head").seq(), 0);

        let entry = journal
            .record_completion(&scope, &completion("decision-interrupted"), at(1))
            .expect("retry heals");
        assert_eq!(entry.seq, 1);
        let page = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect("page");
        assert_eq!(page.entries.len(), 1, "healing must not double-journal");
    }

    /// The same window, but another decision took the seq in between. The
    /// locator now points at somebody else's entry, which is still "not
    /// journalled yet" for its own decision.
    #[test]
    fn a_locator_pointing_at_another_decisions_entry_is_not_believed() {
        let (_tmp, journal, scope) = journal();
        journal
            .write_locator(
                &scope,
                DecisionFamily::TranscriptClaim,
                &CompletionLocator {
                    schema_version: COMPLETION_JOURNAL_SCHEMA,
                    decision_id: "decision-interrupted".to_string(),
                    seq: 1,
                },
            )
            .expect("stage an interrupted locator");
        journal
            .record_completion(&scope, &completion("decision-winner"), at(0))
            .expect("another decision takes seq 1");

        let entry = journal
            .record_completion(&scope, &completion("decision-interrupted"), at(1))
            .expect("retry mints a fresh seq");
        assert_eq!(entry.seq, 2);
        assert_eq!(journal.head(&scope).expect("head").seq(), 2);
    }

    /// The nastiest arrangement of the same window: the seq an interrupted
    /// claim reserved was taken by the *other* register's decision carrying the
    /// same client-supplied id. Per-family locator directories alone would not
    /// catch this — the hint is in the right directory and names the right id —
    /// so the family is checked against the log, which is the authority.
    #[test]
    fn a_locator_whose_seq_was_taken_by_the_other_family_is_not_believed() {
        let (_tmp, journal, scope) = journal();
        journal
            .write_locator(
                &scope,
                DecisionFamily::TranscriptClaim,
                &CompletionLocator {
                    schema_version: COMPLETION_JOURNAL_SCHEMA,
                    decision_id: "d1".to_string(),
                    seq: 1,
                },
            )
            .expect("stage a claim decision interrupted between locator and line");
        journal
            .record_completion(&scope, &commitment_completion("d1"), at(0))
            .expect("the commitment register's `d1` takes seq 1");

        let healed = journal
            .record_completion(&scope, &completion("d1"), at(1))
            .expect("the claim retry heals rather than inheriting a commitment");
        assert_eq!(healed.seq, 2);
        assert_eq!(
            healed.target,
            EvidenceDecisionTarget::TranscriptClaim {
                claim_id: "claim-d1".to_string(),
            }
        );
    }

    #[test]
    fn a_gap_in_the_sequence_fails_closed() {
        let (_tmp, journal, scope) = journal();
        journal
            .record_completion(&scope, &completion("decision-1"), at(0))
            .expect("journal");

        // Seq three, written where seq two belongs: a page that tolerated this
        // would report the journal complete while a completion was missing.
        let skipped = EvidenceCompletionEntry {
            schema_version: COMPLETION_JOURNAL_SCHEMA,
            seq: 3,
            decision_id: "decision-3".to_string(),
            target: EvidenceDecisionTarget::TranscriptClaim {
                claim_id: "claim-3".to_string(),
            },
            receipt_id: "receipt-3".to_string(),
            request_fingerprint: "fingerprint-3".to_string(),
            completed_at: at(0),
            journaled_at: at(0),
        };
        let mut line = serde_json::to_vec(&skipped).expect("encode");
        line.push(b'\n');
        jsonl::append_log_line(
            &journal.workspace_layout,
            &journal.segment_path(&scope, 0),
            &line,
        )
        .expect("append a skipped seq");

        let error = journal
            .page_after(&scope, EvidenceCompletionCursor::START, 10)
            .expect_err("a gap must fail closed");
        assert!(error.to_string().contains("monotonic"));
    }

    #[test]
    fn a_cursor_ahead_of_the_head_fails_closed() {
        let (_tmp, journal, scope) = journal();
        journal
            .record_completion(&scope, &completion("decision-1"), at(0))
            .expect("journal");
        let error = journal
            .page_after(&scope, EvidenceCompletionCursor::consumed_through(9), 10)
            .expect_err("a cursor past the head must fail closed");
        assert!(error.to_string().contains("ahead of journal head"));
    }

    #[test]
    fn a_missing_segment_fails_closed() {
        let (_tmp, journal, scope) = journal();
        let entry = EvidenceCompletionEntry {
            schema_version: COMPLETION_JOURNAL_SCHEMA,
            seq: JOURNAL_SEGMENT_SPAN + 1,
            decision_id: "decision-orphan".to_string(),
            target: EvidenceDecisionTarget::TranscriptClaim {
                claim_id: "claim-orphan".to_string(),
            },
            receipt_id: "receipt-orphan".to_string(),
            request_fingerprint: "fingerprint-orphan".to_string(),
            completed_at: at(0),
            journaled_at: at(0),
        };
        let mut line = serde_json::to_vec(&entry).expect("encode");
        line.push(b'\n');
        jsonl::append_log_line(
            &journal.workspace_layout,
            &journal.segment_path(&scope, 1),
            &line,
        )
        .expect("append into segment one with no segment zero");

        let error = journal
            .head(&scope)
            .expect_err("a journal missing segment zero must fail closed");
        assert!(error.to_string().contains("missing segment"));
    }

    #[test]
    fn a_page_size_outside_the_ceiling_is_refused_rather_than_clamped() {
        let (_tmp, journal, scope) = journal();
        assert!(journal
            .page_after(&scope, EvidenceCompletionCursor::START, 0)
            .is_err());
        assert!(journal
            .page_after(
                &scope,
                EvidenceCompletionCursor::START,
                MAX_COMPLETION_PAGE_ENTRIES + 1,
            )
            .is_err());
    }

    #[test]
    fn a_commitment_completion_must_name_its_shard() {
        let (_tmp, journal, scope) = journal();
        let unshardable = EvidenceDecisionCompletion {
            decision_id: "decision-1".to_string(),
            target: EvidenceDecisionTarget::Commitment {
                audience: AudienceRef::engagement("   "),
                commitment_id: "cmt-1".to_string(),
            },
            receipt_id: "receipt-1".to_string(),
            request_fingerprint: "fingerprint-1".to_string(),
            completed_at: at(0),
        };
        let error = journal
            .record_completion(&scope, &unshardable, at(0))
            .expect_err("an unaddressable receipt must fail closed");
        assert!(error.to_string().contains("audience"));
    }
}
