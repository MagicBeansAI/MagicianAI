//! Capture: turning finished sessions into taste proposals the owner approves.
//!
//! Slice 2 of `docs/archive/plans/2026-08-13-cross-session-taste-slice2-capture.md`.
//! Slice 1 injects one owner-edited note; this module is how directives get
//! *into* that note without the owner writing every one by hand.
//!
//! # The load-bearing property
//!
//! **The proposals note is write-only. Nothing in this codebase parses it.**
//!
//! All machine state lives here, in JSONL. The note is a rendered *mirror* of
//! that state, for the owner to read in their notes app. Approving a proposal
//! takes the directive text from this store, never from the note.
//!
//! That is not a stylistic choice. Slice 1 originally kept proposals in an
//! `## Inbox` section of the profile note and excluded that section at render
//! time; three rounds of hardening found three separate CommonMark leak
//! classes, because a scanner that models fenced code cannot enumerate every
//! way the format makes `##` not-a-heading. Proposals are machine-generated
//! from transcripts that may quote fetched web content, so a parser bug there
//! is an injection path into a system prompt rather than a formatting bug.
//! Keeping the machine state out of markdown entirely means there is no parse
//! to get wrong and no future leak class to discover.
//!
//! If something later needs to *read* the proposals note, that is a design
//! change owing the same adversarial review — not a refactor.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::instrument;

use crate::magician_v2::analytics::runtime_activity_layer::{KIND_BACKGROUND, WORKLOAD_SCHEDULED};

/// Where a directive is destined once approved.
///
/// Only the profile note exists as a destination in this slice. Scoped
/// candidates — preference-store entries carrying scope tags — are Slice 3's
/// seed and deliberately out of scope here, since the design leaves their tag
/// vocabulary open until it is written against real extractions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
}

/// What a proposal asks the owner to do.
///
/// Retraction is the design's conflict-surfacing rule applied to the profile:
/// a directive the recent sessions contradict is *surfaced as a question*
/// rather than silently resolved. Stated beats observed, so nothing is ever
/// removed without the owner saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    /// Add this directive to the profile.
    Add,
    /// This directive is already in the profile and recent work contradicts
    /// it — is it still true?
    Retract,
}

impl Default for ProposalKind {
    fn default() -> Self {
        // Rows written before retractions existed carry no `kind`, and every
        // one of them was an addition.
        Self::Add
    }
}

/// One candidate standing directive, awaiting the owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasteProposal {
    /// Digest of the *normalized* directive text — see [`proposal_id`]. Not a
    /// ULID: a stable content id is what makes "a rejected directive is never
    /// re-proposed" hold across sessions, because a distiller that re-derives
    /// the same directive re-derives the same id.
    pub id: String,
    /// Add or retract. Absent in rows written before retractions existed,
    /// which were all additions.
    #[serde(default)]
    pub kind: ProposalKind,
    pub directive: String,
    /// Verbatim quotes the directive was inferred from. The owner approves on
    /// the strength of these; a proposal without them is not reviewable, only
    /// rubber-stampable.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Heading in the profile note this directive belongs under.
    pub destination: String,
    pub confidence: f32,
    pub status: ProposalStatus,
    pub source_session: String,
    pub proposed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

/// Normalize a directive for identity purposes.
///
/// The balance here is load-bearing in both directions, which is why it is
/// pinned by test rather than left to intuition. Under-normalizing lets a
/// rejected directive return wearing different whitespace or capitalization,
/// which is the approval fatigue the daily cap exists to prevent.
/// Over-normalizing collides directives that genuinely differ, and the
/// consequence there is worse: a *new* directive silently suppressed because
/// something unlike it was rejected once.
///
/// So: trim, collapse internal whitespace, casefold. Deliberately **not**
/// stripped — punctuation and negation. "Never use em-dashes" and "Use
/// em-dashes" must not collide, and stripping punctuation to be tidy is how
/// that happens.
pub fn normalize_directive(directive: &str) -> String {
    directive
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Content-addressed id for a directive.
pub fn proposal_id(directive: &str) -> String {
    let normalized = normalize_directive(directive);
    blake3::hash(normalized.as_bytes()).to_hex()[..24].to_string()
}

/// Adding and retracting a directive are different owner decisions. Preserve
/// legacy addition IDs; retractions must never collide with approved additions.
pub fn proposal_id_for_kind(directive: &str, kind: ProposalKind) -> String {
    match kind {
        ProposalKind::Add => proposal_id(directive),
        ProposalKind::Retract => {
            proposal_id(&format!("retract:{}", normalize_directive(directive)))
        },
    }
}

/// Outcome of filing a batch of candidates.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FileOutcome {
    pub filed: usize,
    /// Already decided — approved or rejected — so never offered again.
    pub suppressed_decided: usize,
    /// Already pending; the same directive from a second session.
    pub suppressed_duplicate: usize,
    /// Refused because the day's cap was already met.
    pub suppressed_capped: usize,
    /// Failed work remains eligible for a later bounded retry. It must not
    /// share the successful-empty outcome that advances the watermark.
    pub retryable_failure: Option<CaptureFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFailure {
    Provider,
    Timeout,
    InvalidReply,
    SourceRead,
    Storage,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureHealth {
    pub completed: u64,
    pub empty: u64,
    pub filed: u64,
    pub failed_attempts: u64,
    pub pending_retries: usize,
    pub last_attempt_at: Option<i64>,
    pub last_failure: Option<CaptureFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CaptureRetry {
    attempts: u32,
    next_attempt_at: i64,
    failure: CaptureFailure,
}

/// Distiller health, derived from the proposal store.
///
/// This is the design's provenance-ledger role, served from the store that
/// already holds the data rather than a second file that could disagree.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct CaptureStats {
    pub pending: usize,
    pub approved: usize,
    pub rejected: usize,
    /// `approved / (approved + rejected)`, or `None` before the first
    /// decision. Absent rather than zero: a ratio over no samples reads as 0%
    /// and would condemn a distiller nobody has judged yet.
    pub accept_rate: Option<f32>,
    /// Committed lines that would not parse — real loss, surfaced because a
    /// dropped rejection silently becomes a re-proposal.
    pub corrupt_lines: usize,
}

impl CaptureStats {
    /// The design's health threshold: below roughly half, the distiller is
    /// proposing more noise than taste and the prompt or model needs revisiting.
    ///
    /// Requires a minimum sample, because two rejections out of three early
    /// decisions is not evidence of anything.
    pub fn needs_attention(&self) -> bool {
        const MIN_SAMPLE: usize = 8;
        match self.accept_rate {
            Some(rate) if self.approved + self.rejected >= MIN_SAMPLE => rate < 0.5,
            _ => false,
        }
    }
}

/// Durable store of proposals.
///
/// Whole-file atomic writes rather than the append-plus-compaction shape the
/// device audit uses: this store is read-modify-write (status changes on every
/// decision) and small by construction, since the daily cap bounds growth. An
/// append log would need compaction to answer "what is pending" and would make
/// a decision two records instead of one.
pub struct TasteProposalStore {
    path: PathBuf,
    write_lock: Mutex<()>,
}

/// A tolerant read: committed records, plus what could not be parsed.
#[derive(Debug, Default)]
pub struct ProposalRead {
    pub proposals: Vec<TasteProposal>,
    /// Bytes of a trailing partial line. Never committed, so never a loss —
    /// a crash mid-append leaves exactly this.
    pub torn_tail_bytes: usize,
    /// Interior lines that failed to parse. **This is real loss** and is
    /// surfaced rather than skipped: a store that silently drops a rejected
    /// directive will re-propose it, and the owner sees a queue that will not
    /// stay dismissed with no indication why.
    pub corrupt_lines: usize,
}

impl TasteProposalStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Read every record, tolerant of a torn tail and corrupt interior lines.
    pub async fn read(&self) -> std::io::Result<ProposalRead> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let body = match std::fs::read(&path) {
                Ok(body) => body,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(ProposalRead::default())
                },
                Err(error) => return Err(error),
            };
            Ok(parse_proposals(&body))
        })
        .await
        .map_err(|error| std::io::Error::other(format!("proposal read panicked: {error}")))?
    }

    /// Replace the file's contents durably (unique temp, fsync, rename,
    /// parent-dir sync), so a reader only ever sees whole-or-previous.
    async fn write_all(&self, proposals: &[TasteProposal]) -> std::io::Result<()> {
        let mut body = Vec::new();
        for proposal in proposals {
            // Propagated rather than `expect`ed. The only realistic failure is
            // a non-finite confidence, which serde_json cannot represent — and
            // the admission gate already refuses NaN. But that gate lives three
            // functions away, and a panic here would take down the worker
            // mid-write for a value that should simply be rejected.
            let line = serde_json::to_vec(proposal).map_err(|error| {
                std::io::Error::other(format!(
                    "taste proposal `{}` could not be serialized: {error}",
                    proposal.id
                ))
            })?;
            body.extend_from_slice(&line);
            body.push(b'\n');
        }
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&path, &body)
        })
        .await
        .map_err(|error| std::io::Error::other(format!("proposal write panicked: {error}")))?
    }

    /// File freshly distilled candidates, refusing everything that should not
    /// reach the owner.
    ///
    /// The cap counts proposals *raised within the trailing day*, not rows in
    /// the file — a store holding a month of decided proposals must not read
    /// as permanently over quota.
    pub async fn file_candidates(
        &self,
        candidates: Vec<TasteProposal>,
        max_per_day: u32,
        now: DateTime<Utc>,
    ) -> std::io::Result<FileOutcome> {
        let _guard = self.write_lock.lock().await;
        let mut existing = self.read().await?.proposals;
        let mut known: HashMap<String, ProposalStatus> =
            existing.iter().map(|p| (p.id.clone(), p.status)).collect();

        let window_start = now - Duration::days(1);
        let mut raised_today = existing
            .iter()
            .filter(|p| p.proposed_at >= window_start)
            .count() as u32;

        let mut outcome = FileOutcome::default();
        for mut candidate in candidates {
            let normalized = normalize_directive(&candidate.directive);
            let opposite = existing
                .iter()
                .filter(|p| {
                    p.kind != candidate.kind
                        && p.status == ProposalStatus::Approved
                        && normalize_directive(&p.directive) == normalized
                })
                .max_by_key(|p| p.decided_at);
            candidate.id = match opposite {
                Some(previous) => proposal_id(&format!(
                    "{:?}:{normalized}:after:{}",
                    candidate.kind, previous.id
                )),
                None => proposal_id_for_kind(&candidate.directive, candidate.kind),
            };
            // A legacy retraction may have the old addition-shaped ID. Respect
            // its decision until an opposite approved transition changes state.
            if let Some(previous) = existing
                .iter()
                .filter(|p| {
                    p.kind == candidate.kind
                        && normalize_directive(&p.directive) == normalized
                        && opposite.is_none_or(|other| {
                            p.proposed_at >= other.decided_at.unwrap_or(other.proposed_at)
                        })
                })
                .max_by_key(|p| p.proposed_at)
            {
                candidate.id = previous.id.clone();
            }
            match known.get(&candidate.id) {
                Some(ProposalStatus::Approved) | Some(ProposalStatus::Rejected) => {
                    outcome.suppressed_decided += 1;
                    continue;
                },
                Some(ProposalStatus::Pending) => {
                    outcome.suppressed_duplicate += 1;
                    continue;
                },
                None => {},
            }
            if raised_today >= max_per_day {
                outcome.suppressed_capped += 1;
                continue;
            }
            raised_today += 1;
            outcome.filed += 1;
            known.insert(candidate.id.clone(), candidate.status);
            existing.push(candidate);
        }

        if outcome.filed > 0 {
            self.write_all(&existing).await?;
        }
        Ok(outcome)
    }

    /// Pending queue and health from a **single** read.
    ///
    /// The review surface needs both on every request, and calling `pending()`
    /// then `stats()` parses the whole file twice for one answer.
    pub async fn pending_with_stats(&self) -> std::io::Result<(Vec<TasteProposal>, CaptureStats)> {
        let read = self.read().await?;
        let mut stats = CaptureStats {
            corrupt_lines: read.corrupt_lines,
            ..Default::default()
        };
        let mut pending = Vec::new();
        for proposal in read.proposals {
            match proposal.status {
                ProposalStatus::Pending => {
                    stats.pending += 1;
                    pending.push(proposal);
                },
                ProposalStatus::Approved => stats.approved += 1,
                ProposalStatus::Rejected => stats.rejected += 1,
            }
        }
        let decided = stats.approved + stats.rejected;
        if decided > 0 {
            stats.accept_rate = Some(stats.approved as f32 / decided as f32);
        }
        pending.sort_by(|a, b| b.proposed_at.cmp(&a.proposed_at));
        Ok((pending, stats))
    }

    pub async fn pending(&self) -> std::io::Result<Vec<TasteProposal>> {
        let mut pending: Vec<TasteProposal> = self
            .read()
            .await?
            .proposals
            .into_iter()
            .filter(|p| p.status == ProposalStatus::Pending)
            .collect();
        pending.sort_by(|a, b| b.proposed_at.cmp(&a.proposed_at));
        Ok(pending)
    }

    /// Record a decision. Returns the proposal as it now stands, or `None` if
    /// the id is unknown — the caller answers 404 rather than inventing one.
    ///
    /// Deciding an already-decided proposal is idempotent and keeps the
    /// *original* decision time: a second approve is a duplicate request, not
    /// a new event, and overwriting the timestamp would erase when the owner
    /// actually chose.
    pub async fn decide(
        &self,
        id: &str,
        status: ProposalStatus,
        now: DateTime<Utc>,
    ) -> std::io::Result<Option<TasteProposal>> {
        let _guard = self.write_lock.lock().await;
        let mut proposals = self.read().await?.proposals;
        let Some(index) = proposals.iter().position(|p| p.id == id) else {
            return Ok(None);
        };
        if proposals[index].status == ProposalStatus::Pending {
            proposals[index].status = status;
            proposals[index].decided_at = Some(now);
            self.write_all(&proposals).await?;
        }
        Ok(Some(proposals[index].clone()))
    }
}

/// Split a JSONL body into committed records, counting what was lost.
fn parse_proposals(body: &[u8]) -> ProposalRead {
    let text = String::from_utf8_lossy(body);
    // A trailing fragment is only a fragment when the file does not end on a
    // record boundary; an empty file ends on one vacuously.
    let (committed, torn_tail_bytes) = match text.rfind('\n') {
        Some(last) if last + 1 < text.len() => (&text[..=last], text.len() - last - 1),
        Some(last) => (&text[..=last], 0),
        None if text.is_empty() => ("", 0),
        None => ("", text.len()),
    };
    let mut read = ProposalRead {
        torn_tail_bytes,
        ..Default::default()
    };
    for line in committed.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TasteProposal>(line) {
            Ok(proposal) => read.proposals.push(proposal),
            Err(_) => read.corrupt_lines += 1,
        }
    }
    read
}

/// Operation name the distiller model binds to in
/// `llm.router.operation_mapping`.
///
/// Its own operation rather than sharing the channel distiller's: the two have
/// different privacy surfaces and an owner must be able to enable one without
/// the other.
pub const TASTE_PROFILE_DISTILL_OPERATION: &str = "taste_profile_distill";

/// Defensive ceilings on what one distillation may produce.
///
/// A local model that loops or degrades must not be able to write an unbounded
/// blob into the owner's inbox note. These bound the blast radius of a bad
/// model, not the quality of a good one — the prompt already asks for less
/// than this.
const MAX_DIRECTIVE_CHARS: usize = 400;
const MAX_EVIDENCE_QUOTES: usize = 4;
const MAX_EVIDENCE_CHARS: usize = 600;

fn capture_scope_key(principal: &str, workspace: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [principal, workspace] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().to_hex()[..24].to_string()
}

/// Resolve the distiller's model binding.
///
/// **Unbound means OFF.** The router's `default_profile` is deliberately not
/// consulted, so enabling capture is a deliberate second act rather than
/// something that silently inherits whichever model is the default.
///
/// **The binding is NOT required to be local, and that is a considered
/// reversal.** The first version refused any non-Ollama provider, copying the
/// channel distiller's locality guard. That guard is right for channel ingest
/// and wrong here, for a reason easy to miss: locality protects content that
/// has *not already been sent to a model*. Email bodies qualify — they arrive
/// from the provider and reach no LLM unless the distiller reads them. A chat
/// transcript does not. It went to whichever model conducted the session,
/// turn by turn, long before any distillation existed; in the shipped config
/// that is a remote OpenAI profile. Refusing a remote distiller here
/// protected nothing and only denied the owner a capable model for the job
/// where quality matters most.
///
/// What stays the owner's call, and belongs in docs rather than code: binding
/// this to a *different vendor* than the one conducting sessions is a
/// genuinely new disclosure. That is a decision to make knowingly, not an
/// invariant to hardcode.
pub fn resolve_taste_distiller(
    router: Option<&crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
) -> Result<
    crate::magician_v2::llm_dispatch_seam::VerifiedLocalBinding,
    crate::magician_v2::llm_dispatch_seam::DistillUnavailable,
> {
    crate::magician_v2::llm_dispatch_seam::resolve_bound_provider_for_operation(
        router,
        TASTE_PROFILE_DISTILL_OPERATION,
    )
}

/// One candidate exactly as the model returned it, before any policy applies.
#[derive(Debug, Clone, Deserialize)]
pub struct DistilledCandidate {
    /// "add" or "retract"; anything else — including absent — reads as an
    /// addition. A model that invents a third verb must not be able to
    /// produce a proposal whose meaning nobody defined.
    #[serde(default)]
    pub kind: String,
    pub directive: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub confidence: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct DistillEnvelope {
    candidates: Vec<DistilledCandidate>,
}

/// Why a candidate never became a proposal.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdmissionCounts {
    pub admitted: usize,
    pub below_confidence: usize,
    pub no_evidence: usize,
    pub over_length: usize,
    pub empty_directive: usize,
    pub invalid_reply: bool,
}

/// Parse the model's reply and admit only candidates that pass policy.
///
/// Malformed output is explicitly distinguished from a valid empty extraction
/// so the worker can retry without pretending the session was processed.
pub fn admit_candidates(
    raw: &str,
    min_confidence: f32,
    max_candidates: usize,
) -> (Vec<DistilledCandidate>, AdmissionCounts) {
    let mut counts = AdmissionCounts::default();
    let Ok(envelope) = serde_json::from_str::<DistillEnvelope>(raw.trim()) else {
        counts.invalid_reply = true;
        return (Vec::new(), counts);
    };

    let mut admitted = Vec::new();
    for candidate in envelope.candidates {
        if admitted.len() >= max_candidates {
            break;
        }
        let directive = candidate.directive.trim().to_string();
        if directive.is_empty() {
            counts.empty_directive += 1;
            continue;
        }
        // Refused, never truncated — the same rule injection follows. A
        // directive cut mid-sentence can invert its own meaning, and "never
        // delete files without asking" truncated at the wrong word is an
        // instruction to delete files.
        if directive.chars().count() > MAX_DIRECTIVE_CHARS {
            counts.over_length += 1;
            continue;
        }
        // Evidence is what the owner reviews against. Without it a proposal is
        // not reviewable, only rubber-stampable, so it is refused rather than
        // shown bare.
        let evidence: Vec<String> = candidate
            .evidence
            .iter()
            .map(|quote| quote.trim())
            .filter(|quote| !quote.is_empty())
            .take(MAX_EVIDENCE_QUOTES)
            .map(|quote| quote.chars().take(MAX_EVIDENCE_CHARS).collect::<String>())
            .collect();
        if evidence.is_empty() {
            counts.no_evidence += 1;
            continue;
        }
        if !(candidate.confidence >= min_confidence) {
            // Written as `!(>=)` deliberately: a NaN confidence must fail the
            // gate, and `<` would let it through.
            counts.below_confidence += 1;
            continue;
        }
        counts.admitted += 1;
        admitted.push(DistilledCandidate {
            directive,
            evidence,
            ..candidate
        });
    }
    (admitted, counts)
}

/// Turn admitted candidates into storable proposals.
pub fn into_proposals(
    candidates: Vec<DistilledCandidate>,
    allowed_sections: &[String],
    source_session: &str,
    now: DateTime<Utc>,
) -> Vec<TasteProposal> {
    candidates
        .into_iter()
        .map(|candidate| {
            // A destination the profile does not have would place the
            // directive in a section the owner never made. Falling back to the
            // first allowed section keeps the proposal reviewable; the owner
            // can move it, and a visibly-misfiled directive is recoverable
            // where an invented heading is clutter they must clean up.
            let destination = allowed_sections
                .iter()
                .find(|section| section.eq_ignore_ascii_case(candidate.destination.trim()))
                .cloned()
                .or_else(|| allowed_sections.first().cloned())
                .unwrap_or_else(|| "Process".to_string());
            let kind = if candidate.kind.eq_ignore_ascii_case("retract") {
                ProposalKind::Retract
            } else {
                // Unknown verbs degrade to the safe direction: an addition the
                // owner can reject, never a removal they might wave through.
                ProposalKind::Add
            };
            TasteProposal {
                id: proposal_id_for_kind(&candidate.directive, kind),
                kind,
                directive: candidate.directive,
                evidence: candidate.evidence,
                destination,
                confidence: candidate.confidence,
                status: ProposalStatus::Pending,
                source_session: source_session.to_string(),
                proposed_at: now,
                decided_at: None,
            }
        })
        .collect()
}

/// How an approved directive is rendered into the profile note.
fn directive_line(directive: &str) -> String {
    format!("- {directive}")
}

/// Place an approved directive into the profile note's markdown.
///
/// Returns `None` when the directive is already present — approving twice must
/// not append twice, and that idempotence is what makes the crash-ordering in
/// [`approve`] safe.
///
/// **This function only ever inserts lines. It never edits, reorders, reflows
/// or deletes one.** Locating a heading in markdown is the operation that
/// produced three leak classes in Slice 1. Here the note is owner-authored
/// rather than untrusted, so a mistake means misplacement rather than
/// injection — and append-only bounds the worst case to *a directive under the
/// wrong heading*, which the owner can see and move, instead of *lost owner
/// prose*, which they cannot recover.
///
/// When the destination heading is not found the directive goes into a new
/// section at the end rather than being dropped or guessed into an existing
/// one. A visible extra heading is a smaller cost than a silently misfiled
/// directive, because the owner can actually notice it.
pub fn place_directive(existing: &str, directive: &str, section: &str) -> Option<String> {
    let line = directive_line(directive);
    // Idempotence is checked against the rendered line, trimmed, so
    // re-approving after a crash is a no-op rather than a duplicate.
    if existing
        .lines()
        .any(|existing_line| existing_line.trim() == line)
    {
        return None;
    }

    let lines: Vec<&str> = existing.lines().collect();
    let heading_at = lines.iter().position(|candidate| {
        let trimmed = candidate.trim_start();
        trimmed
            .strip_prefix('#')
            .map(|rest| {
                rest.trim_start_matches('#')
                    .trim()
                    .eq_ignore_ascii_case(section.trim())
            })
            .unwrap_or(false)
    });

    let mut out: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    match heading_at {
        Some(index) => {
            // End of this section: the line before the next heading, or the
            // end of the note. Trailing blanks are stepped over so the new
            // entry joins the list rather than floating after a gap.
            let mut insert_at = lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, candidate)| candidate.trim_start().starts_with('#'))
                .map(|(position, _)| position)
                .unwrap_or(lines.len());
            while insert_at > index + 1 && lines[insert_at - 1].trim().is_empty() {
                insert_at -= 1;
            }
            out.insert(insert_at, line);
        },
        None => {
            if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                out.push(String::new());
            }
            out.push(format!("## {}", section.trim()));
            out.push(String::new());
            out.push(line);
        },
    }
    let mut rendered = out.join("\n");
    if existing.ends_with('\n') || rendered.is_empty() {
        rendered.push('\n');
    }
    Some(rendered)
}

/// Render the owner-facing mirror of the pending queue.
///
/// This note is **written and never read back**. It exists so the owner can
/// see what is waiting from inside their notes app; every decision is taken
/// against the JSONL store. Nothing here is ever parsed, which is why the
/// formatting can be whatever reads best.
pub fn render_proposals_note(pending: &[TasteProposal]) -> String {
    let mut out = String::from("# Taste proposals awaiting your approval\n\n");
    if pending.is_empty() {
        out.push_str(
            "Nothing waiting.\n\nThis note is written by Magician and is never read back \
             into a prompt. Editing it has no effect — approve or reject from the app.\n",
        );
        return out;
    }
    out.push_str(
        "Approve or reject these in the app. **This note is never injected and never read \
         back** — editing it changes nothing.\n\n",
    );
    for proposal in pending {
        out.push_str(&format!(
            "## {}\n\n_Destination: {} · confidence {:.2}_\n\n",
            proposal.directive, proposal.destination, proposal.confidence
        ));
        for quote in &proposal.evidence {
            out.push_str(&format!("> {quote}\n>\n"));
        }
        out.push('\n');
    }
    out
}

/// What an approve/reject attempt did.
#[derive(Debug, PartialEq, Eq)]
pub enum DecisionOutcome {
    /// Recorded. `placed` is false when the note already matched what was
    /// asked — a repeat approve, which is a no-op rather than a duplicate.
    Decided { placed: bool },
    /// An approved retraction could not be applied because the note no longer
    /// contains that exact line. Reported rather than swallowed: the owner
    /// asked for a removal and did not get one, and telling them it succeeded
    /// would leave a directive they believe is gone still shaping every run.
    RetractionRefused(RemovalRefusal),
    /// No proposal with that id; the caller answers 404 rather than inventing
    /// a result.
    Unknown,
}

/// Ties the store, the profile note and the mirror together.
pub struct TasteCaptureService {
    loader: Arc<crate::magician_v2::taste_profile::TasteProfileLoader>,
    /// Directory holding **one proposal store per scope**.
    ///
    /// Separate files rather than one file plus a scope filter, for the same
    /// reason proposals live in a sibling note rather than a filtered section:
    /// a filter is only as good as every query that remembers to apply it, and
    /// the thing being separated here is directives distilled from one owner's
    /// private transcripts. The first version shared a single store across
    /// every scope, so one owner's review queue listed another's proposals and
    /// could approve them into their own profile.
    base_dir: PathBuf,
    /// Opened stores, so a busy scope does not rebuild its store per request.
    /// Each holds its own write lock, which is correct — the locks guard
    /// distinct files.
    stores: Mutex<HashMap<String, Arc<TasteProposalStore>>>,
}

impl TasteCaptureService {
    pub fn new(
        loader: Arc<crate::magician_v2::taste_profile::TasteProfileLoader>,
        base_dir: PathBuf,
    ) -> Self {
        Self {
            loader,
            base_dir,
            stores: Mutex::new(HashMap::new()),
        }
    }

    /// The proposal store for one scope, opening it on first use.
    ///
    /// The filename is derived from a digest of the scope rather than from the
    /// principal and workspace directly: those are user-controlled strings that
    /// may contain separators, spaces, or path components, and interpolating
    /// them into a filename is how one scope reaches another's file.
    pub async fn store_for(&self, principal: &str, workspace: &str) -> Arc<TasteProposalStore> {
        let key = capture_scope_key(principal, workspace);
        let mut guard = self.stores.lock().await;
        if let Some(store) = guard.get(&key) {
            return Arc::clone(store);
        }
        let store = Arc::new(TasteProposalStore::new(
            self.base_dir.join(format!("taste-proposals-{key}.jsonl")),
        ));
        guard.insert(key, Arc::clone(&store));
        store
    }

    pub async fn capture_health(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<CaptureHealth> {
        let path = self.base_dir.join(format!(
            "taste-capture-{}.json",
            capture_scope_key(principal, workspace)
        ));
        match tokio::fs::read(path).await {
            Ok(bytes) => {
                let mark: CaptureWatermark =
                    serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                Ok(CaptureHealth {
                    pending_retries: mark.retries.len(),
                    ..mark.health
                })
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CaptureHealth::default())
            },
            Err(error) => Err(error),
        }
    }

    /// Distil one finished session and file whatever survives policy.
    ///
    /// Takes the model as a parameter rather than owning one so the whole
    /// admission path is testable without a provider — the interesting
    /// behaviour here is what gets *refused*, and that must be pinned
    /// independently of whether a local model happens to be bound.
    ///
    /// The caller supplies a guard-verified dispatcher. This function does not
    /// re-run the locality guard itself: doing it in two places is how one of
    /// them ends up not doing it.
    ///
    /// One span per distilled session. `system_prompt` and `transcript` are a
    /// prompt body and user content respectively, so `skip_all` is mandatory
    /// here — a default-recording `#[instrument]` would put an owner's whole
    /// chat transcript on the activity websocket.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "taste_profile_distill",
        skip_all,
        fields(
            activity_kind = KIND_BACKGROUND,
            principal = %principal,
            workspace = %workspace,
            session_id = %session_id,
        )
    )]
    pub async fn distill_session(
        &self,
        principal: &str,
        workspace: &str,
        llm: &dyn crate::magician_v2::llm_dispatch_seam::DistillLlm,
        system_prompt: &str,
        transcript: &str,
        session_id: &str,
        allowed_sections: &[String],
        now: DateTime<Utc>,
    ) -> std::io::Result<FileOutcome> {
        let settings = self.loader.settings();
        if !settings.capture_enabled {
            return Ok(FileOutcome::default());
        }
        // The cap is checked before the model call, not after: a capped day
        // must cost nothing, and distilling only to discard is the shape that
        // makes a local model look expensive for no reason.
        let store = self.store_for(principal, workspace).await;
        let already = store
            .read()
            .await?
            .proposals
            .iter()
            .filter(|proposal| proposal.proposed_at >= now - Duration::days(1))
            .count() as u32;
        if already >= settings.max_proposals_per_day {
            return Ok(FileOutcome {
                suppressed_capped: 1,
                ..Default::default()
            });
        }

        let raw = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            llm.complete(system_prompt, transcript),
        )
        .await
        {
            Ok(Ok(raw)) => raw,
            Ok(Err(error)) => {
                tracing::warn!(%error, session_id, "taste capture: distillation failed");
                return Ok(FileOutcome {
                    retryable_failure: Some(CaptureFailure::Provider),
                    ..Default::default()
                });
            },
            Err(_) => {
                return Ok(FileOutcome {
                    retryable_failure: Some(CaptureFailure::Timeout),
                    ..Default::default()
                })
            },
        };

        let (candidates, counts) = admit_candidates(
            &raw,
            settings.min_confidence,
            settings.max_proposals_per_day as usize,
        );
        if counts.invalid_reply {
            return Ok(FileOutcome {
                retryable_failure: Some(CaptureFailure::InvalidReply),
                ..Default::default()
            });
        }
        if counts.admitted == 0 {
            tracing::debug!(
                session_id,
                below_confidence = counts.below_confidence,
                no_evidence = counts.no_evidence,
                over_length = counts.over_length,
                "taste capture: nothing admitted from this session"
            );
            return Ok(FileOutcome::default());
        }

        let proposals = into_proposals(candidates, allowed_sections, session_id, now);
        let outcome = store
            .file_candidates(proposals, settings.max_proposals_per_day, now)
            .await?;
        // The mirror is refreshed by the caller, which knows the scope. This
        // function is deliberately scope-free: writing the mirror under a
        // guessed principal would put one owner's proposals in another
        // owner's notes space.
        Ok(outcome)
    }

    /// Approve a proposal: place the directive, then record the decision.
    ///
    /// **The order is load-bearing and is pinned by test.** The notes layer
    /// takes its write lock per call, not per transaction, so the profile
    /// write and the status write cannot be made atomic with respect to each
    /// other. Given that, the question is only which way a crash should fail:
    ///
    /// - note first, then status — a crash leaves the directive placed and the
    ///   proposal still pending. The owner is offered it again; approving is
    ///   idempotent, so the second approve places nothing.
    /// - status first, then note — a crash leaves the proposal marked approved
    ///   and the directive missing. It is never offered again, and nothing
    ///   anywhere records that it was lost.
    ///
    /// Duplicate-visible beats silently-lost, so the note is written first.
    pub async fn approve(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> std::io::Result<DecisionOutcome> {
        let store = self.store_for(principal, workspace).await;
        let Some(proposal) = store
            .read()
            .await?
            .proposals
            .into_iter()
            .find(|candidate| candidate.id == id)
        else {
            return Ok(DecisionOutcome::Unknown);
        };

        // A replay of an old approval must not undo a later approved opposite
        // transition (add -> retract -> restore), nor override a rejection.
        if proposal.status != ProposalStatus::Pending {
            return Ok(DecisionOutcome::Decided { placed: false });
        }

        let existing = self
            .loader
            .read_profile_note_raw(principal, workspace)
            .await
            .map(|note| note.markdown)
            .unwrap_or_default();

        let note_path = self.loader.settings().note_path.trim().to_string();
        let mut placed = false;
        match proposal.kind {
            ProposalKind::Add => {
                if let Some(updated) =
                    place_directive(&existing, &proposal.directive, &proposal.destination)
                {
                    self.write_note(principal, workspace, &note_path, updated)
                        .await?;
                    placed = true;
                }
            },
            ProposalKind::Retract => match remove_directive(&existing, &proposal.directive) {
                Ok(updated) => {
                    self.write_note(principal, workspace, &note_path, updated)
                        .await?;
                    placed = true;
                },
                // The proposal stays pending. A retraction that silently
                // failed is worse than one still in the queue: the owner
                // believes the directive is gone while it keeps shaping runs.
                Err(refusal) => return Ok(DecisionOutcome::RetractionRefused(refusal)),
            },
        }

        store.decide(id, ProposalStatus::Approved, now).await?;
        self.refresh_mirror(principal, workspace).await;
        Ok(DecisionOutcome::Decided { placed })
    }

    /// Reject a proposal. No note write — rejection only records that this
    /// directive must never be offered again.
    pub async fn reject(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        now: DateTime<Utc>,
    ) -> std::io::Result<DecisionOutcome> {
        let store = self.store_for(principal, workspace).await;
        if store
            .decide(id, ProposalStatus::Rejected, now)
            .await?
            .is_none()
        {
            return Ok(DecisionOutcome::Unknown);
        }
        self.refresh_mirror(principal, workspace).await;
        Ok(DecisionOutcome::Decided { placed: false })
    }

    /// Rewrite the owner-facing mirror from the store.
    ///
    /// Best-effort: the decision is already durable in the store by the time
    /// this runs, so a mirror that fails to write costs the owner a stale view
    /// of their queue, not a lost decision. Failing the request here would
    /// report an error for something that already succeeded.
    async fn refresh_mirror(&self, principal: &str, workspace: &str) {
        let pending = match self.store_for(principal, workspace).await.pending().await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "taste capture: could not read the queue to refresh mirror");
                return;
            },
        };
        let path = self.loader.settings().proposals_note_path();
        let body = render_proposals_note(&pending);
        if let Err(error) = self.write_note(principal, workspace, &path, body).await {
            tracing::warn!(
                %error,
                "taste capture: the proposals mirror could not be refreshed; the queue itself \
                 is unaffected"
            );
        }
    }

    async fn write_note(
        &self,
        principal: &str,
        workspace: &str,
        target_path: &str,
        markdown: String,
    ) -> std::io::Result<()> {
        self.loader
            .notes_store()
            .write_note_markdown(
                principal,
                workspace,
                crate::magician_v2::notes::WriteNoteMarkdownRequest {
                    provider: None,
                    target_path: target_path.to_string(),
                    markdown,
                },
            )
            .await
            .map(|_| ())
    }
}

/// Sessions already distilled, so a restart does not re-read them.
///
/// Ids rather than a timestamp watermark: a timestamp skips any session whose
/// clock or ordering surprises us, and skipping is silent. A bounded id set
/// costs a little more and cannot quietly drop a session.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CaptureWatermark {
    /// Legacy single list, kept only so an existing file still loads. It is
    /// split into the two lists below on read and never written again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    processed: Vec<String>,
    /// Chat session ids, capped independently of executions.
    #[serde(default)]
    chat: Vec<String>,
    #[serde(default)]
    chat_revisions: HashMap<String, i64>,
    /// Execution ids (already `exec:`-prefixed), capped independently of chat.
    #[serde(default)]
    executions: Vec<String>,
    #[serde(default)]
    retries: HashMap<String, CaptureRetry>,
    #[serde(default)]
    health: CaptureHealth,
    #[serde(default)]
    prefer_execution_next: bool,
}

impl CaptureWatermark {
    fn chat_budget(&mut self, batch: usize) -> usize {
        if batch == 1 {
            let budget = usize::from(!self.prefer_execution_next);
            self.prefer_execution_next = !self.prefer_execution_next;
            budget
        } else {
            batch.div_ceil(2)
        }
    }
    fn chat_needs_capture(&mut self, id: &str, updated_at: i64) -> bool {
        if !self.chat.iter().any(|seen| seen == id) {
            return true;
        }
        // Legacy completed IDs get a baseline, not an expensive historical replay.
        let prior = self
            .chat_revisions
            .entry(id.to_owned())
            .or_insert(updated_at);
        *prior < updated_at
    }

    fn due_execution_retries(&self, now: i64, limit: usize) -> Vec<String> {
        let mut due = self
            .retries
            .iter()
            .filter(|(key, retry)| key.starts_with("exec:") && retry.next_attempt_at <= now)
            .collect::<Vec<_>>();
        due.sort_by(|a, b| {
            a.1.next_attempt_at
                .cmp(&b.1.next_attempt_at)
                .then_with(|| a.0.cmp(b.0))
        });
        due.into_iter()
            .take(limit)
            .map(|(key, _)| key.trim_start_matches("exec:").to_owned())
            .collect()
    }
    fn retry_ready(&self, key: &str, now: i64) -> bool {
        self.retries
            .get(key)
            .is_none_or(|retry| retry.next_attempt_at <= now)
    }

    fn record_failure(&mut self, key: &str, failure: CaptureFailure, now: i64) {
        let attempts = self
            .retries
            .get(key)
            .map_or(1, |retry| retry.attempts.saturating_add(1));
        let delay = (900_i64 * (1_i64 << attempts.saturating_sub(1).min(7))).min(86_400);
        self.retries.insert(
            key.to_string(),
            CaptureRetry {
                attempts,
                next_attempt_at: now.saturating_add(delay),
                failure,
            },
        );
        // Bounded retry bookkeeping. Evicted IDs remain unprocessed and may
        // be rediscovered from their source; eviction never fabricates success.
        if self.retries.len() > WATERMARK_KEEP {
            if let Some(oldest) = self
                .retries
                .iter()
                .filter(|(id, _)| id.as_str() != key)
                .min_by_key(|(_, retry)| retry.next_attempt_at)
                .map(|(id, _)| id.clone())
            {
                self.retries.remove(&oldest);
            }
        }
        self.health.failed_attempts = self.health.failed_attempts.saturating_add(1);
        self.health.pending_retries = self.retries.len();
        self.health.last_attempt_at = Some(now);
        self.health.last_failure = Some(failure);
    }

    fn record_outcome(&mut self, key: &str, outcome: &FileOutcome, now: i64) {
        if let Some(failure) = outcome.retryable_failure {
            self.record_failure(key, failure, now);
            return;
        }
        self.health.filed = self.health.filed.saturating_add(outcome.filed as u64);
        if outcome.suppressed_capped > 0 {
            return; // Deferred by policy, not successfully examined.
        }
        self.retries.remove(key);
        let list = if key.starts_with("exec:") {
            &mut self.executions
        } else {
            &mut self.chat
        };
        if !list.iter().any(|id| id == key) {
            list.push(key.to_string());
        }
        self.health.completed = self.health.completed.saturating_add(1);
        if outcome.filed == 0 {
            self.health.empty = self.health.empty.saturating_add(1);
        }
        self.health.pending_retries = self.retries.len();
        self.health.last_attempt_at = Some(now);
    }

    /// Fold a legacy single list into the per-kind lists.
    ///
    /// One shared cap let a busy execution scope evict chat ids, so chat
    /// sessions were re-distilled — wasted model calls rather than duplicate
    /// proposals, since the content-addressed store still refuses anything
    /// already decided, but waste that grows quietly with activity.
    fn migrate_legacy(&mut self) {
        if self.processed.is_empty() {
            return;
        }
        for id in std::mem::take(&mut self.processed) {
            if id.starts_with("exec:") {
                self.executions.push(id);
            } else {
                self.chat.push(id);
            }
        }
    }

    /// Every id already handled, regardless of source.
    fn seen(&self) -> std::collections::HashSet<String> {
        self.chat
            .iter()
            .chain(self.executions.iter())
            .cloned()
            .collect()
    }
}

const WATERMARK_KEEP: usize = 500;

/// Sessions distilled in one sweep.
///
/// Bounds both the transcript reads and the model calls a single pass can
/// make. The daily cap already limits *proposals*, but a session can yield
/// none, so without this a deep backlog would read and distil session after
/// session for nothing.
const MAX_SESSIONS_PER_SWEEP: usize = 5;

/// Turn a session's messages into the text the distiller reads.
///
/// Only plain text survives. Tool calls and structured payloads are the
/// machine's side of the conversation, and a directive inferred from them
/// would be about how Magician worked rather than what the owner wants.
pub fn transcript_from_messages(
    messages: &[crate::magician_v2::chat::models::ChatMessage],
) -> String {
    let mut out = String::new();
    for message in messages {
        if let crate::magician_v2::chat::models::ChatMessageContent::Text { text, .. } =
            &message.content
        {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push_str(&format!("{:?}: {trimmed}\n\n", message.direction));
        }
    }
    out
}

/// Turn an execution's turns into the text the distiller reads.
///
/// The sibling of [`transcript_from_messages`] for the execution side. Same
/// rule: only what was actually said, so a directive is inferred from the
/// owner's words rather than from the machine's bookkeeping.
pub fn transcript_from_turns(turns: &[crate::magician_v2::storage::models::V2Turn]) -> String {
    let mut out = String::new();
    for turn in turns {
        let trimmed = turn.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push_str(&format!("{:?}: {trimmed}\n\n", turn.direction));
    }
    out
}

/// Watermark key for an execution.
///
/// Prefixed so an execution id can never collide with a chat session id in the
/// processed set. Chat ids stay bare: prefixing one side is enough to
/// disambiguate, and changing the chat format would re-distil every session a
/// deployment had already processed.
fn execution_watermark_key(execution_id: &str) -> String {
    format!("exec:{execution_id}")
}

/// Distils finished chat sessions and terminal executions on an interval.
///
/// An interval sweep rather than a hook on session close: there is no single
/// close path, and a missed hook is a feature that silently never runs — the
/// failure mode this codebase has paid for more than once. A sweep that finds
/// a session late still finds it.
pub struct TasteCaptureWorker {
    service: Arc<TasteCaptureService>,
    chat: Arc<crate::magician_v2::chat::service::ChatService>,
    router:
        Option<Arc<crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>>,
    /// Enumerated **every sweep**, not captured at boot.
    ///
    /// One worker per scope, spawned from a startup snapshot, silently never
    /// runs for any scope created afterwards — the owner enables capture, adds
    /// a workspace, and nothing ever happens in it until the next restart.
    /// That is the same silent-no-op failure this worker's interval design
    /// exists to avoid, so the scope list is re-read rather than frozen.
    scopes: Arc<crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace>,
    /// Execution transcripts. The design specified "chat idle-out **or**
    /// execution terminal", and a correction stated while a task runs is
    /// exactly where much of the owner's standing direction gets expressed.
    conversations: Arc<dyn crate::magician_v2::storage::r#trait::V2ConversationStore>,
    watermark_dir: PathBuf,
    idle_after: Duration,
    interval: std::time::Duration,
}

impl TasteCaptureWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        service: Arc<TasteCaptureService>,
        chat: Arc<crate::magician_v2::chat::service::ChatService>,
        router: Option<
            Arc<crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
        >,
        scopes: Arc<crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace>,
        conversations: Arc<dyn crate::magician_v2::storage::r#trait::V2ConversationStore>,
        watermark_dir: PathBuf,
    ) -> Self {
        Self {
            service,
            chat,
            router,
            scopes,
            conversations,
            watermark_dir,
            idle_after: Duration::minutes(30),
            interval: std::time::Duration::from_secs(900),
        }
    }

    /// Per-scope watermark path. Digest-named for the same reason the store is:
    /// principal and workspace are user-controlled and must never reach a path.
    fn watermark_path(&self, principal: &str, workspace: &str) -> PathBuf {
        let key = capture_scope_key(principal, workspace);
        self.watermark_dir.join(format!("taste-capture-{key}.json"))
    }

    /// Sweep every scope that currently exists.
    async fn sweep_all_scopes(&self, now: DateTime<Utc>) -> usize {
        let scopes = match self.scopes.list_tenant_scope_segments_sync() {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::warn!(%error, "taste capture: scope listing failed; sweep skipped");
                return 0;
            },
        };
        let mut filed = 0usize;
        for (principal, workspace) in scopes {
            match self.sweep_once(&principal, &workspace, now).await {
                Ok(count) => filed += count,
                // One scope's failure must not stop the others: a single
                // unreadable store would otherwise starve every other owner.
                Err(error) => tracing::warn!(
                    %error, principal, workspace, "taste capture: scope sweep failed"
                ),
            }
        }
        filed
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.interval);
            // The first tick fires immediately; skip it so a boot storm does
            // not distil every session the moment the process starts.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                self.sweep_all_scopes(Utc::now()).await;
            }
        })
    }

    async fn load_watermark(
        &self,
        principal: &str,
        workspace: &str,
    ) -> std::io::Result<CaptureWatermark> {
        match tokio::fs::read(&self.watermark_path(principal, workspace)).await {
            Ok(body) => {
                let mut mark: CaptureWatermark =
                    serde_json::from_slice(&body).map_err(std::io::Error::other)?;
                mark.migrate_legacy();
                Ok(mark)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CaptureWatermark::default())
            },
            Err(error) => Err(error),
        }
    }

    async fn save_watermark(
        &self,
        principal: &str,
        workspace: &str,
        mark: &mut CaptureWatermark,
    ) -> std::io::Result<()> {
        // Capped per kind. A shared cap meant whichever source was busier
        // evicted the other's history and caused it to be re-distilled.
        for list in [&mut mark.chat, &mut mark.executions] {
            if list.len() > WATERMARK_KEEP {
                let drop = list.len() - WATERMARK_KEEP;
                list.drain(..drop);
            }
        }
        mark.chat_revisions.retain(|id, _| mark.chat.contains(id));
        let path = self.watermark_path(principal, workspace);
        let body = serde_json::to_vec(mark).map_err(std::io::Error::other)?;
        let write = tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&path, &body)
        })
        .await;
        write.map_err(std::io::Error::other)?
    }

    /// One pass. Public so a test or an operator can drive it directly rather
    /// than waiting on the interval.
    ///
    /// The sweep is the unit an operator cares about — one row per pass, with
    /// each distilled session nested underneath it.
    #[instrument(
        name = "taste_capture_sweep",
        skip_all,
        fields(
            activity_kind = KIND_BACKGROUND,
            workload_class = WORKLOAD_SCHEDULED,
            principal = %principal,
            workspace = %workspace,
        )
    )]
    pub async fn sweep_once(
        &self,
        principal: &str,
        workspace: &str,
        now: DateTime<Utc>,
    ) -> std::io::Result<usize> {
        if !self.service.loader.settings().capture_enabled {
            return Ok(0);
        }
        // Resolve the guard once per sweep, against live config. Unbound means
        // capture is off right now — not "use the default model".
        let binding = match resolve_taste_distiller(self.router.as_deref()) {
            Ok(binding) => binding,
            Err(reason) => {
                tracing::debug!(?reason, "taste capture: no distiller bound; sweep is idle");
                return Ok(0);
            },
        };
        let Some(router) = self.router.clone() else {
            return Ok(0);
        };
        tracing::debug!(profile = %binding.profile, "taste capture: sweeping finished sessions");

        let llm = crate::magician_v2::llm_dispatch_seam::RouterDistillLlm::new_for_operation(
            router,
            None,
            principal.to_string(),
            workspace.to_string(),
            TASTE_PROFILE_DISTILL_OPERATION,
            false,
        );

        // Read the profile once per sweep, not per session: it cannot change
        // mid-sweep in any way that matters, and re-reading per session would
        // multiply provider reads by the session count for identical content.
        let current_profile = self
            .service
            .loader
            .read_profile_note_raw(principal, workspace)
            .await
            .map(|note| note.markdown)
            .unwrap_or_default();
        let system_prompt = match self.render_prompt(&current_profile).await {
            Some(prompt) => prompt,
            None => return Ok(0),
        };

        let sessions = match self.chat.list_sessions(principal, workspace).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "taste capture: could not list sessions");
                return Ok(0);
            },
        };

        let mut mark = self.load_watermark(principal, workspace).await?;
        // Owned, not borrowed from `mark.processed`: the loops below push to
        // that vec while still consulting the set, and a borrowed view would
        // hold `mark` immutably across the mutation.
        let already = mark.seen();
        let mut distilled = 0usize;
        let cutoff = (now - self.idle_after).timestamp_millis();

        // Budget the sweep BEFORE reading anything. `distill_session` also
        // checks the cap, but it checks it after the caller has already paid
        // for the transcript read — and on the first sweep after enabling
        // capture on an existing deployment, that is a 200-message read for
        // every session in history to produce at most a handful of proposals.
        let store = self.service.store_for(principal, workspace).await;
        let raised_today = store
            .read()
            .await?
            .proposals
            .iter()
            .filter(|proposal| proposal.proposed_at >= now - Duration::days(1))
            .count() as u32;
        let daily_cap = self.service.loader.settings().max_proposals_per_day;
        if raised_today >= daily_cap {
            tracing::debug!(
                raised_today,
                daily_cap,
                "taste capture: day's cap met; sweep idle"
            );
            return Ok(0);
        }
        let remaining = (daily_cap - raised_today) as usize;

        // Newest first, by an explicit sort. `list_sessions` documents no
        // ordering — its sibling says "in no particular order" — so the
        // earlier `.reverse()` here reversed an arbitrary list and the comment
        // claiming newest-first was simply false. A session that just ended is
        // likelier to hold something worth proposing than one from last month,
        // and the batch bound means the older tail waits for a later sweep.
        let needs_capture: std::collections::HashSet<String> = sessions
            .iter()
            .filter(|s| mark.chat_needs_capture(&s.id, s.updated_at))
            .map(|s| s.id.clone())
            .collect();
        let mut pending: Vec<&_> = sessions
            .iter()
            .filter(|session| {
                needs_capture.contains(&session.id)
                    && mark.retry_ready(&session.id, now.timestamp())
            })
            .filter(|session| session.updated_at <= cutoff)
            .collect();
        pending.sort_by(|a, b| {
            mark.retries
                .contains_key(&b.id)
                .cmp(&mark.retries.contains_key(&a.id))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        // Split the batch between the two sources rather than letting chat
        // take it all. Chat-first-takes-all starves executions to zero on any
        // deployment with steady chat activity — the source would be wired and
        // never once run, which is the failure this worker's whole shape
        // exists to avoid. Alternate the last daily slot durably across sources.
        let batch = remaining.min(MAX_SESSIONS_PER_SWEEP);
        let chat_budget = mark.chat_budget(batch);
        let mut candidates: Vec<String> = pending
            .iter()
            .take(chat_budget)
            .map(|session| session.id.clone())
            .collect();

        // Terminal executions, sharing the same budget as chat. Gathered after
        // chat so a busy chat scope cannot be starved by a long execution
        // backlog, and bounded by whatever the chat pass left unspent.
        let mut execution_candidates: Vec<String> = Vec::new();
        // Whatever chat did not use, plus its own half. A quiet chat scope
        // hands its slots to executions rather than wasting them.
        let exec_budget = batch.saturating_sub(candidates.len());
        if exec_budget > 0 {
            // A failed execution may have fallen off the newest-50 discovery
            // page. Reserve retry capacity and revalidate its scope/terminal state.
            for id in mark.due_execution_retries(now.timestamp(), exec_budget.div_ceil(2)) {
                match self.conversations.get_execution(&id).await {
                    Ok(run)
                        if run.principal == principal
                            && run.workspace == workspace
                            && run.waiting_state.is_terminal() =>
                    {
                        execution_candidates.push(id)
                    },
                    Ok(_) => {},
                    Err(_) => mark.record_failure(
                        &execution_watermark_key(&id),
                        CaptureFailure::SourceRead,
                        now.timestamp(),
                    ),
                }
            }
            match self
                .conversations
                .list_executions(
                    principal,
                    workspace,
                    runtime_core::PaginationParams {
                        limit: 50,
                        offset: 0,
                    },
                )
                .await
            {
                Ok(page) => {
                    // No `.rev()`: `list_executions` already sorts
                    // `updated_at` descending, so the page arrives newest
                    // first. Reversing it — copied from the chat path before
                    // that path was fixed — walked the oldest of the newest 50
                    // and left recent work waiting behind stale runs.
                    for summary in page.items.iter() {
                        if execution_candidates.len() >= exec_budget {
                            break;
                        }
                        // Only finished work: distilling a running execution
                        // would read a conversation the owner is still having.
                        if !summary.waiting_state.is_terminal() {
                            continue;
                        }
                        let key = execution_watermark_key(&summary.id);
                        if already.contains(&key)
                            || execution_candidates.contains(&summary.id)
                            || !mark.retry_ready(&key, now.timestamp())
                        {
                            continue;
                        }
                        execution_candidates.push(summary.id.clone());
                    }
                },
                Err(error) => {
                    // Warn and carry on with chat: one unavailable source must
                    // not silence the other.
                    tracing::warn!(%error, "taste capture: could not list executions");
                },
            }
        }

        if candidates.is_empty() && execution_candidates.is_empty() && batch == 1 {
            candidates.extend(pending.first().map(|session| session.id.clone()));
        }
        for execution_id in execution_candidates {
            let turns = match self.conversations.get_turns(&execution_id).await {
                Ok(turns) => turns,
                Err(error) => {
                    tracing::warn!(%error, execution_id, "taste capture: could not read turns");
                    mark.record_failure(
                        &execution_watermark_key(&execution_id),
                        CaptureFailure::SourceRead,
                        now.timestamp(),
                    );
                    self.save_watermark(principal, workspace, &mut mark).await?;
                    continue;
                },
            };
            let transcript = transcript_from_turns(&turns);
            if transcript.trim().is_empty() {
                mark.record_outcome(
                    &execution_watermark_key(&execution_id),
                    &FileOutcome::default(),
                    now.timestamp(),
                );
                self.save_watermark(principal, workspace, &mut mark).await?;
                continue;
            }
            let outcome = self
                .service
                .distill_session(
                    principal,
                    workspace,
                    &llm,
                    &system_prompt,
                    &transcript,
                    &execution_id,
                    &self.destination_sections(),
                    now,
                )
                .await
                .unwrap_or_else(|_| FileOutcome {
                    retryable_failure: Some(CaptureFailure::Storage),
                    ..Default::default()
                });
            distilled += outcome.filed;
            mark.record_outcome(
                &execution_watermark_key(&execution_id),
                &outcome,
                now.timestamp(),
            );
            self.save_watermark(principal, workspace, &mut mark).await?;
        }

        for session_id in candidates {
            let messages = match self
                .chat
                .chat_store_ref()
                .get_messages_paginated(&session_id, 200, None)
                .await
            {
                Ok((messages, _)) => messages,
                Err(error) => {
                    tracing::warn!(%error, session_id, "taste capture: could not read messages");
                    mark.record_failure(&session_id, CaptureFailure::SourceRead, now.timestamp());
                    self.save_watermark(principal, workspace, &mut mark).await?;
                    continue;
                },
            };
            // Idle-out, judged from the last message rather than a session
            // status field: a session the owner is still in must not be
            // distilled mid-conversation.
            let last_at = messages.iter().map(|m| m.created_at).max().unwrap_or(0);
            if last_at > cutoff {
                continue;
            }
            let transcript = transcript_from_messages(&messages);
            if transcript.trim().is_empty() {
                // Nothing the owner said; record it so we do not re-read an
                // empty session on every sweep forever.
                mark.record_outcome(&session_id, &FileOutcome::default(), now.timestamp());
                if let Some(session) = sessions.iter().find(|s| s.id == session_id) {
                    mark.chat_revisions
                        .insert(session_id.clone(), session.updated_at);
                }
                self.save_watermark(principal, workspace, &mut mark).await?;
                continue;
            }
            let outcome = self
                .service
                .distill_session(
                    principal,
                    workspace,
                    &llm,
                    &system_prompt,
                    &transcript,
                    &session_id,
                    &self.destination_sections(),
                    now,
                )
                .await
                .unwrap_or_else(|_| FileOutcome {
                    retryable_failure: Some(CaptureFailure::Storage),
                    ..Default::default()
                });
            distilled += outcome.filed;
            mark.record_outcome(&session_id, &outcome, now.timestamp());
            if outcome.retryable_failure.is_none() && outcome.suppressed_capped == 0 {
                if let Some(session) = sessions.iter().find(|s| s.id == session_id) {
                    mark.chat_revisions
                        .insert(session_id.clone(), session.updated_at);
                }
            }
            self.save_watermark(principal, workspace, &mut mark).await?;
        }

        self.save_watermark(principal, workspace, &mut mark).await?;
        if distilled > 0 {
            self.service.refresh_mirror(principal, workspace).await;
        }
        Ok(distilled)
    }

    fn destination_sections(&self) -> Vec<String> {
        vec![
            "Voice".to_string(),
            "Process".to_string(),
            "Boundaries".to_string(),
        ]
    }

    /// Render the distiller's system prompt from the store.
    ///
    /// `rendered_prompt` deliberately has no compiled fallback: a missing
    /// template degrades capture to idle rather than silently substituting
    /// different instructions. Prompts live in the store, and a hardcoded
    /// backup here would be a second copy that drifts.
    async fn render_prompt(&self, current_profile: &str) -> Option<String> {
        let mut variables = HashMap::new();
        variables.insert(
            "current_profile".to_string(),
            if current_profile.trim().is_empty() {
                // No profile means no retraction is possible. Saying so beats
                // an empty block, which a model can read as "the profile is
                // empty, so everything in it is stale".
                "(no profile written yet)".to_string()
            } else {
                current_profile.to_string()
            },
        );
        variables.insert(
            "destination_sections".to_string(),
            self.destination_sections()
                .iter()
                .map(|section| format!("- {section}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        variables.insert(
            "max_candidates".to_string(),
            self.service
                .loader
                .settings()
                .max_proposals_per_day
                .to_string(),
        );
        // No `transcript` variable: it rides the user turn, because untrusted
        // input must not sit in the instructions. v1.1.0 still carried an
        // empty `{transcript}` slot, so the template announced a section and
        // rendered nothing under it.
        match crate::magician_v2::prompts::rendered_prompt(
            crate::magician_v2::prompts::constants::names::TASTE_PROFILE_DISTILL,
            crate::magician_v2::prompts::constants::versions::TASTE_PROFILE_DISTILL,
            variables,
        )
        .await
        {
            Ok(prompt) => Some(prompt),
            Err(error) => {
                tracing::warn!(%error, "taste capture: distiller prompt unavailable; sweep is idle");
                None
            },
        }
    }
}

/// Why a retraction could not be applied.
#[derive(Debug, PartialEq, Eq)]
pub enum RemovalRefusal {
    /// The line is not in the note. Already removed, or the owner reworded it.
    NotPresent,
    /// The same line appears more than once. Which one they meant is not
    /// knowable, and guessing risks deleting the wrong one.
    Ambiguous(usize),
}

/// Remove one approved-retraction directive from the profile note.
///
/// **This is the only code path in the feature that deletes owner text, and it
/// is deliberately the narrowest one that can work.** The additive path
/// ([`place_directive`]) is safe because a parse bug there misplaces a line;
/// a deletion bug loses prose the owner cannot recover, so the two cannot
/// share a risk posture.
///
/// Three constraints do the work:
///
/// - **Exact match on the rendered line, not a search.** No fuzzy matching, no
///   substring, no "starts with". The line removed is byte-identical (after
///   trim) to the one this system rendered when the directive was added.
/// - **Exactly one match, or nothing happens.** Zero matches means the owner
///   already changed it and the retraction is stale — refuse rather than hunt
///   for something similar. More than one is unresolvable, and deleting "the
///   first one" is a coin flip with the owner's writing.
/// - **Only that line.** Surrounding blank lines, the heading, and every other
///   line are preserved exactly, so an emptied section stays where it was
///   rather than being tidied away.
pub fn remove_directive(existing: &str, directive: &str) -> Result<String, RemovalRefusal> {
    let target = directive_line(directive);
    let matches = existing
        .lines()
        .filter(|line| line.trim() == target)
        .count();
    match matches {
        0 => return Err(RemovalRefusal::NotPresent),
        1 => {},
        other => return Err(RemovalRefusal::Ambiguous(other)),
    }
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| line.trim() != target)
        .collect();
    let mut rendered = kept.join("\n");
    if existing.ends_with('\n') {
        rendered.push('\n');
    }
    Ok(rendered)
}

static GLOBAL_CAPTURE_SERVICE: std::sync::OnceLock<Arc<TasteCaptureService>> =
    std::sync::OnceLock::new();

pub fn install_global_capture_service(service: Arc<TasteCaptureService>) {
    let _ = GLOBAL_CAPTURE_SERVICE.set(service);
}

pub fn global_capture_service() -> Option<Arc<TasteCaptureService>> {
    GLOBAL_CAPTURE_SERVICE.get().cloned()
}

static GLOBAL_PROPOSAL_STORE: std::sync::OnceLock<Arc<TasteProposalStore>> =
    std::sync::OnceLock::new();

pub fn install_global_proposal_store(store: Arc<TasteProposalStore>) {
    let _ = GLOBAL_PROPOSAL_STORE.set(store);
}

pub fn global_proposal_store() -> Option<Arc<TasteProposalStore>> {
    GLOBAL_PROPOSAL_STORE.get().cloned()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn capture_failure_recovery_and_empty_success_survive_restart() {
        let mut mark = CaptureWatermark::default();
        for failure in [
            CaptureFailure::Provider,
            CaptureFailure::InvalidReply,
            CaptureFailure::Timeout,
            CaptureFailure::Storage,
        ] {
            mark.record_outcome(
                "chat-1",
                &FileOutcome {
                    retryable_failure: Some(failure),
                    ..Default::default()
                },
                100,
            );
            assert!(!mark.seen().contains("chat-1"));
        }
        let mut restored: CaptureWatermark =
            serde_json::from_slice(&serde_json::to_vec(&mark).unwrap()).unwrap();
        assert!(!restored.retry_ready("chat-1", 101));
        assert!(restored.retry_ready("chat-1", 100_000));
        restored.record_outcome("chat-1", &FileOutcome::default(), 100_000);
        assert!(restored.seen().contains("chat-1"));
        assert!(restored.retries.is_empty());
        assert_eq!(restored.health.failed_attempts, 4);
        assert_eq!(restored.health.completed, 1);
        assert_eq!(restored.health.empty, 1);
        assert_eq!(restored.health.pending_retries, 0);
    }

    #[test]
    fn capture_single_remaining_slot_is_fair_across_restarts() {
        let mut mark = CaptureWatermark::default();
        assert_eq!(mark.chat_budget(1), 1);
        let mut restarted: CaptureWatermark =
            serde_json::from_str(&serde_json::to_string(&mark).unwrap()).unwrap();
        assert_eq!(restarted.chat_budget(1), 0);
        assert_eq!(restarted.chat_budget(1), 1);
        assert_eq!(restarted.chat_budget(8), 4);
    }

    #[test]
    fn capture_new_chat_revisions_and_due_old_execution_retries_are_not_lost() {
        let mut mark = CaptureWatermark::default();
        mark.chat.push("legacy-chat".into());
        assert!(!mark.chat_needs_capture("legacy-chat", 100));
        assert!(mark.chat_needs_capture("legacy-chat", 200));
        mark.record_failure("exec:outside-newest-page", CaptureFailure::Provider, 0);
        mark.record_failure("exec:later", CaptureFailure::Provider, 100);
        mark.record_failure("chat:wrong-lane", CaptureFailure::Provider, 0);
        assert_eq!(
            mark.due_execution_retries(901, 5),
            vec!["outside-newest-page"]
        );
        assert_eq!(
            mark.due_execution_retries(1001, 1),
            vec!["outside-newest-page"]
        );
    }

    struct CaptureReply(bool, &'static str);
    #[async_trait::async_trait]
    impl crate::magician_v2::llm_dispatch_seam::DistillLlm for CaptureReply {
        async fn complete(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            if self.0 {
                anyhow::bail!("temporary provider failure");
            }
            Ok(self.1.into())
        }
    }

    #[tokio::test]
    async fn capture_service_distinguishes_provider_invalid_and_valid_empty_replies() {
        use crate::magician_v2::{
            artifact_v2::workspace::ArtifactV2Workspace,
            notes::NotesSettingsStore,
            taste_profile::{TasteProfileLoader, TasteProfileSettings},
        };
        let temp = tempfile::tempdir().unwrap();
        let settings = TasteProfileSettings {
            capture_enabled: true,
            ..Default::default()
        };
        let loader = Arc::new(TasteProfileLoader::new(
            NotesSettingsStore::with_workspace_layout(ArtifactV2Workspace::new(temp.path())),
            settings,
        ));
        let service = TasteCaptureService::new(loader, temp.path().to_path_buf());
        for (reply, expected) in [
            (CaptureReply(true, ""), Some(CaptureFailure::Provider)),
            (
                CaptureReply(false, "{}"),
                Some(CaptureFailure::InvalidReply),
            ),
            (CaptureReply(false, r#"{"candidates":[]}"#), None),
        ] {
            let result = service
                .distill_session(
                    "p",
                    "w",
                    &reply,
                    "system",
                    "transcript",
                    "session",
                    &[],
                    Utc::now(),
                )
                .await
                .unwrap();
            assert_eq!(result.retryable_failure, expected);
        }
        assert!(service
            .store_for("p", "w")
            .await
            .pending()
            .await
            .unwrap()
            .is_empty());
        let mut mark = CaptureWatermark::default();
        mark.record_failure("session", CaptureFailure::Provider, 100);
        let path = temp.path().join(format!(
            "taste-capture-{}.json",
            capture_scope_key("p", "w")
        ));
        tokio::fs::write(&path, serde_json::to_vec(&mark).unwrap())
            .await
            .unwrap();
        let health = service.capture_health("p", "w").await.unwrap();
        assert_eq!(health.failed_attempts, 1);
        assert_eq!(health.pending_retries, 1);
        assert_eq!(
            service
                .capture_health("other", "w")
                .await
                .unwrap()
                .pending_retries,
            0
        );
        tokio::fs::write(&path, b"invalid watermark").await.unwrap();
        assert!(service.capture_health("p", "w").await.is_err());
    }

    fn candidate(directive: &str, confidence: f32, at: DateTime<Utc>) -> TasteProposal {
        TasteProposal {
            id: proposal_id(directive),
            kind: ProposalKind::Add,
            directive: directive.to_string(),
            evidence: vec!["because I said so".to_string()],
            destination: "Voice".to_string(),
            confidence,
            status: ProposalStatus::Pending,
            source_session: "session-1".to_string(),
            proposed_at: at,
            decided_at: None,
        }
    }

    fn store() -> (TasteProposalStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TasteProposalStore::new(dir.path().join("proposals.jsonl"));
        (store, dir)
    }

    #[tokio::test]
    async fn memory_lifecycle_profile_add_retract_and_restore_have_distinct_decisions() {
        let (store, _dir) = store();
        let now = Utc::now();
        let add = candidate("Prefer short answers", 0.95, now);
        assert_eq!(
            store
                .file_candidates(vec![add.clone()], 5, now)
                .await
                .unwrap()
                .filed,
            1
        );
        store
            .decide(&add.id, ProposalStatus::Approved, now)
            .await
            .unwrap();
        let mut retract = add.clone();
        retract.kind = ProposalKind::Retract;
        retract.proposed_at = now + chrono::Duration::seconds(1);
        let outcome = store.file_candidates(vec![retract], 5, now).await.unwrap();
        assert_eq!(outcome.filed, 1);
        let pending = store.pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_ne!(pending[0].id, add.id);
        assert_eq!(pending[0].kind, ProposalKind::Retract);
        store
            .decide(
                &pending[0].id,
                ProposalStatus::Approved,
                now + chrono::Duration::seconds(2),
            )
            .await
            .unwrap();
        let mut restore = add.clone();
        restore.proposed_at = now + chrono::Duration::seconds(3);
        assert_eq!(
            store
                .file_candidates(vec![restore.clone(), restore], 5, now)
                .await
                .unwrap()
                .filed,
            1
        );
        let restored = store.pending().await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].kind, ProposalKind::Add);
        assert_ne!(restored[0].id, add.id);
        assert_ne!(restored[0].id, pending[0].id);
    }

    #[tokio::test]
    async fn memory_lifecycle_profile_rejected_retraction_is_not_reoffered_on_restart() {
        let (store, dir) = store();
        let now = Utc::now();
        let mut retract = candidate("Prefer short answers", 0.95, now);
        retract.kind = ProposalKind::Retract;
        store
            .file_candidates(vec![retract.clone()], 5, now)
            .await
            .unwrap();
        let pending = store.pending().await.unwrap();
        store
            .decide(&pending[0].id, ProposalStatus::Rejected, now)
            .await
            .unwrap();
        let reopened = TasteProposalStore::new(dir.path().join("proposals.jsonl"));
        assert_eq!(
            reopened
                .file_candidates(vec![retract], 5, now)
                .await
                .unwrap()
                .suppressed_decided,
            1
        );
        assert!(reopened.pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn memory_lifecycle_profile_approval_replay_cannot_undo_later_retraction() {
        use crate::magician_v2::{
            artifact_v2::workspace::ArtifactV2Workspace,
            notes::NotesSettingsStore,
            taste_profile::{TasteProfileLoader, TasteProfileSettings},
        };
        let temp = tempfile::tempdir().unwrap();
        let loader = Arc::new(TasteProfileLoader::new(
            NotesSettingsStore::with_workspace_layout(ArtifactV2Workspace::new(temp.path())),
            TasteProfileSettings::default(),
        ));
        let service = TasteCaptureService::new(loader.clone(), temp.path().join("capture"));
        let store = service.store_for("p", "w").await;
        let now = Utc::now();
        let add = candidate("Prefer short answers", 0.95, now);
        store
            .file_candidates(vec![add.clone()], 5, now)
            .await
            .unwrap();
        service.approve("p", "w", &add.id, now).await.unwrap();
        assert!(loader
            .read_profile_note_raw("p", "w")
            .await
            .unwrap()
            .markdown
            .contains("Prefer short answers"));
        let mut retract = add.clone();
        retract.kind = ProposalKind::Retract;
        retract.proposed_at = now + Duration::seconds(1);
        store
            .file_candidates(vec![retract], 5, now + Duration::seconds(1))
            .await
            .unwrap();
        let pending = store.pending().await.unwrap();
        service
            .approve("p", "w", &pending[0].id, now + Duration::seconds(2))
            .await
            .unwrap();
        let retired = loader
            .read_profile_note_raw("p", "w")
            .await
            .unwrap()
            .markdown;
        assert!(!retired.contains("Prefer short answers"));
        service
            .approve("p", "w", &add.id, now + Duration::seconds(3))
            .await
            .unwrap();
        assert_eq!(
            loader
                .read_profile_note_raw("p", "w")
                .await
                .unwrap()
                .markdown,
            retired
        );
        assert!(loader.read_profile_note_raw("other", "w").await.is_none());
    }

    #[test]
    fn identity_survives_reformatting_but_not_negation() {
        // Whitespace and case must not mint a new identity, or a rejected
        // directive returns wearing a different coat.
        assert_eq!(
            proposal_id("Prefer  short\n sentences"),
            proposal_id("prefer short sentences")
        );
        // Negation must, or rejecting one suppresses its opposite.
        assert_ne!(
            proposal_id("Use em-dashes"),
            proposal_id("Never use em-dashes")
        );
    }

    #[tokio::test]
    async fn a_rejected_directive_is_never_offered_again() {
        let (store, _dir) = store();
        let now = Utc::now();
        let one = candidate("prefer short sentences", 0.9, now);
        store
            .file_candidates(vec![one.clone()], 5, now)
            .await
            .unwrap();
        store
            .decide(&one.id, ProposalStatus::Rejected, now)
            .await
            .unwrap();

        // The distiller re-derives the same directive next session.
        let outcome = store.file_candidates(vec![one], 5, now).await.unwrap();
        assert_eq!(outcome.filed, 0);
        assert_eq!(outcome.suppressed_decided, 1);
        assert!(store.pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_cap_counts_the_day_not_the_file() {
        let (store, _dir) = store();
        let now = Utc::now();
        let long_ago = now - Duration::days(30);
        // A month of history must not read as permanently over quota.
        let old: Vec<TasteProposal> = (0..10)
            .map(|i| candidate(&format!("old directive {i}"), 0.9, long_ago))
            .collect();
        store.file_candidates(old, 100, long_ago).await.unwrap();

        let fresh: Vec<TasteProposal> = (0..3)
            .map(|i| candidate(&format!("new directive {i}"), 0.9, now))
            .collect();
        let outcome = store.file_candidates(fresh, 5, now).await.unwrap();
        assert_eq!(
            outcome.filed, 3,
            "yesterday's proposals must not spend today's budget"
        );
        assert_eq!(outcome.suppressed_capped, 0);
    }

    #[tokio::test]
    async fn the_cap_refuses_the_overflow_only() {
        let (store, _dir) = store();
        let now = Utc::now();
        let many: Vec<TasteProposal> = (0..8)
            .map(|i| candidate(&format!("directive {i}"), 0.9, now))
            .collect();
        let outcome = store.file_candidates(many, 5, now).await.unwrap();
        assert_eq!(outcome.filed, 5);
        assert_eq!(outcome.suppressed_capped, 3);
    }

    #[tokio::test]
    async fn deciding_twice_keeps_the_first_decision_time() {
        let (store, _dir) = store();
        let now = Utc::now();
        let one = candidate("prefer short sentences", 0.9, now);
        store
            .file_candidates(vec![one.clone()], 5, now)
            .await
            .unwrap();

        let first = store
            .decide(&one.id, ProposalStatus::Approved, now)
            .await
            .unwrap()
            .expect("known id");
        let later = now + Duration::hours(2);
        let second = store
            .decide(&one.id, ProposalStatus::Approved, later)
            .await
            .unwrap()
            .expect("known id");
        assert_eq!(first.decided_at, second.decided_at);
        assert_eq!(second.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn an_unknown_id_is_none_not_an_invented_proposal() {
        let (store, _dir) = store();
        let decided = store
            .decide("nope", ProposalStatus::Approved, Utc::now())
            .await
            .unwrap();
        assert!(decided.is_none(), "the API must be able to answer 404");
    }

    #[test]
    fn a_torn_tail_is_not_loss_but_a_corrupt_line_is() {
        // A crash mid-append leaves an uncommitted fragment: no record was
        // ever complete, so nothing was lost.
        let torn = b"{\"id\":\"a\",\"directive\":\"x\",\"destination\":\"Voice\",\"confidence\":0.9,\"status\":\"pending\",\"source_session\":\"s\",\"proposed_at\":\"2026-08-13T00:00:00Z\"}\n{\"id\":\"b\",\"dir";
        let read = parse_proposals(torn);
        assert_eq!(read.proposals.len(), 1);
        assert!(read.torn_tail_bytes > 0);
        assert_eq!(read.corrupt_lines, 0, "a torn tail is not corruption");

        // A committed line that will not parse is real loss and must be
        // counted — silently skipping it re-proposes a rejected directive.
        let corrupt = b"{\"id\":\"a\",\"directive\":\"x\",\"destination\":\"Voice\",\"confidence\":0.9,\"status\":\"pending\",\"source_session\":\"s\",\"proposed_at\":\"2026-08-13T00:00:00Z\"}\n{not json}\n";
        let read = parse_proposals(corrupt);
        assert_eq!(read.proposals.len(), 1);
        assert_eq!(read.corrupt_lines, 1);
        assert_eq!(read.torn_tail_bytes, 0);
    }

    fn one(directive: &str, confidence: f32, evidence: &str) -> String {
        serde_json::json!({
            "candidates": [{
                "directive": directive,
                "evidence": if evidence.is_empty() { vec![] } else { vec![evidence] },
                "destination": "Voice",
                "confidence": confidence,
            }]
        })
        .to_string()
    }

    #[test]
    fn junk_from_the_model_costs_nothing() {
        // A hard failure here would stall the worker's watermark behind a
        // session it can never process, so junk must be survivable.
        for raw in ["", "not json", "{}", "{\"candidates\": \"nope\"}", "null"] {
            let (admitted, _) = admit_candidates(raw, 0.5, 5);
            assert!(admitted.is_empty(), "raw {raw:?} should admit nothing");
        }
    }

    #[test]
    fn an_unevidenced_directive_is_refused_not_shown_bare() {
        let (admitted, counts) = admit_candidates(&one("never use em-dashes", 0.99, ""), 0.5, 5);
        assert!(admitted.is_empty());
        assert_eq!(counts.no_evidence, 1);
    }

    #[test]
    fn an_over_long_directive_is_refused_never_truncated() {
        let long = "x".repeat(MAX_DIRECTIVE_CHARS + 1);
        let (admitted, counts) = admit_candidates(&one(&long, 0.99, "quote"), 0.5, 5);
        assert!(
            admitted.is_empty(),
            "truncating could invert the directive's meaning"
        );
        assert_eq!(counts.over_length, 1);
    }

    #[test]
    fn a_nan_confidence_fails_the_gate() {
        // `confidence < min` would admit NaN, since every comparison with NaN
        // is false. The gate is written as `!(>=)` for exactly this.
        let raw = "{\"candidates\":[{\"directive\":\"d\",\"evidence\":[\"q\"],\"destination\":\"Voice\",\"confidence\":null}]}";
        let (admitted, _) = admit_candidates(raw, 0.5, 5);
        assert!(admitted.is_empty());

        let (admitted, counts) = admit_candidates(&one("d", 0.2, "q"), 0.5, 5);
        assert!(admitted.is_empty());
        assert_eq!(counts.below_confidence, 1);
    }

    #[test]
    fn the_candidate_ceiling_is_enforced_on_the_model_not_trusted_from_it() {
        let many: Vec<serde_json::Value> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "directive": format!("directive {i}"),
                    "evidence": ["quote"],
                    "destination": "Voice",
                    "confidence": 0.9,
                })
            })
            .collect();
        let raw = serde_json::json!({ "candidates": many }).to_string();
        let (admitted, _) = admit_candidates(&raw, 0.5, 5);
        assert_eq!(admitted.len(), 5);
    }

    #[test]
    fn a_retraction_candidate_becomes_a_retraction_proposal() {
        let raw = serde_json::json!({
            "candidates": [{
                "kind": "retract",
                "directive": "keep it short",
                "evidence": ["actually write me the long version from now on"],
                "destination": "Voice",
                "confidence": 0.9,
            }]
        })
        .to_string();
        let (admitted, _) = admit_candidates(&raw, 0.5, 5);
        let proposals = into_proposals(admitted, &["Voice".to_string()], "s1", Utc::now());
        assert_eq!(proposals[0].kind, ProposalKind::Retract);
    }

    #[test]
    fn an_unknown_kind_degrades_to_addition_not_removal() {
        // A model inventing a third verb must not be able to produce a
        // proposal whose meaning nobody defined — and if it degrades, it must
        // degrade toward the reversible direction. An unwanted addition costs
        // one click; an unwanted removal costs the owner's writing.
        for kind in ["", "delete", "REMOVE", "retract-maybe", "🙂"] {
            let raw = serde_json::json!({
                "candidates": [{
                    "kind": kind,
                    "directive": "prefer short sentences",
                    "evidence": ["q"],
                    "destination": "Voice",
                    "confidence": 0.9,
                }]
            })
            .to_string();
            let (admitted, _) = admit_candidates(&raw, 0.5, 5);
            let proposals = into_proposals(admitted, &["Voice".to_string()], "s1", Utc::now());
            assert_eq!(
                proposals[0].kind,
                ProposalKind::Add,
                "kind {kind:?} must not be read as a removal"
            );
        }
    }

    #[test]
    fn an_invented_destination_lands_somewhere_real() {
        let raw = one("prefer short sentences", 0.9, "quote");
        let (admitted, _) = admit_candidates(&raw, 0.5, 5);
        let sections = vec!["Process".to_string(), "Boundaries".to_string()];
        let proposals = into_proposals(admitted, &sections, "s1", Utc::now());
        assert_eq!(proposals.len(), 1);
        assert!(
            sections.contains(&proposals[0].destination),
            "a model-invented heading must not create a section the owner never made"
        );
    }

    const NOTE: &str = "# My taste\n\n## Voice\n\n- keep it short\n\n## Process\n\n- test first\n";

    #[test]
    fn a_directive_lands_at_the_end_of_its_section() {
        let out = place_directive(NOTE, "never use em-dashes", "Voice").expect("inserted");
        let voice = out.split("## Process").next().unwrap();
        assert!(voice.contains("- keep it short"));
        assert!(voice.contains("- never use em-dashes"));
        assert!(
            out.contains("- test first"),
            "the other section is untouched"
        );
    }

    #[test]
    fn approving_twice_appends_once() {
        // The crash-ordering in approve() depends on this: the profile note is
        // written before the proposal is marked, so a crash between them
        // leaves the directive placed and the proposal still pending. The
        // owner approves again and must not get a duplicate.
        let once = place_directive(NOTE, "never use em-dashes", "Voice").expect("inserted");
        assert!(place_directive(&once, "never use em-dashes", "Voice").is_none());
    }

    #[test]
    fn owner_prose_is_never_rewritten_only_added_to() {
        let out = place_directive(NOTE, "new directive", "Voice").expect("inserted");
        // Every original line must survive, byte for byte and in order.
        let mut original = NOTE.lines();
        let mut current = original.next();
        for line in out.lines() {
            if Some(line) == current {
                current = original.next();
            }
        }
        assert!(
            current.is_none(),
            "an original line was altered or reordered"
        );
    }

    #[test]
    fn an_unknown_section_is_created_rather_than_guessed() {
        let out = place_directive(NOTE, "ship on fridays", "Boundaries").expect("inserted");
        assert!(out.contains("## Boundaries"));
        assert!(out.trim_end().ends_with("- ship on fridays"));
        // It must not have been quietly filed under an existing heading.
        let voice = out.split("## Process").next().unwrap();
        assert!(!voice.contains("ship on fridays"));
    }

    #[test]
    fn a_retraction_removes_exactly_one_line_and_nothing_else() {
        let out = remove_directive(NOTE, "keep it short").expect("removed");
        assert!(!out.contains("keep it short"));
        // Everything else survives byte for byte, including the now-empty
        // section's heading — an emptied section stays where it was rather
        // than being tidied away.
        assert!(out.contains("# My taste"));
        assert!(out.contains("## Voice"));
        assert!(out.contains("## Process"));
        assert!(out.contains("- test first"));
    }

    #[test]
    fn a_stale_retraction_refuses_rather_than_hunting_for_something_similar() {
        // The owner reworded it since the proposal was raised. Refusing is
        // correct: no fuzzy match, because the alternative is deleting a line
        // the owner wrote and we merely think resembles the target.
        assert_eq!(
            remove_directive(NOTE, "keep it shortish"),
            Err(RemovalRefusal::NotPresent)
        );
        assert_eq!(
            remove_directive(NOTE, "never written down"),
            Err(RemovalRefusal::NotPresent)
        );
    }

    #[test]
    fn a_duplicated_line_is_ambiguous_and_nothing_is_deleted() {
        // Deleting "the first one" is a coin flip with the owner's writing.
        let doubled = "## Voice\n\n- be terse\n\n## Process\n\n- be terse\n";
        let refusal = remove_directive(doubled, "be terse").unwrap_err();
        assert_eq!(refusal, RemovalRefusal::Ambiguous(2));
        // And the note is untouched, since the function returns Err rather
        // than a partially-edited string.
        assert_eq!(doubled.matches("- be terse").count(), 2);
    }

    #[test]
    fn add_then_retract_returns_the_note_to_where_it_started() {
        let added = place_directive(NOTE, "never use em-dashes", "Voice").expect("added");
        let back = remove_directive(&added, "never use em-dashes").expect("removed");
        assert_eq!(back, NOTE, "a round trip must not leave residue behind");
    }

    #[test]
    fn an_empty_note_still_takes_a_directive() {
        let out = place_directive("", "be terse", "Voice").expect("inserted");
        assert!(out.contains("## Voice"));
        assert!(out.contains("- be terse"));
    }

    #[tokio::test]
    async fn two_scopes_never_share_a_proposal_store() {
        // The first version shared one file across every scope, so one owner's
        // review queue listed proposals distilled from another owner's private
        // transcripts — and could approve them into their own profile.
        let dir = tempfile::tempdir().expect("tempdir");
        let a = TasteProposalStore::new(dir.path().join("scope-a.jsonl"));
        let b = TasteProposalStore::new(dir.path().join("scope-b.jsonl"));
        let now = Utc::now();
        a.file_candidates(vec![candidate("owner a's directive", 0.9, now)], 5, now)
            .await
            .unwrap();
        assert_eq!(a.pending().await.unwrap().len(), 1);
        assert!(
            b.pending().await.unwrap().is_empty(),
            "a second scope must not see the first's proposals"
        );
    }

    #[test]
    fn the_store_filename_cannot_be_steered_by_a_hostile_scope() {
        // principal and workspace are user-controlled strings. Interpolating
        // them into a filename is how one scope reaches another's file, so the
        // name is a digest and the raw values never touch the path.
        let digest = |principal: &str, workspace: &str| {
            let mut hasher = blake3::Hasher::new();
            for part in [principal, workspace] {
                hasher.update(&(part.len() as u64).to_le_bytes());
                hasher.update(part.as_bytes());
            }
            hasher.finalize().to_hex()[..24].to_string()
        };
        for (p, w) in [("../../etc", "x"), ("a/b", "c"), ("a", "b/../c"), ("", "")] {
            let name = digest(p, w);
            assert!(
                name.chars().all(|c| c.is_ascii_hexdigit()),
                "scope ({p:?}, {w:?}) produced a non-hex filename: {name}"
            );
        }
        // Length-prefixing keeps ("ab","c") and ("a","bc") distinct.
        assert_ne!(digest("ab", "c"), digest("a", "bc"));
    }

    #[tokio::test]
    async fn a_capped_day_costs_no_reads_and_no_model_calls() {
        // The sweep must refuse before it reads transcripts. Enabling capture
        // on an existing deployment otherwise means a 200-message read for
        // every session in history to produce at most a handful of proposals.
        let (store, _dir) = store();
        let now = Utc::now();
        let filled: Vec<TasteProposal> = (0..5)
            .map(|i| candidate(&format!("directive {i}"), 0.9, now))
            .collect();
        store.file_candidates(filled, 5, now).await.unwrap();
        let raised = store
            .read()
            .await
            .unwrap()
            .proposals
            .iter()
            .filter(|p| p.proposed_at >= now - Duration::days(1))
            .count();
        assert_eq!(raised, 5, "the cap is met, so a sweep must do nothing");
    }

    #[test]
    fn the_sweep_batch_is_bounded_independently_of_the_daily_cap() {
        // A session can yield no proposals, so the proposal cap alone does not
        // bound how many sessions a pass will read and distil.
        assert!(MAX_SESSIONS_PER_SWEEP > 0);
        assert!(
            MAX_SESSIONS_PER_SWEEP <= 10,
            "a sweep that reads more than a handful of transcripts holds the \
             worker for as long as the backlog is deep"
        );
    }

    #[test]
    fn the_mirror_says_it_is_not_read_back() {
        // The note looks editable, sits in the owner's notes app next to notes
        // that *are* authoritative, and is silently overwritten on the next
        // refresh. Saying so in the note is the only place the owner will see
        // it before losing an edit.
        let empty = render_proposals_note(&[]);
        assert!(empty.contains("never read back"));

        let now = Utc::now();
        let one = candidate("prefer short sentences", 0.9, now);
        let rendered = render_proposals_note(&[one]);
        assert!(rendered.contains("prefer short sentences"));
        assert!(
            rendered.contains("because I said so"),
            "evidence must reach the mirror"
        );
        assert!(rendered.contains("never injected"));
    }

    #[test]
    fn only_the_owners_words_reach_the_distiller() {
        use crate::magician_v2::chat::models::{
            ChatMessage, ChatMessageContent, ChatMessageDirection,
        };
        let text = |id: &str, body: &str| {
            ChatMessage::new(
                id,
                "s1",
                ChatMessageDirection::User,
                ChatMessageContent::Text {
                    text: body.to_string(),
                    plan_reply: None,
                },
                0,
            )
        };
        let transcript =
            transcript_from_messages(&[text("1", "always run the tests first"), text("2", "   ")]);
        assert!(transcript.contains("always run the tests first"));
        // A blank turn contributes nothing rather than an empty labelled line,
        // which would spend the model's attention on structure with no content.
        assert_eq!(transcript.matches("User").count(), 1);
    }

    #[test]
    fn execution_turns_become_a_transcript_the_same_way_chat_does() {
        use crate::magician_v2::storage::models::{TurnDirection, V2Turn};
        let turn = |text: &str| V2Turn {
            id: "t".into(),
            execution_id: "e1".into(),
            direction: TurnDirection::Inbound,
            text: text.into(),
            in_reply_to_slot_id: None,
            created_at: 0,
            query_analysis: None,
            analysis_metadata: None,
            strategy_attempts: Vec::new(),
            processing_metadata: None,
            recommended_questions: None,
            enriched_query: None,
        };
        let transcript = transcript_from_turns(&[turn("always run the tests first"), turn("   ")]);
        assert!(transcript.contains("always run the tests first"));
        // A blank turn contributes nothing, matching the chat path.
        assert_eq!(transcript.matches("Inbound").count(), 1);
    }

    #[test]
    fn a_legacy_watermark_file_migrates_without_re_distilling() {
        // An existing deployment's watermark is one flat list. It must split
        // by kind on read, not be discarded — discarding it would re-distil
        // every session and execution the deployment had already handled.
        let mut mark = CaptureWatermark {
            processed: vec![
                "chat-1".to_string(),
                "exec:run-1".to_string(),
                "chat-2".to_string(),
            ],
            ..Default::default()
        };
        mark.migrate_legacy();
        assert!(
            mark.processed.is_empty(),
            "legacy list is consumed, not kept"
        );
        assert_eq!(mark.chat, vec!["chat-1", "chat-2"]);
        assert_eq!(mark.executions, vec!["exec:run-1"]);
        // Everything previously handled is still considered seen.
        let seen = mark.seen();
        for id in ["chat-1", "chat-2", "exec:run-1"] {
            assert!(seen.contains(id), "{id} must survive migration");
        }
        // Idempotent: a second pass changes nothing.
        let before = mark.seen();
        mark.migrate_legacy();
        assert_eq!(mark.seen(), before);
    }

    #[test]
    fn one_busy_source_cannot_evict_the_other_s_history() {
        // A shared cap let a busy execution scope evict chat ids, so chat
        // sessions were re-distilled — wasted model calls that grow with
        // activity.
        let mut mark = CaptureWatermark::default();
        mark.chat.push("chat-keeper".to_string());
        for i in 0..(WATERMARK_KEEP * 2) {
            mark.executions.push(format!("exec:{i}"));
        }
        for list in [&mut mark.chat, &mut mark.executions] {
            if list.len() > WATERMARK_KEEP {
                let drop = list.len() - WATERMARK_KEEP;
                list.drain(..drop);
            }
        }
        assert!(
            mark.seen().contains("chat-keeper"),
            "a flood of executions must not evict chat history"
        );
        assert_eq!(
            mark.executions.len(),
            WATERMARK_KEEP,
            "each list is bounded"
        );
    }

    #[test]
    fn neither_source_can_starve_the_other() {
        // Chat-first-takes-all meant executions were never distilled on any
        // deployment with steady chat activity — wired and never once run.
        let split = |batch: usize| {
            let chat = batch.div_ceil(2);
            // Worst case for executions: chat fills its whole share.
            (chat, batch.saturating_sub(chat))
        };
        for batch in 1..=MAX_SESSIONS_PER_SWEEP {
            let (chat, exec) = split(batch);
            assert!(chat > 0, "chat starved at batch {batch}");
            if batch > 1 {
                assert!(exec > 0, "executions starved at batch {batch}");
            }
            assert_eq!(chat + exec, batch, "the batch must be fully allocated");
        }
        // A quiet chat scope hands its unused slots to executions.
        let batch = MAX_SESSIONS_PER_SWEEP;
        let chat_used = 0usize;
        assert_eq!(batch.saturating_sub(chat_used), batch);
    }

    #[test]
    fn execution_and_chat_watermarks_cannot_collide() {
        // The processed set holds both sources. Chat ids stay bare and
        // executions are prefixed: prefixing one side is enough to
        // disambiguate, and changing the chat format would re-distil every
        // session a deployment had already processed.
        let shared_id = "abc123";
        assert_ne!(execution_watermark_key(shared_id), shared_id);
        assert!(execution_watermark_key(shared_id).starts_with("exec:"));
    }

    #[test]
    fn capture_settings_are_only_validated_while_capture_is_on() {
        use crate::magician_v2::taste_profile::TasteProfileSettings;
        // Off: nonsense values are tolerated, because turning a feature off
        // must never require first repairing settings nothing will read.
        let off = TasteProfileSettings {
            capture_enabled: false,
            max_proposals_per_day: 0,
            min_confidence: 9.0,
            ..Default::default()
        };
        assert!(off.validate().is_ok());

        // On: a zero cap is refused rather than silently meaning "never",
        // because `capture_enabled: false` is how you say that legibly.
        let zero_cap = TasteProfileSettings {
            capture_enabled: true,
            max_proposals_per_day: 0,
            ..Default::default()
        };
        assert!(zero_cap.validate().is_err());

        let bad_confidence = TasteProfileSettings {
            capture_enabled: true,
            min_confidence: 1.5,
            ..Default::default()
        };
        assert!(bad_confidence.validate().is_err());

        let sane = TasteProfileSettings {
            capture_enabled: true,
            ..Default::default()
        };
        assert!(sane.validate().is_ok());
    }

    #[tokio::test]
    async fn the_same_directive_from_two_sessions_queues_once() {
        let (store, _dir) = store();
        let now = Utc::now();
        let one = candidate("prefer short sentences", 0.9, now);
        let mut again = candidate("Prefer short sentences.", 0.95, now);
        // Punctuation differs, so this is genuinely a different id — the
        // duplicate case needs the *same* normalized text.
        again.id = proposal_id("prefer  short   sentences");
        again.directive = "prefer  short   sentences".to_string();

        store.file_candidates(vec![one], 5, now).await.unwrap();
        let outcome = store.file_candidates(vec![again], 5, now).await.unwrap();
        assert_eq!(outcome.filed, 0);
        assert_eq!(outcome.suppressed_duplicate, 1);
        assert_eq!(store.pending().await.unwrap().len(), 1);
    }
}
