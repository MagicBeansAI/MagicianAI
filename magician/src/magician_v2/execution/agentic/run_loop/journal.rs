//! What happened, in the order it happened, and how far of it is authoritative.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`. A worker cannot be
//! handed the loop's continuation, so it has to be handed a **record** of where
//! the loop got to. This is that record: an append-only log of phase
//! transitions, the events a phase wants emitted, and the owner changes that
//! move authority mid-run.
//!
//! # The watermark, not the log, is authoritative
//!
//! `append_journal` and `commit` are two operations, so a worker that appends
//! and then dies leaves records **ahead of** the committed state. Replaying
//! "snapshot + journal tail" would apply work nobody committed.
//!
//! The rule the design settles on, and the one this module enforces:
//! [`LoopState::journal_seq`](super::state::LoopState::journal_seq) is the
//! watermark. Records at or below it are authoritative and replayable. Records
//! beyond it are an orphaned attempt — never replayed, never projected, swept on
//! the next commit. [`Journal::authoritative`] is the only way to get records out
//! of this module for replay, and it takes the watermark as an argument
//! precisely so no caller can forget it.
//!
//! An earlier draft of the design said "the journal is authoritative, snapshots
//! are a cache". That is wrong as stated and would have made an orphaned attempt
//! authoritative. Snapshots are a cache; replay is bounded by the watermark.
//!
//! # Events are journaled, not emitted — the rule, and how much of it is built
//!
//! A phase that emitted inline would emit again every time it re-ran after a
//! crash. So the rule is that a phase appends the event it wants and a projector
//! emits it — and dedupe is by [`EventKey`], **not** by seq, because a re-run
//! appends a *new* record at a *new* seq. `event_key = (execution_id, iteration,
//! phase, ordinal)` is deterministic, so the re-run's record carries the same
//! address and the projector recognises it.
//!
//! **Built here:** [`JournalBody::Event`] is a record kind, [`JournalAppend::event`]
//! is how a producer makes one without being able to write a record the store
//! would refuse, and [`ProjectorCursor::project`] is the emitting loop —
//! watermark-bounded, address-deduped, advancing its mark one record at a time so
//! a refused emit cannot bury the records beneath it.
//!
//! **Producer, drain and projector are live on the stateless arm.** Each
//! produced event has one delivery path:
//!
//! - **Producer: built, in `phases::outbox`.** All nine direct
//!   `ActionExecutors::emit_event` sites across the phases — `decide` 5,
//!   `epilogue` 2 (`AgenticStepStuckWarning`, `AgenticIterationCompleted`),
//!   `prepare` 1 (`AgenticIterationStarted`), `apply` 1
//!   (`AgenticWaitingForUser`) — now go through `outbox::journal_and_emit`,
//!   which builds a [`JournalBody::Event`] through [`JournalAppend::event`]. An
//!   accepted record is projector-only; an unaddressed, oversized, capacity-
//!   refused, or in-process-arm event falls back to inline delivery.
//! - **Drain: BUILT, in `executor.rs`.**
//!   `InProcessWorkerHost::run_phase` runs
//!   `report.records.extend(phases::outbox::take(execution_id))` before it
//!   pushes any owner-transition record — before, because `commit_boundary`
//!   stamps ordinals by index and extending after the push would shift every
//!   event's address on exactly the attempts that had a handover — and the same
//!   file drains any leftover at phase entry. So the records the producer makes
//!   now reach a `PhaseReport`, an append and the watermark on every boundary.
//!   They are **not** discarded.
//! - **A second record kind landed 2026-08-28: [`JournalBody::NamedEvent`].**
//!   `RuntimeTransportBroadcaster::emit_named` performs a chat fan-out before it
//!   reaches a transport event, so a record holding the finished event replays a
//!   strictly narrower delivery than the producer performed. That variant holds
//!   the CALL instead, and five more sites — `plan.step.started`,
//!   `plan.step.finished` (×2 branches) and `tool.result.projected` — journal
//!   through it. [`ProjectorCursor::project`] emits it beside an
//!   [`JournalBody::Event`], deduped on the same address, out of the same
//!   window.
//! - **Further emission paths reach the transports and are NOT journalled at
//!   all**, so a projected replay restores most of a run's timeline and not
//!   all of it.
//!
//!   **How many is `phases::outbox`'s to say, and this line deliberately does
//!   not repeat it.** It carried a number twice and was the stale copy twice —
//!   it said *thirty-seven* while the census said *fourteen*, then *ten of
//!   fifty-one* while the census was being re-counted to something else again.
//!   The figure has risen on every re-sweep without any code regressing,
//!   because each sweep reached emitters the previous method could not see, so
//!   a copy here is a copy that goes wrong on a schedule. See
//!   `phases::outbox`'s *WHAT IS NOT JOURNALLED*, which carries the list, the
//!   greps that reproduce it, and the *METHOD* section saying what the sweep
//!   still cannot reach.
//!
//!   What is stable enough to state here, because it is a fact about this
//!   file's record kinds rather than about a population: most of them build
//!   their event inside a helper where the call site cannot see the value, so
//!   rebuilding it at the call site — the second event vocabulary this design
//!   refuses — is what a naive fix would do. Two of them do not produce a
//!   `RuntimeTransportEvent` at all and are **deliberately left
//!   unjournalled**: they write straight to the canonical event log, so a
//!   projected replay would append a second record for one artifact rather
//!   than rescue a lost one.
//! - **Projector: active, and its mark is durable.**
//!   `driver_worker::commit_boundary` runs one projection pass after every
//!   commit it lands, bounded by the watermark that commit published, and
//!   [`LoopStateStore::save_projector_cursor`](super::store::LoopStateStore::save_projector_cursor)
//!   persists the mark under its own key. `InProcessWorkerHost` opts into
//!   projected delivery and implements both routed transport events and named
//!   events. The trait default remains `false` for hosts that do not provide
//!   those sinks.
//!
//! The drain could not be built *from this module*, which is why it arrived from
//! another one: the only assembler of a `driver_worker::PhaseReport` — the one
//! channel by which a phase's records reach an append — is
//! `executor.rs::InProcessWorkerHost::run_phase`, and the only loop that observes
//! a commit land is `executor.rs::StatelessArm::advance_iteration`.
//!
//! # What is deliberately not here
//!
//! Effects. The ledger in [`super::effects`] owns them, keyed by `effect_id`,
//! because parallel batch members settle out of order and a status map answers
//! "did this one fire" in one read where a log scan would not.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::outcome::{BoundaryOutcome, Phase};
use super::state::{LoopState, LoopStateRefusal, Placement};
use crate::magician_v2::execution::agentic::types::AgenticOutcome;

/// The largest one journal record may serialize to.
///
/// Enforced on **write** so a record can never be produced that a bounded read
/// would refuse, and on **read** so a file written by something else — an older
/// build, a hand edit, a corruption — cannot make a reader allocate without
/// limit. A bound checked on only one side is a bound that fails exactly when it
/// is needed.
///
/// It is also the read-side half of the same rule: a line over the bound is
/// reported as corruption rather than parsed, so a damaged file cannot make a
/// reader take a record no writer could have produced. The filesystem store
/// reads the journal whole rather than scanning a window back from its end — see
/// the `journal_last_seq` note in `store::fs` — which is why the per-line bound,
/// and not a tail window, is what keeps a parse bounded.
pub const MAX_JOURNAL_RECORD_BYTES: usize = 64 * 1024;

/// The largest a journal file may grow before a read refuses it.
pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

/// The most records one read may return.
///
/// A ceiling on the *reader*, not on the run: an execution that legitimately
/// exceeds this is telling us the snapshot cadence is wrong, which is a
/// configuration answer rather than a reason to load an unbounded vector.
pub const MAX_JOURNAL_RECORDS: usize = 200_000;

/// The longest retry delay a record may carry, in milliseconds.
///
/// # Why a delay needs a ceiling at all
///
/// [`RecordedBoundary::Retry`] fails toward *waiting*, never toward re-firing,
/// which is the safe direction and was the reason this went unbounded. It is the
/// right reading of the direction and the wrong reading of the consequence:
/// under a stateless driver the delay is read off disk into
/// `LoopState::runnable_at_ms`, and a record carrying an absurd value parks a
/// run effectively forever. A wedged execution is not a safe failure just
/// because it is not a duplicate send — nobody is waiting on a duplicate send,
/// and somebody is waiting on this run.
///
/// # Where it is enforced, and why differently on each side
///
/// **On the write side it clamps**, in `From<BoundaryOutcome>`. That conversion
/// already narrows — it saturates a delay too large for `u64` milliseconds
/// rather than truncating it — so clamping to a value a reader will accept is
/// the same act with a bound that means something. Refusing there would fail a
/// live run for a condition the runtime can correctly narrow, which is the
/// trade the owner-transition intersection makes for the same reason.
///
/// **On the read side it refuses**, at parse, naming the seq. A record over this
/// ceiling cannot have been produced by the clamp above, so it is corruption, a
/// hand edit, or a foreign writer — and the point of refusing at parse rather
/// than at use is that the cause is visible where it enters rather than three
/// phases later in a run that will not wake.
///
/// # The value
///
/// One hour. Both production producers of a `Retry` — `phases/resolve.rs` and
/// `phases/apply.rs`, and the exhaustiveness gate in `outcome.rs` asserts there
/// are exactly two — take their delay from `transient_retry_backoff`
/// (`executor.rs`), whose last line is `.min(30_000)`. So the largest delay any
/// phase can ask for is 30 seconds, and this ceiling is 120× that. It bounds
/// nothing a phase does today; it bounds what a reader will believe.
pub const MAX_RETRY_AFTER_MS: u64 = 60 * 60 * 1_000;

/// Which record this is, for a projector that must not emit twice.
///
/// The persisted envelope for a projector identity. New projectors fold a body
/// fingerprint into `execution_id` and use `ordinal` as the identical-body
/// occurrence within one append batch; legacy cursors contain the historical
/// positional form. Both remain readable through the same wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventKey {
    /// **The journal's key, which is a loop-state address — not always an
    /// execution id.** Named `execution_id` because it was one when this was
    /// written and because the name is on disk: `ProjectorCursor` persists its
    /// `recent_keys` through `ProjectorCursorWire`, so renaming the field would
    /// orphan every saved cursor.
    ///
    /// It is whatever the projection was given —
    /// `driver_worker::project_outbox` passes `key.execution_id()`, and
    /// `executor.rs::StatelessArm::new` builds that key from
    /// `loop_state_address`, which appends `-r{n}` / `-n{…}` / `-p{n}` for a
    /// resume generation, a nesting chain and a refinement pass. Correct for
    /// dedupe, which is the only thing this type is for: the address is
    /// per-journal, and per-journal is exactly what keeps two segments of one
    /// execution from colliding.
    ///
    /// **Do not read identity out of it.** See
    /// [`RecordedCanonicalScope`]'s *THE FILE KEY IS NOT THE EXECUTION ID*.
    pub execution_id: String,
    pub iteration: usize,
    pub phase: Phase,
    /// Distinguishes several records from one phase of one iteration. The
    /// caller assigns it in the order it appends, starting at zero.
    pub ordinal: u32,
}

impl EventKey {
    /// Stable, bounded identity for a host-owned projection of this record.
    ///
    /// The persisted fields are framed before hashing, so delimiters inside the
    /// loop-state address cannot create an ambiguous preimage. The reference is
    /// intentionally distinct from a canonical event id: a failed downstream
    /// observer can cause the canonical append to be offered again, while every
    /// offer still carries this same source identity for idempotency.
    pub fn source_event_ref(&self) -> String {
        fn hash_framed(hasher: &mut blake3::Hasher, value: &[u8]) {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value);
        }

        let iteration = self.iteration.to_string();
        let phase = self.phase.to_string();
        let ordinal = self.ordinal.to_string();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"magician.stateless.source-event.v1\0");
        hash_framed(&mut hasher, self.execution_id.as_bytes());
        hash_framed(&mut hasher, iteration.as_bytes());
        hash_framed(&mut hasher, phase.as_bytes());
        hash_framed(&mut hasher, ordinal.as_bytes());
        format!("evt_loop_v1_{}", hasher.finalize().to_hex())
    }
}

impl fmt::Display for EventKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.execution_id, self.iteration, self.phase, self.ordinal
        )
    }
}

/// A [`BoundaryOutcome`] as it survives to disk.
///
/// Deliberately a separate type rather than `Serialize` on `BoundaryOutcome`
/// itself. That enum is a **checked vocabulary**: the exhaustiveness gate in
/// `outcome.rs` compares its variant names against the `// EXIT:` notes in the
/// loop, and `Retry` carries a `Duration` whose serde form (`{secs, nanos}`) is
/// not the shape a log line should carry. Mirroring it keeps the on-disk format
/// free to change without touching a type whose spelling a source-scanning test
/// depends on.
///
/// The conversion is exhaustive in both directions, so a new variant on
/// `BoundaryOutcome` fails to compile here rather than serializing as something
/// approximate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum RecordedBoundary {
    Advance,
    NextIteration,
    /// The delay the phase asked for, in milliseconds.
    ///
    /// A required field of the variant rather than an `Option` on the record, so
    /// "no delay recorded" cannot be confused with "delay of zero". A retry with
    /// no wait is `Retry { after_ms: 0 }` and says so.
    Retry {
        after_ms: u64,
    },
    PopFrame,
}

impl From<BoundaryOutcome> for RecordedBoundary {
    fn from(outcome: BoundaryOutcome) -> Self {
        match outcome {
            BoundaryOutcome::Advance => RecordedBoundary::Advance,
            BoundaryOutcome::NextIteration => RecordedBoundary::NextIteration,
            // Saturating rather than `as`: a delay longer than u64 milliseconds
            // is not reachable, but truncation would turn a long backoff into a
            // short one, which is the failure this whole variant exists to
            // prevent.
            //
            // Then clamped to [`MAX_RETRY_AFTER_MS`], so the write side can never
            // produce a record the read side refuses. Saturating to `u64::MAX`
            // and stopping there would have done exactly that.
            BoundaryOutcome::Retry(after) => RecordedBoundary::Retry {
                after_ms: u64::try_from(after.as_millis())
                    .unwrap_or(u64::MAX)
                    .min(MAX_RETRY_AFTER_MS),
            },
            BoundaryOutcome::PopFrame => RecordedBoundary::PopFrame,
        }
    }
}

impl From<RecordedBoundary> for BoundaryOutcome {
    fn from(recorded: RecordedBoundary) -> Self {
        match recorded {
            RecordedBoundary::Advance => BoundaryOutcome::Advance,
            RecordedBoundary::NextIteration => BoundaryOutcome::NextIteration,
            RecordedBoundary::Retry { after_ms } => {
                BoundaryOutcome::Retry(Duration::from_millis(after_ms))
            },
            RecordedBoundary::PopFrame => BoundaryOutcome::PopFrame,
        }
    }
}

/// Which way a run ended, without the payload that ended it.
///
/// `AgenticOutcome` derives `Serialize` and **not** `Deserialize`, and adding it
/// would mean making eleven variants' worth of payload — environments, message
/// logs, child execution sets — round-trip through a log line. The journal does
/// not need that: the outcome itself is already published through the execution's
/// own result path. What the journal needs is *that the run ended and which way*,
/// so a replay stops at the right seq and a reader can tell a pause from a
/// failure.
///
/// The `From` impl is an exhaustive match, so a new `AgenticOutcome` variant is a
/// build error here rather than a silent misclassification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Success,
    Failed,
    MaxIterationsReached,
    LoopDetected,
    WaitingForUser,
    WaitingForConfirmation,
    PausedByUser,
    WaitingForChildren,
    BudgetExhausted,
    CannotProceed,
    /// This durable segment stopped because its exact continuation checkpoint
    /// was handed to a different loop-state address. The execution may remain
    /// live there, but this segment must never be entered again.
    HandedOff,
    Sleeping,
}

impl TerminalKind {
    /// Stable receipt spelling. This is separate from `Debug` and from Serde's
    /// representation so neither a formatting edit nor a wire rename can make
    /// an already-committed cross-layer proof name a different ending.
    pub const fn settlement_label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::MaxIterationsReached => "max_iterations_reached",
            Self::LoopDetected => "loop_detected",
            Self::WaitingForUser => "waiting_for_user",
            Self::WaitingForConfirmation => "waiting_for_confirmation",
            Self::PausedByUser => "paused_by_user",
            Self::WaitingForChildren => "waiting_for_children",
            Self::BudgetExhausted => "budget_exhausted",
            Self::CannotProceed => "cannot_proceed",
            Self::HandedOff => "handed_off",
            Self::Sleeping => "sleeping",
        }
    }

    /// Whether the run can be picked up again from this ending.
    ///
    /// A pause, a confirmation and a wait are all *segment-resumable*: state
    /// stays in this LoopState and something outside answers. Max-iteration or
    /// budget exhaustion ends this exact segment even when its bounded terminal
    /// settlement receipt carries a separate Artifact pause generation for a
    /// later segment. This predicate therefore governs loop reachability only;
    /// it does not claim that the outer product execution can never resume.
    pub const fn is_resumable(self) -> bool {
        matches!(
            self,
            TerminalKind::WaitingForUser
                | TerminalKind::WaitingForConfirmation
                | TerminalKind::PausedByUser
                | TerminalKind::WaitingForChildren
                | TerminalKind::Sleeping
        )
    }
}

impl From<&AgenticOutcome> for TerminalKind {
    fn from(outcome: &AgenticOutcome) -> Self {
        match outcome {
            AgenticOutcome::Success { .. } => TerminalKind::Success,
            AgenticOutcome::Failed { .. } => TerminalKind::Failed,
            AgenticOutcome::MaxIterationsReached { .. } => TerminalKind::MaxIterationsReached,
            AgenticOutcome::LoopDetected { .. } => TerminalKind::LoopDetected,
            AgenticOutcome::WaitingForUser { .. } => TerminalKind::WaitingForUser,
            AgenticOutcome::WaitingForConfirmation { .. } => TerminalKind::WaitingForConfirmation,
            AgenticOutcome::PausedByUser { .. } => TerminalKind::PausedByUser,
            AgenticOutcome::WaitingForChildren { .. } => TerminalKind::WaitingForChildren,
            AgenticOutcome::BudgetExhausted { .. } => TerminalKind::BudgetExhausted,
            AgenticOutcome::CannotProceed { .. } => TerminalKind::CannotProceed,
            AgenticOutcome::Sleeping { .. } => TerminalKind::Sleeping,
        }
    }
}

/// What a phase did, in the vocabulary [`super::outcome::PhaseStep`] already
/// speaks.
///
/// The three variants are not degrees of the same thing, and conflating two of
/// them is the mistake `outcome.rs` documents at length: **a boundary exit never
/// ends a run**. It leaves the iteration, the epilogue runs, and the next
/// iteration starts. Ending the run is [`RecordedStep::RunEnded`]. A replay that
/// read one as the other would either resume a finished run or abandon a live
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RecordedStep {
    /// The phase produced its output and the iteration continues to the next
    /// phase.
    Continued,
    /// The phase ended the iteration. The run continues.
    Exited { boundary: RecordedBoundary },
    /// The phase ended the run.
    RunEnded { terminal: TerminalKind },
}

/// The scope half of what `ActionExecutors::emit_event` decides from, as it
/// survives to disk.
///
/// # It carries four of the five fields, and the missing one is deliberate
///
/// `CanonicalEventScope` has five fields; this has four. `execution_id` is
/// **not** recorded, because it is a run-level constant and writing it once per
/// line would pay for it per record to learn nothing — the same reasoning that
/// makes [`JournalRecord::event_key`] take the id from its caller rather than
/// from the record.
///
/// # THE FILE KEY IS NOT THE EXECUTION ID — corrected 2026-08-28
///
/// An earlier version of this section said the id was safe to omit *"because
/// the journal file already names it: every reader of a record gets it from the
/// file's own `ExecutionKey`"*, and told a reader to rebuild the fifth field
/// from that key. **Both halves are false in production**, and the second is an
/// instruction to fabricate.
///
/// A journal's `ExecutionKey` carries a **loop-state address**, not an execution
/// id. `executor.rs::StatelessArm::new` keys it with
/// `loop_state_address(ctx, execution_id)`, and
/// `executor.rs::loop_state_execution_id` appends `-r{n}` for a resume
/// generation, `-n{…}` for a nesting chain, and `-p{n}` for a refinement pass.
/// One execution owns as many journals as it has segments:
///
/// - a **resumed** invocation runs under `{exec}-r{offset}`;
/// - a **sub-goal** and an **in-context delegation** push a continuation frame
///   while deliberately keeping the parent's `ctx.execution_id`
///   (`executor.rs::run_single_delegate_in_context`,
///   `executor.rs::handle_spawn_sub_goal_decision`), so they run under
///   `{exec}-n{…}`;
/// - a **refinement pass** runs under `{exec}-p{n}`.
///
/// On every one of those the file key is a string no execution was ever
/// registered under. `driver_worker::project_outbox` hands
/// [`ProjectorCursor::project`] exactly that key
/// (`cursor.project(&journal, key.execution_id(), …)`), so a sink that took the
/// fifth field from there would mint `{exec}-r1` as an execution id and attach
/// this run's real principal, workspace, task and thread to it.
///
/// That value is correct for **dedupe**, which is all [`EventKey`] uses it for
/// and all any reader uses it for today: an address is per-journal, and a
/// per-journal key is exactly what makes two segments of one execution unable to
/// collide. It is wrong for **identity**.
///
/// So: a reader rebuilding a `CanonicalEventScope` from this must supply the
/// execution id from the RUN — the id the producing context was addressed by —
/// and must not derive it from the journal's key or from
/// [`EventKey::execution_id`]. [`Self::into_scope_fields`] is the shape of the
/// four names, kept here rather than in the reader so they are written once; it
/// deliberately does not offer the fifth, because this format cannot supply one
/// and a reader that has no honest source for it has no business rebuilding a
/// scope at all.
///
/// ~~**Nothing implements that reader yet.**~~ — **IT DOES, since 2026-08-28.**
/// `driver_worker::HostEventSink` overrides `emit_routed`, and
/// `executor.rs::InProcessWorkerHost::emit_projected_runtime_fact` is the
/// reader: it calls [`Self::into_scope_fields`] for the four names and takes
/// the fifth from `runtime_execution_id_opt(self.ctx)` — the RUN's id — never
/// from the key it was handed. A run with no execution id gets a warning and a
/// dropped event rather than a fabricated one.
///
/// The struck-through sentence is kept because it was true when written and a
/// reader who finds it quoted elsewhere needs to know which way it resolved.
/// **This section is still the reason that reader must not be rewritten
/// against the file key**, which is the durable half of it.
///
/// # Recorded once the run is under way, and constant after that
///
/// The value is `ActionExecutors::canonical_event_scope`, which is set by
/// `with_canonical_event_scope` **before** the executors are put in their `Arc`
/// and is never reachable for mutation afterwards. So it is a run-level
/// constant, and a record of it is exact for the whole run rather than a
/// snapshot that drifts — which is the property that makes recording it per
/// event honest rather than merely redundant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedCanonicalScope {
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    /// UI thread id, for chat-surface routing. Empty when the producer had no
    /// thread — `CanonicalEventScope` carries an empty string for that case
    /// rather than an `Option`, and this mirrors it exactly rather than
    /// inventing a second spelling of absent.
    pub ui_thread_id: String,
}

impl RecordedCanonicalScope {
    /// The four recorded names, in the order a `CanonicalEventScope` takes them.
    ///
    /// Returned as a tuple rather than as a `CanonicalEventScope` on purpose:
    /// this module must not depend on `artifact_v2::events`, because the journal
    /// is the durable format and a type it re-exported would make the format's
    /// compatibility a property of somebody else's struct. The caller that owns
    /// the transport owns the construction, and supplies the execution id.
    ///
    /// **Not from the journal's key, and not from [`EventKey::execution_id`].**
    /// Both carry a loop-state address, which is the execution id only for an
    /// invocation that was never resumed, never nested and is not a refinement
    /// pass. See the type docs' *THE FILE KEY IS NOT THE EXECUTION ID*.
    pub fn into_scope_fields(self) -> (String, String, String, String) {
        (
            self.principal,
            self.workspace,
            self.task_id,
            self.ui_thread_id,
        )
    }
}

/// Which of the two things `ActionExecutors::emit_event` does to an event, as
/// the record remembers it.
///
/// # The decision this exists to carry, spelled out
///
/// `emit_event` (`executor.rs`) does not put an event on one rail. It
/// chooses, per event, between a **canonical runtime fact** — which persists —
/// and a **transport-only** send, which reaches live surfaces and nothing else.
/// It chooses from three fields of `ActionExecutors`: `event_broadcaster`,
/// `canonical_event_sink` and `canonical_event_scope`. A projector holds none of
/// them.
///
/// Without this field a sink built on `phases::outbox::rejoin` alone takes every
/// record down the transport-only branch: chat activity, the deep-work panel and
/// `/debug` timelines all keep working, and the persisted runtime-fact stream
/// stops. Nothing looks wrong, which is what makes it the worst available shape
/// of failure and why the decision is recorded rather than re-derived.
///
/// # Why the DECISION and not the inputs
///
/// Re-deriving needs `map_v2_realtime_event` — a pure function of the event, so
/// recoverable — **and** the run's scope, which is not. Recording the scope
/// alone would still leave the comparison to be re-implemented at every reader,
/// and a second implementation of a rule is the thing that drifts. So the
/// producer records what it decided, and [`Self::CanonicalRuntimeFact`] carries
/// the scope as well, because a **foreign** process replaying this journal has
/// an empty scope registry and cannot look one up: without the scope on the
/// record a foreign replay can name the decision and still not act on it.
///
/// # What it does NOT promise
///
/// - **Not that anything was sent, on either variant.** This is the
///   CLASSIFICATION of the event, computed by `phases::outbox::routing_for` —
///   not an observation of what `emit_event` then managed to do with it.
///
///   **It is not computed from the same three fields**, and an earlier version
///   of this bullet said it was. `routing_for` is handed exactly ONE of them,
///   `canonical_event_scope` (`phases::outbox::journal` passes
///   `executors.canonical_event_scope.as_ref()` and nothing else, deliberately:
///   "a parameter that could reach a transport would invite one that did"), plus
///   the event, plus a fourth input `emit_event` has no analogue for — the id of
///   the run whose journal the record goes in, which is what
///   [`Self::Unrecorded`]'s mismatch arm compares. `event_broadcaster` and
///   `canonical_event_sink` are **not** read, and that is not an omission: they
///   are the two that decide whether anything is *sent*, which is precisely what
///   this bullet says the field does not promise. Reading them would make the
///   record say "delivered", a claim that stops being true the moment it is
///   replayed in a process holding different transports.
///
///   `emit_event` multiplies the
///   classification by the transports the producing process happened to hold,
///   and **both** answers have a shape that sends nothing at all:
///   [`Self::CanonicalRuntimeFact`] with no `canonical_event_sink` and no
///   broadcaster, and [`Self::TransportOnly`] with no broadcaster, which falls
///   to the canonical-sink branch (`emit_event`'s three-way `if let`) whose scope
///   comparison then drops the event. Recording "not sent" would tell a
///   replayer the event was not a runtime fact, which is false — it was one, and
///   the producer had nowhere to put it. This is the division that survives the
///   event being replayed somewhere else, which is the only one worth writing
///   down.
/// - **Not which rail.** A record says the event was a canonical runtime fact,
///   not that a broadcaster carried it. A process replaying with a different set
///   of transports delivers it differently — more live surfaces or fewer. That
///   is inherent to an outbox and is not a defect of this field.
/// - **Not that the two rails are the same event.** `broadcaster.emit` is not
///   `emit_transport_only` plus persistence: it also derives a
///   `v3_planning_progress_for_event` and fans that out as a second
///   transport-only send for `planexec_`-prefixed runs
///   (`realtime_events.rs:4392`, `:4409`). Replay stays faithful — a
///   `TransportOnly` record replayed through `emit_transport_only` loses nothing
///   it ever had — but "the branches differ only in persistence" is a false
///   premise to reason from, and it is the one a projector author reaches for.
/// - **Not the rule as it stands today.** A record replays under the rule that
///   was in force when it was produced. A later fix to the rule does not reach
///   records already written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
// Internally tagged like every other recorded vocabulary in this file, and the
// tag is `decision` rather than `routing` because the FIELD is already called
// `routing`: a same-named tag reads back as `routing.routing`, in an operator's
// eye and in every `jq` expression ever written against this log.
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RecordedEventRouting {
    /// **The record does not say.** Three ways to get here, and they are not
    /// interchangeable with each other either:
    ///
    /// 1. The record **predates this field**. `#[serde(default)]` on the field
    ///    means every journal written before it reads back as this rather than
    ///    failing to parse.
    /// 2. A producer wrote it explicitly because the case is not about routing
    ///    at all — a size-probe fixture, say.
    /// 3. The producer **could not express** the decision in this record: a
    ///    scope naming a different execution than the **run** whose journal the
    ///    record is going into, where the four recorded fields would come back
    ///    attached to a run they do not describe. **No production path produces
    ///    that pair today** — corrected 2026-08-27, after an earlier draft here
    ///    called it "the live case" and pointed at sub-goals, which run under
    ///    the parent's execution id on purpose. It is an invariant check on an
    ///    unenforced convention; see `phases::outbox::routing_for`'s *What the
    ///    arm actually guards* for the sites that maintain it and the one writer
    ///    that could move them apart.
    ///
    ///    "The run whose journal" is deliberate and was `"the journal"` until
    ///    2026-08-28. The producer compares the scope's `execution_id` against
    ///    the **producing context's** `execution_id`, which is the right pair —
    ///    the journal FILE is keyed by a loop-state address, and one execution
    ///    legitimately owns one journal per resume generation, nesting frame and
    ///    refinement pass. A guard that compared against the file key instead
    ///    would answer this variant for every resumed and every nested run, and
    ///    those runs' events genuinely are canonical runtime facts. See
    ///    [`RecordedCanonicalScope`]'s *THE FILE KEY IS NOT THE EXECUTION ID*
    ///    for the half of that split that is a real defect, and it is on the
    ///    reader.
    ///
    /// It is **not** a synonym for [`Self::TransportOnly`], and the difference is
    /// the whole reason it is a named variant rather than an absent field.
    /// `TransportOnly` is a decision a producer made; this is the absence of one.
    /// A reader that treats them alike re-creates, silently, the exact failure
    /// this enum exists to prevent — see the type docs. A live projector may
    /// apply an explicitly documented ambient fallback; a cold durable
    /// projector that cannot prove that fallback's canonical append must refuse
    /// and leave its cursor below the record.
    #[default]
    Unrecorded,
    /// **The event was not a canonical runtime fact of this run**: live
    /// surfaces only, nothing to persist. Three shapes reach it — no
    /// `canonical_event_scope` at all, a variant `map_v2_realtime_event` does
    /// not cover, or a mapped event naming a *different* execution than the
    /// producer's scope.
    ///
    /// **It does not say the producer sent anything**, and the earlier wording
    /// here ("the producer sent this event as a transport-only event") did.
    /// `emit_transport_only` exists only on the broadcaster branch. With no
    /// `event_broadcaster`, `emit_event` falls to the canonical-sink branch
    /// (`emit_event`'s three-way `if let`) — and **all three shapes above fail its
    /// three-way `if let` or its scope comparison**, so the event reaches no
    /// transport at all. A replaying process that holds a broadcaster then
    /// delivers an event the original producer never emitted — which is
    /// inherent to an outbox, and is why this is written as a classification
    /// rather than as a report of a send. See *What it does NOT promise* on the
    /// type.
    ///
    /// Not measured: how often a producer on this path has no broadcaster. The
    /// blast radius of that replay difference is therefore unsized, not small.
    TransportOnly,
    /// **The event was a canonical runtime fact of this run**, under this scope:
    /// a mapped event naming the producer's own execution.
    ///
    /// Like [`Self::TransportOnly`] this is the classification and not a report
    /// of a send. A producer holding a broadcaster emitted and persisted it; one
    /// holding only a `canonical_event_sink` persisted it and put it on no live
    /// surface; one holding neither did nothing with it at all. The record says
    /// what the event WAS, which is the half a replaying process can act on.
    ///
    /// The scope is carried rather than looked up because a replaying process
    /// may not be the producing one, and the registry the producer read is
    /// process-local. See [`RecordedCanonicalScope`] for why it holds four
    /// fields and not five.
    CanonicalRuntimeFact { scope: RecordedCanonicalScope },
}

/// What one journal record says.
///
/// Four bodies, not more. Effects live in their own ledger (see the module
/// docs), and every other candidate — budget ticks, cost accumulation, tool
/// lineage — is state the commit already carries. A body earns its place by
/// being something a **replay** or a **projector** must see, and nothing else
/// qualifies today.
///
/// # ADDING A VARIANT IS SAFE IN ONE DIRECTION ONLY
///
/// Checked rather than assumed, because the read direction is the one that
/// bites. This enum is `#[serde(tag = "kind")]` and **nothing on it, on
/// [`JournalRecord`], or on any body's fields is `deny_unknown_fields`** — the
/// only two `deny_unknown_fields` in this subtree are `store::ExecutionKeyWire`
/// and `store::EndedRun`, neither of which is a journal record. So:
///
/// - **Old records still load under a new build.** They carry
///   `kind: "phase_completed" | "event" | "owner_transition"`, every one of
///   which is still a variant, and an added variant adds no required field to
///   any of them. This is the direction [`Self::NamedEvent`] was added in and it
///   is the one that had to hold.
/// - **New records do NOT load under an old build**, and that is a real
///   rollback cost rather than a theoretical one. `serde` answers an unknown
///   tag with *"unknown variant `named_event`"*, [`Journal::parse`] treats a
///   parse failure anywhere but a torn tail as [`JournalError::CorruptRecord`],
///   and a corrupt record fails the **whole file** — so one downgraded process
///   reading a journal that contains one of these records quarantines the run
///   rather than skipping the record. A rollback across this change therefore
///   has to be a rollback of the runs too, not only of the binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalBody {
    /// A phase ran to one of its three conclusions.
    PhaseCompleted { step: RecordedStep },
    /// A cold holder found an Apply cursor with a valid pre-Resolve capsule and
    /// moved back to Resolve before running any phase.  This is a journaled
    /// transition rather than a snapshot edit so replay and the committed
    /// cursor remain one source of truth.
    RecoveryRewind { to: Phase },
    /// An event the projector should emit once, addressed by this record's
    /// [`EventKey`].
    ///
    /// The payload is `serde_json::Value` because the event vocabulary belongs
    /// to the emitter, not to the log — canonical execution events are already
    /// typed at their own boundary, and re-declaring them here would create a
    /// second definition to keep in step. The bound that matters is
    /// [`MAX_JOURNAL_RECORD_BYTES`], which applies whatever the shape.
    Event {
        event_type: String,
        payload: serde_json::Value,
        /// What `ActionExecutors::emit_event` decided for this event: a
        /// canonical runtime fact, or a transport-only send.
        ///
        /// **The payload cannot answer this and neither can the variant.**
        /// `emit_event` decides from `event_broadcaster`, `canonical_event_sink`
        /// and `canonical_event_scope`, and one `RuntimeTransportEvent` variant
        /// reaches both answers: `LLMResponseReceived` is a canonical fact
        /// through `emit_event` and an unconditional transport-only send from
        /// `analytics/operation_llm_telemetry.rs`. So a projector that guessed
        /// from the type name would be wrong for that event and silently right
        /// for most others, which is the worst way to be wrong.
        ///
        /// `#[serde(default)]`, so a record written before this field existed
        /// reads back as [`RecordedEventRouting::Unrecorded`] rather than
        /// failing to parse — and `Unrecorded` is an absence of a decision, not
        /// a decision. See that variant.
        #[serde(default)]
        routing: RecordedEventRouting,
    },
    /// A **named** event the projector should emit once, addressed by this
    /// record's [`EventKey`] exactly as [`Self::Event`] is.
    ///
    /// The five fields are `RuntimeTransportBroadcaster::emit_named`'s five
    /// arguments and nothing else. That is the whole vocabulary — there is no
    /// mirror type here and nothing to keep in step, for the same reason
    /// [`Self::Event`] carries a split of `RuntimeTransportEvent`'s own
    /// encoding rather than a re-declaration of it.
    ///
    /// # WHY THIS IS NOT [`Self::Event`], WHICH IS THE ONLY QUESTION THAT MATTERS
    ///
    /// It would fit. `emit_named` ends at
    /// `RuntimeTransportEvent::AgentEvent { event }` — `emit_named` →
    /// `emit_scoped_or_unscoped` → `emit_agent_transport_event` →
    /// `emit_transport_only` (`realtime_events.rs:4675`, `:4686`, `:4639`,
    /// `:4167`) — so a producer *could* build that variant at the call site and
    /// journal it through [`JournalAppend::event`], and an earlier statement of
    /// this problem said the rail "carries no variant tag" and therefore could
    /// not be held. **That is false: the tag is `AgentEvent`.**
    ///
    /// What the record has to hold is not the tag, it is the **delivery**.
    /// `emit_scoped_or_unscoped` does two things before it builds the envelope,
    /// and neither survives being handed a finished `AgentEvent`:
    ///
    /// - It stamps `timestamp_ms` into the payload when the payload is an
    ///   object and the caller did not set one (`realtime_events.rs:4694`).
    /// - It looks the payload's `task_id` / `execution_id` up in the
    ///   **chat fan-out** registry and, when a chat session is listening, emits
    ///   the primary envelope *and a re-stamped copy per target*, having first
    ///   stamped `chat_turn_id` onto the primary payload
    ///   (`realtime_events.rs:4726-4790`).
    ///
    /// A projector that replayed these through `emit_transport_only` would
    /// deliver **one** envelope where the producer delivered `1 + n`, and the
    /// chat activity card — the surface those four `plan.step.*` sites carry
    /// `task_id` specifically to reach
    /// (`executor.rs::emit_step_events_if_signaled`) — would
    /// lose the step lifecycle it exists to show. The fan-out is keyed by a
    /// registry that is process-local and time-varying, so it cannot be
    /// recorded either; the only honest record is the **call**, replayed
    /// through the same entry point.
    ///
    /// So this variant is not a second event vocabulary. It is the same
    /// vocabulary recorded one function earlier, at the last point where the
    /// fan-out has not yet happened.
    ///
    /// # It carries no [`RecordedEventRouting`], and that is not an oversight
    ///
    /// [`Self::Event`] needs one because `ActionExecutors::emit_event` has two
    /// branches and 54 of `RuntimeTransportEvent`'s variants can take either.
    /// This rail has **one**: every path out of `emit_scoped_or_unscoped` —
    /// the fast path and the fan-out path, primary envelope and every stamped
    /// copy — ends at `emit_transport_only`, and `map_v2_realtime_event` has no
    /// `AgentEvent` arm at all (`artifact_v2/events.rs:475-1957`, catch-all
    /// `_ => None`), so `emit_event` would take the transport-only branch for
    /// one of these too. A field whose only possible value is `TransportOnly`
    /// would be indistinguishable from the `Unrecorded` that means nobody
    /// decided, and an assertion about it could not fail.
    NamedEvent {
        /// The `event_type` the envelope is built with — `plan.step.started`,
        /// `tool.result.projected`. Called `name` here because `event_type` on
        /// this record would read as [`Self::Event`]'s field, which is a serde
        /// variant tag and not a taxonomy name.
        name: String,
        /// `"__system__"` when the producer had no agent, which is what every
        /// call site already substitutes.
        agent_id: String,
        /// `None` is a real value: `emit_scoped_or_unscoped` builds an
        /// **unscoped** envelope unless BOTH are `Some`, so a record that
        /// collapsed the pair would replay a scoped envelope where the producer
        /// sent an unscoped one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        payload: serde_json::Value,
    },
    /// Authority moved to another agent.
    ///
    /// Its own record rather than an incidental mutation inside a phase, because
    /// it moves the **ceiling**: a replay must reconstruct which agent held the
    /// run at each seq, and `LoopState::identity` is derived from these records.
    /// Applied at phase entry, never mid-phase, through
    /// [`apply_owner_transition`] — which is where a portable run's ceiling is
    /// narrowed rather than replaced.
    ///
    /// # It does NOT reconstruct *who authorised what*, and an earlier draft
    /// said it did
    ///
    /// See the `transition_authorization` field below. It has no writer, so no
    /// record in existence carries one. What replays from this body is who ended
    /// up holding the run, which is the half the ceiling depends on.
    OwnerTransition {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_agent_id: Option<String>,
        to_agent_id: String,
        /// **UNPOPULATED. No writer sets this, so every record ever written has
        /// `None`.** Read the two paragraphs below before using it for anything.
        ///
        /// `None` means **no authorization was recorded**, never "authorized" —
        /// a reader must fail closed on absence. Since absence is universal, an
        /// audit of *"under whose authority did this run change owner"* fails
        /// closed for every transition there is. The field is a **place** for
        /// that answer, not the answer; it is kept rather than deleted because
        /// removing it would take the shape of the record with it, and a later
        /// writer would have to migrate the log to put it back.
        ///
        /// # Why it has no writer, rather than nobody having got to it
        ///
        /// The only producer of these records is
        /// `InProcessWorkerHost::owner_transition_record`, which derives the
        /// transition by comparing the owner **before and after** a phase — the
        /// handover happens inside `phases::apply`'s eight-way match and nothing
        /// the phase returns says it happened. From outside the phase there is
        /// no view of the authorization the handover path checked *inside* it.
        /// The nearest available value, the context's current invocation
        /// binding, describes the frame the run is in *now*; writing it here
        /// would assert that THIS transition was authorized on the strength of a
        /// value describing a different one, which is worse than absence because
        /// absence is required to fail closed and a wrong value is not.
        ///
        /// # What a writer would need
        ///
        /// The handover path returning its authorization decision as part of
        /// what the phase reports, so the boundary records the check that
        /// actually ran. Note also that `AgenticPauseState::transition_authorization`
        /// — the field this one was named after — is a
        /// `TransitionAuthorizationBinding { source_agent_id, target_agent_id,
        /// surface }`, not a string, so a writer also has to decide this field's
        /// encoding. Neither is done here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transition_authorization: Option<String>,
    },
}

/// Apply a journaled [`JournalBody::OwnerTransition`] to the state it moves.
///
/// The record names the agent; the **ceiling** is the incoming owner's own
/// declaration (`AgentDefinition::browser_transports`) and is passed in, because
/// that is where the runtime reads it today — `set_executor_browser_transports`
/// is handed the same list at the same moment. Putting it on the record instead
/// would make the journal a second source of truth for an agent's declared
/// ceiling, and the two would drift the first time a definition was edited
/// between the transition and a replay.
///
/// # A portable run's ceiling is INTERSECTED, never replaced
///
/// The ordinary rule is *replaced, never merged*: a delegate must not inherit
/// its parent's wider ceiling. That rule alone is what makes this case reachable
/// — a run can start [`Placement::Portable`] under a `[cdp]` ceiling and hand
/// over to an agent whose ceiling is unrestricted, and now a portable execution
/// is permitted to launch a local Chrome. The placement is unchanged and
/// correct; the ceiling is unchanged and correct; the combination is wrong, and
/// no start-time check would ever see it.
///
/// So for a portable run the incoming ceiling is intersected with `[cdp]`.
/// Intersection is the fail-closed direction and matches the rule `grants.rs`
/// already states — *a ceiling narrows*. Deriving the placement from the ceiling
/// instead would be wrong here: it would re-class the run mid-flight, which is
/// the one thing [`Placement`] exists to prevent.
///
/// # The one case that refuses instead of narrowing
///
/// When the incoming owner's ceiling does not contain `cdp` at all, the
/// intersection is empty — and an empty ceiling is not "permits nothing", it is
/// `BrowserTransportCeiling::parse(&[])`, which permits **everything**. Writing
/// the intersection literally would turn the narrowest possible outcome into the
/// widest. There is no value that expresses "permits nothing", so the transition
/// is refused and named. This is the only refusal here; every other handover a
/// portable run can take is narrowed rather than failed.
pub fn apply_owner_transition(
    state: &mut LoopState,
    to_agent_id: &str,
    incoming_browser_transports: &[String],
) -> Result<(), LoopStateRefusal> {
    use crate::magician_v2::execution::primitive_dispatch::browser::session::{
        BrowserTransport, BrowserTransportCeiling,
    };

    // Parsed before anything is written, so an unrecognised transport name
    // refuses the handover rather than installing a ceiling nobody can read.
    // `parse` already refuses a typo rather than dropping it, and inheriting that
    // refusal here is what keeps a misspelled `cpd` from reaching the dispatcher.
    let incoming =
        BrowserTransportCeiling::parse(incoming_browser_transports).map_err(|error| {
            LoopStateRefusal::UnreadableCeiling {
                detail: error.to_string(),
            }
        })?;

    let effective = match &state.placement {
        Placement::Portable => {
            if !incoming.permits(BrowserTransport::Cdp) {
                return Err(LoopStateRefusal::HandoverLeavesPortableRunNoTransport {
                    to_agent_id: to_agent_id.to_string(),
                    incoming: incoming_browser_transports.to_vec(),
                });
            }
            vec![BrowserTransport::Cdp.label().to_string()]
        },
        // Replaced, never merged. A pinned run holds a worker, so a
        // process-local transport is exactly what it is entitled to.
        Placement::Pinned { .. } => incoming_browser_transports.to_vec(),
    };

    // The ceiling about to be installed is checked against Decision 2's
    // predicate itself, not against a value this function believes satisfies it.
    // Both halves of the invariant then read the same function, so the commit
    // side cannot tighten while this side keeps writing what used to pass — and
    // a portable branch that ever stopped producing a cdp-only list would fail
    // here rather than at some later commit that could not explain itself.
    super::state::portable_ceiling_is_cdp_only(&state.placement, &effective)?;

    state.identity.agent_id = Some(to_agent_id.to_string());
    state.identity.browser_transports = effective;
    Ok(())
}

/// The phase whose serde name encodes to the most bytes.
///
/// Used by [`JournalAppend::event`]'s size probe, which must measure the widest
/// record the store could write for a body — and `phase` is one of the fields
/// `driver_worker::commit_boundary` re-stamps, so the caller's value is not the
/// one that reaches the log.
///
/// `tests::the_size_probe_stamps_the_widest_phase_name` pins this against every
/// variant of [`Phase`], so a phase added with a longer name fails there rather
/// than by letting an over-sized body through a probe that measured a short one.
const WIDEST_PHASE: Phase = Phase::Epilogue;

/// A record on its way into the log, before the store has placed it.
///
/// Carries everything the *caller* knows and nothing it does not. `seq` and the
/// timestamp are the store's to assign, and the reason is not tidiness: a
/// caller-assigned seq is a second source of truth about the log's length, and
/// after a crash the two disagree. The store holds the file, so the store
/// numbers the lines.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalAppend {
    pub iteration: usize,
    pub phase: Phase,
    pub ordinal: u32,
    pub body: JournalBody,
}

/// The ordinal a phase-completion record always carries.
///
/// **Reserved, and deliberately not `0`.** A batch addresses its records by
/// `(iteration, phase, ordinal)`, and for an [`JournalBody::Event`] that address
/// is the projector's dedupe key — so an event's ordinal must depend on nothing
/// but its own position among the records the phase produced.
///
/// It used to depend on more than that. The completion record led the batch at
/// ordinal `0` and the host's records started at `1`, *except* when the phase
/// ended the run, where the completion trails and the host's records started at
/// `0` instead. `ends_run` is a property of how that ATTEMPT finished, not of
/// emission order, so the same logical event took ordinal `1` on the attempt
/// that continued and `0` on the attempt that paused. Re-running the phase then
/// produced two addresses for one event: the projector re-emits the one that
/// misses the dedupe window, and **silently drops** the one that collides with
/// an unrelated record — reporting `deduped: 1, emitted: 1`, which is exactly
/// what a correct run reports.
///
/// Reserving the top of the range fixes it at the root: the host's records own
/// `0..n` and always mean the same thing, and the completion record cannot
/// collide with them at any batch size this store admits.
pub const PHASE_COMPLETION_ORDINAL: u32 = u32::MAX;

impl JournalAppend {
    /// A phase-completion record, which is most of them.
    ///
    /// Stamped with [`PHASE_COMPLETION_ORDINAL`] rather than `0` so that the
    /// host's records can own a stable `0..n` regardless of where this record
    /// sits in the batch. See that constant for the defect this prevents.
    pub fn phase_completed(iteration: usize, phase: Phase, step: RecordedStep) -> Self {
        Self {
            iteration,
            phase,
            ordinal: PHASE_COMPLETION_ORDINAL,
            body: JournalBody::PhaseCompleted { step },
        }
    }

    /// Record the single recovery rewind admitted by the stateless loop.
    pub fn recovery_rewind(iteration: usize) -> Self {
        Self {
            iteration,
            phase: Phase::Apply,
            ordinal: PHASE_COMPLETION_ORDINAL,
            body: JournalBody::RecoveryRewind { to: Phase::Resolve },
        }
    }

    /// Re-observe a cold Resolve whose decision carry was deliberately not
    /// checkpointed (browser, credential-token, or Primitive work).
    pub fn recovery_reobserve(iteration: usize) -> Self {
        Self {
            iteration,
            phase: Phase::Resolve,
            ordinal: PHASE_COMPLETION_ORDINAL,
            body: JournalBody::RecoveryRewind { to: Phase::Observe },
        }
    }

    /// Re-observe a cold Apply whose Resolve output and pre-Resolve capsule are
    /// both unavailable. No effect is considered before this record commits.
    pub fn recovery_reobserve_from_apply(iteration: usize) -> Self {
        Self {
            iteration,
            phase: Phase::Apply,
            ordinal: PHASE_COMPLETION_ORDINAL,
            body: JournalBody::RecoveryRewind { to: Phase::Observe },
        }
    }

    /// Re-observe after a worker died between the reconstructed Observe and
    /// Decide boundaries. The old observation was process-local, so a new
    /// holder must not decide from that cursor.
    pub fn recovery_reobserve_from_decide(iteration: usize) -> Self {
        Self {
            iteration,
            phase: Phase::Decide,
            ordinal: PHASE_COMPLETION_ORDINAL,
            body: JournalBody::RecoveryRewind { to: Phase::Observe },
        }
    }

    /// An outbox entry: an event this phase wants emitted exactly once.
    ///
    /// # Why this returns a `Result` when [`Self::phase_completed`] does not
    ///
    /// A completion record has a bounded shape — an enum and two integers — so it
    /// cannot exceed [`MAX_JOURNAL_RECORD_BYTES`]. An event's payload is caller
    /// data of arbitrary size, and the size ceiling is enforced by
    /// [`JournalRecord::to_line`] at the **store**, on the whole batch. So the
    /// cost of an oversized event is not a lost event: the append fails, which
    /// fails the commit, which refuses the boundary and **stalls a live run** —
    /// a run held up by the size of something emitted for observability.
    ///
    /// Refusing here moves that decision to the producer, which is the only place
    /// that can answer it: drop the event, truncate the payload, or emit a
    /// reference to it. The run is not the thing that should pay.
    ///
    /// # The measurement is deliberately pessimistic — on EVERY field the caller
    /// does not control
    ///
    /// Five of a record's six fields are somebody else's to assign by the time it
    /// reaches the store, so the probe stamps the **widest** value each can ever
    /// take rather than the value in front of it:
    ///
    /// - `seq` and `at_ms` are the *store's*, and are not known yet: `u64::MAX`
    ///   and `i64::MIN`, twenty characters each.
    /// - `ordinal`, `iteration` and `phase` are the *driver's*.
    ///   `commit_boundary` discards whatever this constructor was handed and
    ///   re-stamps its own — see the section below — so measuring the caller's
    ///   values would measure a record the store never writes. The probe uses
    ///   `u32::MAX`, `usize::MAX` and [`WIDEST_PHASE`].
    ///
    /// Every real record the store writes for this body is therefore no larger
    /// than what was measured, which is the property that matters: a body that
    /// passes here cannot fail at the store.
    ///
    /// The narrow-probe failure is not hypothetical. A probe that copied the
    /// caller's `iteration` and `phase` leaves 22 bytes of headroom unmeasured:
    /// 19 for `iteration`, since `0` is one character and `usize::MAX` is twenty,
    /// and 3 for `phase`, since `"apply"` encodes to seven and `"epilogue"` to
    /// ten. A producer passing the placeholders the section below invites —
    /// `iteration: 0`, a short phase — therefore gets `Ok` for a body within
    /// those 22 bytes of the ceiling. `commit_boundary` then re-stamps its own
    /// `iteration: 250, Phase::Epilogue`, the encoded line crosses
    /// [`MAX_JOURNAL_RECORD_BYTES`], `to_line` fails inside `fs::append_journal`,
    /// and one over-long observability record fails the batch, fails the commit
    /// and refuses the boundary. That is a live run stalled by the size of
    /// something emitted for observability, which is verbatim the failure this
    /// constructor exists to prevent, arriving through the check meant to
    /// prevent it.
    ///
    /// # `ordinal` is not a parameter, and `iteration` and `phase` are not
    /// promises
    ///
    /// The driver re-stamps `iteration`, `phase` and `ordinal` on every record a
    /// phase reports (`driver_worker::commit_boundary`), so a value passed here
    /// would be overwritten and a producer would be reading a number that never
    /// reached the log. They are parameters only because the probe has to be
    /// *shaped* like a record; nothing downstream reads them back.
    ///
    /// ## What the producer owes, and what it CANNOT be given today
    ///
    /// The ordinal is a position in the batch `commit_boundary` assembles, and
    /// [`ProjectorCursor`] dedupes on it, so a producer owes a **deterministic
    /// emission order** — a re-run that emits its events in a different order
    /// re-addresses them and the dedupe silently misfires.
    ///
    /// ### The `ends_run` half of this is FIXED — corrected 2026-08-27
    ///
    /// An earlier version of this section said `commit_boundary` computes
    /// `ordinal: if ends_run { index } else { index + 1 }`, and that a phase
    /// ending the run therefore re-addressed its own events. **That is no longer
    /// what the driver does.** It stamps `ordinal: index` — the record's position
    /// among the *host's own* records — and the completion record takes
    /// [`PHASE_COMPLETION_ORDINAL`] at the top of the range instead of competing
    /// for a low one. So the address no longer moves when a phase that paused
    /// last time continues this time, which is exactly the fix that paragraph
    /// asked the driver for.
    ///
    /// The failure it described is worth keeping, because it is what the rule
    /// protects against: a re-addressed event is emitted twice, and with two
    /// events in one batch the second attempt's first event lands on the first
    /// attempt's *second* address and is deduped away while its sibling emits —
    /// a silent drop that [`Projection`] reports as `deduped: 1, emitted: 1`,
    /// indistinguishable from a correct run.
    ///
    /// ### The other half is DISCHARGED, and it was on the HOST
    ///
    /// `ordinal: index` makes the address stable against how the phase ended. It
    /// cannot make it stable against the host handing over a **different list**.
    /// `executor.rs::InProcessWorkerHost::run_phase` pushes an owner-transition
    /// record onto `report.records` — but only on the attempts where a handover
    /// actually happened — so a drain that extended the list *after* that push
    /// would put the transition at index 0 and shift every event by one, on
    /// exactly those attempts. Same failure, from the host's side rather than the
    /// driver's.
    ///
    /// The drain landed 2026-08-27 and it landed on the right side of that push:
    /// `report.records.extend(phases::outbox::take(execution_id))` runs
    /// **before** `owner_transition_record` is pushed. The obligation is met;
    /// the paragraph above is kept as the reason it has to stay met, because
    /// moving those two statements past each other is a one-line edit that
    /// re-addresses every event on a subset of attempts and breaks nothing
    /// visibly.
    ///
    /// # ITS PRODUCTION CALLER USES EXACTLY ONE DELIVERY PATH
    ///
    /// `phases::outbox::journal_and_emit` sends an accepted record through the
    /// stateless host's projector after commit. If this constructor refuses the
    /// record, or the selected arm has no drain, the producer emits inline
    /// instead. It never does both for the same event.
    /// # `routing` is REQUIRED, and that is the point of it being a parameter
    ///
    /// It would be cheaper to default it to
    /// [`RecordedEventRouting::Unrecorded`] and let producers opt in. That is
    /// the silent choice the field exists to prevent: a producer added later
    /// would write records that say nothing about how the event was emitted, a
    /// projector would fall back, and the fallback's warning would be the only
    /// trace. A parameter makes the next producer answer the question at the
    /// call site, where it holds the `ActionExecutors` that knows the answer.
    ///
    /// `Unrecorded` is still a value a caller may pass — a test that is about
    /// the size probe rather than about routing should pass it and say so —
    /// but it has to be typed out.
    pub fn event(
        iteration: usize,
        phase: Phase,
        event_type: impl Into<String>,
        payload: serde_json::Value,
        routing: RecordedEventRouting,
    ) -> Result<Self, JournalError> {
        Self::event_measured(iteration, phase, event_type, payload, routing)
            .map(|(append, _bytes)| append)
    }

    /// [`Self::event`], keeping the size its probe already measured.
    ///
    /// # It exists so the size probe is the ONLY encode on the producer's path
    ///
    /// [`measure_against_the_widest_record`] encodes the record to apply the
    /// ceiling, and every byte of that encode is paid whether or not anybody
    /// wants the number. `phases::outbox::build` does want one — it charges the
    /// record against `MAX_PENDING_BYTES_PER_RUN` — and used to get it by
    /// encoding the body a **second** time, once per journalled event, to
    /// recompute something the probe had just measured and dropped.
    ///
    /// **A second constructor rather than a changed return type**, because
    /// [`Self::event`]'s signature has call sites in `executor.rs` and
    /// `driver_worker.rs` that want the append and nothing else, and widening
    /// their return to a tuple would edit two files to say `.0`. This one is
    /// the whole implementation and [`Self::event`] is the projection of it, so
    /// there is still exactly one probe and one place to get it wrong.
    ///
    /// The `usize` is the **line** at the widest stamps, not the body alone —
    /// see [`measure_against_the_widest_record`] for why that distinction
    /// matters to a caller that thought it was getting `to_string(&body).len()`.
    pub fn event_measured(
        iteration: usize,
        phase: Phase,
        event_type: impl Into<String>,
        payload: serde_json::Value,
        routing: RecordedEventRouting,
    ) -> Result<(Self, usize), JournalError> {
        let body = JournalBody::Event {
            event_type: event_type.into(),
            payload,
            routing,
        };
        let (body, line_bytes) = measure_against_the_widest_record(body)?;
        Ok((
            Self {
                iteration,
                phase,
                ordinal: 0,
                body,
            },
            line_bytes,
        ))
    }

    /// An outbox entry for the **named** rail: one
    /// `RuntimeTransportBroadcaster::emit_named` call, recorded as the call.
    ///
    /// Everything [`Self::event`]'s docs say about the `Result`, the size probe,
    /// the re-stamped address and the producer's obligation to a deterministic
    /// emission order applies here **unchanged**, because both go through
    /// [`measure_against_the_widest_record`] and both are addressed by the same
    /// `(iteration, phase, ordinal)` the driver stamps. The two paragraphs worth
    /// repeating are the two that are different:
    ///
    /// - **There is no `routing` parameter**, because the rail has one delivery
    ///   branch. See [`JournalBody::NamedEvent`] for why a field here could
    ///   only ever hold one value.
    /// - **The payload is taken by value and the caller keeps its own copy.**
    ///   The producer (`phases::outbox::journal_and_emit_named`) keeps the
    ///   original available for inline fallback if admission fails, so one clone
    ///   per named event is unavoidable while that fallback exists. It is paid
    ///   *after* the drain gate, so a run on an arm with no drain pays nothing —
    ///   see that module's *What it costs per event*.
    pub fn named_event(
        iteration: usize,
        phase: Phase,
        name: impl Into<String>,
        agent_id: impl Into<String>,
        principal: Option<String>,
        workspace: Option<String>,
        payload: serde_json::Value,
    ) -> Result<Self, JournalError> {
        Self::named_event_measured(
            iteration, phase, name, agent_id, principal, workspace, payload,
        )
        .map(|(append, _bytes)| append)
    }

    /// [`Self::named_event`], keeping the size its probe already measured.
    ///
    /// Exists for the reason [`Self::event_measured`] does and carries the same
    /// two caveats: the number is the **line** at the widest stamps rather than
    /// the body alone, and the plain constructor stays for the callers that
    /// want only the append.
    pub fn named_event_measured(
        iteration: usize,
        phase: Phase,
        name: impl Into<String>,
        agent_id: impl Into<String>,
        principal: Option<String>,
        workspace: Option<String>,
        payload: serde_json::Value,
    ) -> Result<(Self, usize), JournalError> {
        let body = JournalBody::NamedEvent {
            name: name.into(),
            agent_id: agent_id.into(),
            principal,
            workspace,
            payload,
        };
        let (body, line_bytes) = measure_against_the_widest_record(body)?;
        Ok((
            Self {
                iteration,
                phase,
                ordinal: 0,
                body,
            },
            line_bytes,
        ))
    }
}

/// Run a body through the store's own encode and the store's own ceiling,
/// stamped with the **widest** value every field the caller does not control can
/// take.
///
/// # One function, because two copies of this is how the probe gets narrowed
///
/// Extracted when [`JournalAppend::named_event`] arrived. The failure it is
/// preventing is not hypothetical and is written up in full on
/// [`JournalAppend::event`]: a probe that measured the *caller's* `iteration`
/// and `phase` leaves 22 bytes unmeasured, `driver_worker::commit_boundary`
/// re-stamps its own, the encoded line crosses [`MAX_JOURNAL_RECORD_BYTES`] at
/// the store, and one over-long observability record fails the batch, fails the
/// commit and stalls a live run. A second constructor with its own probe is one
/// edit away from measuring a narrower record than the first; there is one
/// probe so there is one thing to get right.
///
/// Returns the body it was handed, **moved through** rather than cloned: the
/// probe needs to own a `JournalRecord` to encode it, and the caller needs the
/// body back to put in the append. Nothing here allocates beyond the one
/// `to_line` encode the ceiling check requires.
///
/// # And it returns the SIZE that encode measured, which is why one encode is
/// # enough
///
/// `to_line` measures the encoded line to apply the ceiling, and used to drop
/// the number on the floor. A caller that then wanted a byte charge for the
/// record — `phases::outbox::build` and `build_named`, which charge one against
/// `MAX_PENDING_BYTES_PER_RUN` — had to encode a second time to get it back.
/// Handing the measurement out removes a whole JSON encode from the per-event
/// producer path without adding a line of work to this one.
///
/// **It is the LINE at the widest stamps, not the body alone.** The probe wraps
/// the body in `seq`, `iteration`, `phase`, `ordinal` and `at_ms`, so this
/// number exceeds `serde_json::to_string(&body).len()` by that envelope — a
/// constant of roughly 130 bytes at the sentinel values below. For a buffer
/// charge that is the right direction and arguably the better number, since the
/// line is what the store writes; for anything that needs the body's own length
/// it is the wrong number and must not be read from here.
fn measure_against_the_widest_record(
    body: JournalBody,
) -> Result<(JournalBody, usize), JournalError> {
    let probe = JournalRecord {
        seq: u64::MAX,
        // NOT the caller's `iteration` and `phase`. The driver re-stamps both
        // and the caller's are frequently placeholders, so measuring them
        // measures a record that is never written. See
        // [`JournalAppend::event`]'s doc.
        iteration: usize::MAX,
        phase: WIDEST_PHASE,
        ordinal: u32::MAX,
        at_ms: i64::MIN,
        body,
    };
    // `to_line` runs the same encode and the same ceiling the store's writer
    // runs, rather than an estimate of them — the two cannot drift apart
    // because there is only one of them.
    //
    // The match is EXHAUSTIVE, and that is the point of writing it out. Every
    // variant that carries a `seq` is re-stamped to the "no seq assigned yet"
    // sentinel, because the probe's `u64::MAX` in an operator-facing message
    // would send somebody looking for a record eighteen quintillion lines
    // into a file that has none. A catch-all arm would opt every future
    // seq-carrying variant out of that rule silently; this way the next
    // author has to answer the question.
    let encoded = probe.to_line().map_err(|error| match error {
        JournalError::RecordTooLarge { bytes, .. } => {
            JournalError::RecordTooLarge { seq: 0, bytes }
        },
        JournalError::Encode { reason, .. } => JournalError::Encode { seq: 0, reason },
        JournalError::RetryDelayTooLarge { after_ms, .. } => {
            JournalError::RetryDelayTooLarge { seq: 0, after_ms }
        },
        JournalError::RecordAfterTerminal { terminal, .. } => {
            JournalError::RecordAfterTerminal { seq: 0, terminal }
        },
        JournalError::ImpossibleTransition { phase, reason, .. } => {
            JournalError::ImpossibleTransition {
                seq: 0,
                phase,
                reason,
            }
        },
        // No seq of their own: nothing to re-stamp, and their operands are
        // already about a file rather than about a record this call made.
        error @ (JournalError::CorruptRecord { .. }
        | JournalError::SeqGap { .. }
        | JournalError::SeqRewind { .. }
        | JournalError::TooManyRecords { .. }
        | JournalError::FileTooLarge { .. }
        | JournalError::DuplicateEventKey { .. }) => error,
    })?;
    // The probe's own encode, measured once. `encoded` is an owned `String`, so
    // taking its length here does not keep `probe` borrowed and the body can
    // still be moved out below.
    let line_bytes = encoded.len();
    Ok((probe.body, line_bytes))
}

/// A placed record: what the caller said, plus where the store put it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    /// One-based and gapless. Zero is reserved for "no records", which is what
    /// an empty watermark means.
    pub seq: u64,
    pub iteration: usize,
    pub phase: Phase,
    pub ordinal: u32,
    /// When the store placed the record. Wall clock, for operators reading the
    /// log — nothing in replay depends on it, because a log whose correctness
    /// depended on clock ordering would break the first time two workers
    /// disagreed about the time.
    pub at_ms: i64,
    pub body: JournalBody,
}

/// Remove the one producer-stamped observation field for this rail. Transport
/// events stamp `timestamp`; `phases::outbox::stamp_emit_time` inserts
/// `timestamp_ms` for named events. The caller selects exactly one. Correlation
/// ids, the other rail's similarly named field, nested objects, and domain time
/// fields remain semantic: a duplicate is safer than silently collapsing two
/// application facts.
fn logical_event_payload(
    payload: &serde_json::Value,
    producer_timestamp_field: &str,
) -> serde_json::Value {
    let mut logical = crate::magician_v2::json_traversal::clone_json_iteratively(payload);
    if let serde_json::Value::Object(fields) = &mut logical {
        fields.remove(producer_timestamp_field);
    }
    logical
}

impl JournalRecord {
    /// The address a projector dedupes on.
    ///
    /// Takes the journal's key from the caller rather than storing it on every
    /// record: it is constant for the whole file, and writing it per line would
    /// pay for it once per record to learn nothing.
    ///
    /// The parameter is named for the field it fills and **not** for what a
    /// production caller passes, which is a loop-state address. See
    /// [`EventKey::execution_id`].
    pub fn event_key(&self, execution_id: &str) -> EventKey {
        EventKey {
            execution_id: execution_id.to_string(),
            iteration: self.iteration,
            phase: self.phase,
            ordinal: self.ordinal,
        }
    }

    /// A logical-content projector key for an event, with an occurrence number
    /// that distinguishes two equivalent facts in one append batch.
    ///
    /// The historical key above is positional. That made a retry whose admitted
    /// membership changed silently confuse the new event at position N with a
    /// different event from the previous attempt at position N. The digest
    /// makes different logical bodies different identities while excluding the
    /// one producer-stamped emit-time field for that rail;
    /// `occurrence` preserves two intentional equivalent emissions from a batch.
    ///
    /// The digest is folded into the persisted `execution_id` string instead of
    /// changing `EventKey`'s wire shape. Existing projector cursors therefore
    /// remain readable, and [`ProjectorCursor::project`] migrates each legacy
    /// positional entry to the first body it historically represented.
    fn stable_event_key(&self, execution_id: &str, fingerprint: &str, occurrence: u32) -> EventKey {
        EventKey {
            execution_id: format!("{execution_id}#{fingerprint}"),
            iteration: self.iteration,
            phase: self.phase,
            ordinal: occurrence,
        }
    }

    fn event_fingerprint(&self) -> Option<String> {
        let stable_body = match &self.body {
            JournalBody::Event {
                event_type,
                payload,
                routing,
            } => serde_json::json!({
                "rail": "event",
                "event_type": event_type,
                "payload": logical_event_payload(payload, "timestamp"),
                "routing": routing,
            }),
            JournalBody::NamedEvent {
                name,
                agent_id,
                principal,
                workspace,
                payload,
            } => serde_json::json!({
                "rail": "named",
                "name": name,
                "agent_id": agent_id,
                "principal": principal,
                "workspace": workspace,
                "payload": logical_event_payload(payload, "timestamp_ms"),
            }),
            JournalBody::PhaseCompleted { .. }
            | JournalBody::RecoveryRewind { .. }
            | JournalBody::OwnerTransition { .. } => return None,
        };
        Some({
            let encoded = serde_json::to_vec(&stable_body)
                .expect("a normalized journal event body must encode back to JSON");
            blake3::hash(&encoded).to_hex().to_string()
        })
    }

    /// Serialize to one log line, refusing a record no bounded read could take
    /// back.
    ///
    /// The check is on the encoded bytes, not on an estimate, because the only
    /// number that matters is the one the reader will measure.
    pub fn to_line(&self) -> Result<String, JournalError> {
        // The same bounds the reader applies, so a record can never be written
        // that a bounded read would refuse. `From<BoundaryOutcome>` clamps, so
        // the supported path cannot reach this refusal; a hand-built record can.
        check_record_bounds(self)?;
        let encoded = serde_json::to_string(self).map_err(|error| JournalError::Encode {
            seq: self.seq,
            reason: error.to_string(),
        })?;
        if encoded.len() > MAX_JOURNAL_RECORD_BYTES {
            return Err(JournalError::RecordTooLarge {
                seq: self.seq,
                bytes: encoded.len(),
            });
        }
        // A record containing a raw newline would split into two lines and the
        // second would parse as garbage — or worse, as a different record.
        // `serde_json` escapes newlines inside strings, so this cannot happen
        // through the supported path; asserting it anyway costs one scan and
        // closes the case where it arrives some other way.
        if encoded.contains('\n') {
            return Err(JournalError::Encode {
                seq: self.seq,
                reason: "a journal record encoded to more than one line".to_string(),
            });
        }
        Ok(encoded)
    }
}

/// Why a journal could not be read, written, or replayed.
///
/// Every variant names the seq or line it concerns, because the operator
/// recovery path for a corrupt journal is to look at the bytes, and a message
/// that does not say where to look is a message that costs an hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    Encode {
        seq: u64,
        reason: String,
    },
    RecordTooLarge {
        seq: u64,
        bytes: usize,
    },
    /// A line that is not the last one failed to parse. Never skipped: a hole in
    /// the middle of an append-only log is corruption, and continuing past it
    /// would replay a run against a history that is missing a step.
    CorruptRecord {
        line: usize,
        reason: String,
    },
    /// Seqs must be one-based and gapless. A gap means a record was lost, which
    /// is not something replay can paper over.
    SeqGap {
        expected: u64,
        found: u64,
    },
    /// A seq went backwards: the same number appears twice, or an older one
    /// follows a newer one.
    ///
    /// Its own variant rather than a `SeqGap` with the operands reversed,
    /// because it has a different cause and a different fix. A gap means bytes
    /// were lost. A rewind means **two holders appended to one journal** — a
    /// lease that was not held, or was held by two workers at once — and the
    /// operator needs to be told that, not sent looking for missing bytes.
    SeqRewind {
        expected: u64,
        found: u64,
    },
    TooManyRecords {
        limit: usize,
    },
    FileTooLarge {
        bytes: u64,
        limit: u64,
    },
    /// A record arrived after the run had already ended.
    RecordAfterTerminal {
        seq: u64,
        terminal: TerminalKind,
    },
    /// A phase claimed a transition it cannot make — an exit from the epilogue,
    /// which is where exits land rather than a place they come from.
    ImpossibleTransition {
        seq: u64,
        phase: Phase,
        reason: &'static str,
    },
    /// Two records in one append batch claimed the same address.
    DuplicateEventKey {
        key: String,
    },
    /// A recorded retry delay is longer than any writer could have produced.
    ///
    /// Refused rather than clamped on this side. Clamping a value that cannot
    /// have come from the clamp on the write side would be repairing bytes by
    /// guessing, which is what the store's `Corrupt` posture already refuses to
    /// do elsewhere — and the delay is the one field whose wrong value is
    /// invisible at the point of use, because a run that never wakes looks like
    /// a run that is still working.
    RetryDelayTooLarge {
        seq: u64,
        after_ms: u64,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Encode { seq, reason } => {
                write!(f, "journal record {seq} could not be encoded: {reason}")
            },
            JournalError::RecordTooLarge { seq, bytes } => write!(
                f,
                "journal record {seq} is {bytes} bytes, over the {MAX_JOURNAL_RECORD_BYTES}-byte \
                 limit"
            ),
            JournalError::CorruptRecord { line, reason } => write!(
                f,
                "journal line {line} is corrupt and is not the torn tail: {reason}"
            ),
            JournalError::SeqGap { expected, found } => write!(
                f,
                "journal expected seq {expected} but found {found}; records between them were \
                 lost"
            ),
            JournalError::SeqRewind { expected, found } => write!(
                f,
                "journal expected seq {expected} but found {found} again; two holders appended to \
                 one journal, so a lease was not held"
            ),
            JournalError::TooManyRecords { limit } => {
                write!(f, "journal holds more than {limit} records")
            },
            JournalError::FileTooLarge { bytes, limit } => {
                write!(f, "journal is {bytes} bytes, over the {limit}-byte limit")
            },
            JournalError::RecordAfterTerminal { seq, terminal } => write!(
                f,
                "journal record {seq} follows a run that already ended as {terminal:?}"
            ),
            JournalError::ImpossibleTransition { seq, phase, reason } => write!(
                f,
                "journal record {seq} claims a transition {phase} cannot make: {reason}"
            ),
            JournalError::DuplicateEventKey { key } => {
                write!(f, "two records in one append claim the address {key}")
            },
            JournalError::RetryDelayTooLarge { seq, after_ms } => write!(
                f,
                "journal record {seq} asks for a retry in {after_ms}ms, over the \
                 {MAX_RETRY_AFTER_MS}ms limit; no writer produces that, and honouring it would \
                 park the run for longer than anyone is waiting"
            ),
        }
    }
}

impl std::error::Error for JournalError {}

/// Which kind of seq break this is.
///
/// One function so `parse` and `from_records` cannot classify the same damage
/// differently — the contract suite runs both paths and a divergence would show
/// up as one store rejecting what another accepted.
fn seq_break(expected: u64, found: u64) -> JournalError {
    if found < expected {
        JournalError::SeqRewind { expected, found }
    } else {
        JournalError::SeqGap { expected, found }
    }
}

/// The bounds a record's *payload* must satisfy, whichever door it came in.
///
/// One function for the same reason as [`seq_break`]: `parse` and `from_records`
/// both run in the contract suite, and a bound one door enforced and the other
/// did not would show up as one store accepting what another refused.
///
/// Only the retry delay is checked here. The byte and count ceilings belong to
/// the file and are enforced where the bytes are, and every other field is
/// either a bounded enum or a value replay does not act on.
fn check_record_bounds(record: &JournalRecord) -> Result<(), JournalError> {
    if let JournalBody::PhaseCompleted {
        step:
            RecordedStep::Exited {
                boundary: RecordedBoundary::Retry { after_ms },
            },
    } = &record.body
    {
        if *after_ms > MAX_RETRY_AFTER_MS {
            return Err(JournalError::RetryDelayTooLarge {
                seq: record.seq,
                after_ms: *after_ms,
            });
        }
    }
    Ok(())
}

/// A journal's records, in order, with the invariants a reader may rely on
/// already checked.
///
/// Constructing one is the only way to get records out of a file, so "did
/// anybody validate this" has one answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Journal {
    records: Vec<JournalRecord>,
}

impl Journal {
    /// Parse a whole journal file.
    ///
    /// The caller is responsible for not reading an unbounded file into the
    /// string in the first place — [`MAX_JOURNAL_BYTES`] is re-checked here, but
    /// a store that read the file without checking its size first has already
    /// paid the allocation this bound exists to prevent.
    ///
    /// # Torn tails, and only tails
    ///
    /// An append that is interrupted mid-write leaves a partial final line. That
    /// is expected and recoverable: the record was never acknowledged, so
    /// dropping it loses nothing. A partial line **anywhere else** is not
    /// recoverable — it means bytes were lost from the middle of an append-only
    /// file — and is an error rather than a skip.
    ///
    /// The tail is treated as possibly-torn only when the file does not end in a
    /// newline. A file ending in a newline has no incomplete line by
    /// construction, so a parse failure on its last line is corruption like any
    /// other. And the exemption belongs to the last line **of the file**, not to
    /// the last line that happened to hold something: a file ending in a blank
    /// line would otherwise hand the exemption to the complete,
    /// newline-terminated record above it and read real corruption as a torn
    /// write. `OutwardAssertionStore::act_attempts` applies the same rule to its
    /// own append-only file over its *non-blank* lines, which is the one place
    /// this is deliberately stricter rather than looser.
    pub fn parse(raw: &str) -> Result<Self, JournalError> {
        let byte_len = raw.len() as u64;
        if byte_len > MAX_JOURNAL_BYTES {
            return Err(JournalError::FileTooLarge {
                bytes: byte_len,
                limit: MAX_JOURNAL_BYTES,
            });
        }
        let tail_may_be_torn = !raw.is_empty() && !raw.ends_with('\n');

        let mut records: Vec<JournalRecord> = Vec::new();
        // Streamed rather than collected first. A file at [`MAX_JOURNAL_BYTES`]
        // made of very short lines holds tens of millions of them, and building
        // one vector entry per line before the record ceiling could fire would
        // allocate several times the size of the file being read — a bound paid
        // for after the allocation it exists to prevent is not a bound.
        //
        // Line numbers are reported against the raw file so an operator can find
        // the bytes. Blank lines are skipped but keep their place in the
        // numbering.
        let mut lines = raw.lines().enumerate().peekable();
        while let Some((index, raw_line)) = lines.next() {
            let line_number = index + 1;
            // Whether this is the last line OF THE FILE — not the last one that
            // held anything. A file whose final line is blank has a complete,
            // newline-terminated record above it, and that record was
            // acknowledged; letting the torn-tail exemption slide up to it would
            // swallow real corruption.
            let is_final_line = lines.peek().is_none();
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_JOURNAL_RECORD_BYTES {
                return Err(JournalError::CorruptRecord {
                    line: line_number,
                    reason: format!(
                        "the line is {} bytes, over the {MAX_JOURNAL_RECORD_BYTES}-byte record \
                         limit",
                        line.len()
                    ),
                });
            }
            match serde_json::from_str::<JournalRecord>(line) {
                Ok(record) => {
                    if records.len() >= MAX_JOURNAL_RECORDS {
                        return Err(JournalError::TooManyRecords {
                            limit: MAX_JOURNAL_RECORDS,
                        });
                    }
                    let expected = records.len() as u64 + 1;
                    if record.seq != expected {
                        return Err(seq_break(expected, record.seq));
                    }
                    // At parse, not at use: a delay nobody could have written is
                    // a fact about these bytes, and the operator recovering from
                    // it needs to be pointed at the line rather than at the run
                    // that later failed to wake.
                    check_record_bounds(&record)?;
                    records.push(record);
                },
                // The one tolerated failure: the final line of a file that does
                // not end in a newline. The length check above runs first, so a
                // final line too long to be a record is corruption rather than a
                // torn write — a torn line is a prefix of a complete one and is
                // therefore never longer than one.
                Err(_) if is_final_line && tail_may_be_torn => break,
                Err(error) => {
                    return Err(JournalError::CorruptRecord {
                        line: line_number,
                        reason: error.to_string(),
                    })
                },
            }
        }
        Ok(Self { records })
    }

    /// Build a journal from records already in hand, checking the same
    /// invariants a parse would.
    ///
    /// Used by stores that hold records in memory rather than in a file. Sharing
    /// the checks is the point: a contract suite that proved a filesystem store
    /// rejected a seq gap and let an in-memory one accept it would be testing
    /// the file format, not the store.
    pub fn from_records(records: Vec<JournalRecord>) -> Result<Self, JournalError> {
        if records.len() > MAX_JOURNAL_RECORDS {
            return Err(JournalError::TooManyRecords {
                limit: MAX_JOURNAL_RECORDS,
            });
        }
        for (index, record) in records.iter().enumerate() {
            let expected = index as u64 + 1;
            if record.seq != expected {
                return Err(seq_break(expected, record.seq));
            }
            check_record_bounds(record)?;
        }
        Ok(Self { records })
    }

    /// Parse a complete suffix whose first record has a known sequence.
    ///
    /// This is intentionally narrower than [`Self::parse`]: a suffix is valid
    /// only when a persistent journal index supplied its exact byte offset and the
    /// enclosing store proved that index still names the current file
    /// generation. Torn tails are not accepted here; an indexed generation was
    /// published only after a newline-terminated durable append.
    pub(crate) fn parse_suffix(
        raw: &str,
        first_seq: u64,
    ) -> Result<Vec<JournalRecord>, JournalError> {
        if raw.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(JournalError::FileTooLarge {
                bytes: raw.len() as u64,
                limit: MAX_JOURNAL_BYTES,
            });
        }
        if !raw.is_empty() && !raw.ends_with('\n') {
            return Err(JournalError::CorruptRecord {
                line: 1,
                reason: "an indexed journal suffix does not end in a newline".to_string(),
            });
        }
        let mut records = Vec::new();
        for (index, raw_line) in raw.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_JOURNAL_RECORD_BYTES {
                return Err(JournalError::CorruptRecord {
                    line: index + 1,
                    reason: format!(
                        "the line is {} bytes, over the {MAX_JOURNAL_RECORD_BYTES}-byte record limit",
                        line.len()
                    ),
                });
            }
            if records.len() >= MAX_JOURNAL_RECORDS {
                return Err(JournalError::TooManyRecords {
                    limit: MAX_JOURNAL_RECORDS,
                });
            }
            let record: JournalRecord =
                serde_json::from_str(line).map_err(|error| JournalError::CorruptRecord {
                    line: index + 1,
                    reason: error.to_string(),
                })?;
            let expected = first_seq.saturating_add(records.len() as u64);
            if record.seq != expected {
                return Err(seq_break(expected, record.seq));
            }
            check_record_bounds(&record)?;
            records.push(record);
        }
        Ok(records)
    }

    /// Every record, orphans included. For sweeps and operator tooling.
    ///
    /// Named so that reaching for it instead of [`Self::authoritative`] is a
    /// visible choice rather than an accident.
    pub fn all_records(&self) -> &[JournalRecord] {
        &self.records
    }

    /// Consume a checked journal without deep-cloning event payloads.
    pub(crate) fn into_records(self) -> Vec<JournalRecord> {
        self.records
    }

    /// The last seq in the file, or zero when it holds nothing.
    pub fn last_seq(&self) -> u64 {
        self.records.last().map_or(0, |record| record.seq)
    }

    /// The records a committed state vouches for.
    ///
    /// Everything past `watermark` is an orphaned attempt by a worker that
    /// appended and then failed to commit. Never replayed, never projected.
    pub fn authoritative(&self, watermark: u64) -> &[JournalRecord] {
        let take = self
            .records
            .iter()
            .position(|record| record.seq > watermark)
            .unwrap_or(self.records.len());
        &self.records[..take]
    }

    /// The records past the watermark, which a commit may sweep.
    pub fn orphaned(&self, watermark: u64) -> &[JournalRecord] {
        let skip = self
            .records
            .iter()
            .position(|record| record.seq > watermark)
            .unwrap_or(self.records.len());
        &self.records[skip..]
    }

    /// Records from `from_seq` onward, orphans excluded.
    pub fn authoritative_from(&self, from_seq: u64, watermark: u64) -> &[JournalRecord] {
        let authoritative = self.authoritative(watermark);
        let skip = authoritative
            .iter()
            .position(|record| record.seq >= from_seq)
            .unwrap_or(authoritative.len());
        &authoritative[skip..]
    }

    /// Reject two records in one batch claiming one address.
    ///
    /// A *later* re-run legitimately produces a duplicate address — that is what
    /// the projector's dedupe is for — but two inside a single append are the
    /// caller having assigned the same ordinal twice, which silently drops one
    /// event at projection time.
    pub fn check_batch_addresses(
        execution_id: &str,
        appends: &[JournalAppend],
    ) -> Result<(), JournalError> {
        let mut seen: HashSet<(usize, Phase, u32)> = HashSet::with_capacity(appends.len());
        for append in appends {
            if !seen.insert((append.iteration, append.phase, append.ordinal)) {
                return Err(JournalError::DuplicateEventKey {
                    key: EventKey {
                        execution_id: execution_id.to_string(),
                        iteration: append.iteration,
                        phase: append.phase,
                        ordinal: append.ordinal,
                    }
                    .to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Where a run is, reconstructed from its records.
///
/// The cursor a worker would resume at, and the value the replay-determinism
/// test compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayedCursor {
    /// The seq this cursor reflects. Zero means "before any record".
    pub seq: u64,
    pub iteration: usize,
    pub phase: Phase,
    /// How the run ended, when the last record ended it.
    ///
    /// Cleared again the moment a further record appears **and** the ending was
    /// a resumable one — see [`replay_each`], where that distinction is the
    /// difference between a paused execution that can be picked up and a journal
    /// nothing can ever replay again.
    pub terminal: Option<TerminalKind>,
}

impl ReplayedCursor {
    /// Where an execution starts: iteration one, first phase, nothing recorded.
    pub const fn start() -> Self {
        Self {
            seq: 0,
            iteration: 1,
            phase: Phase::first(),
            terminal: None,
        }
    }
}

/// Fold a journal back into the cursor it recorded, at every seq.
///
/// # Why every seq and not the last one
///
/// The design's testing section asks that "replaying a journal reproduces the
/// recorded state at every seq" — every seq, not the end. A reader that jumped
/// to the last record could not answer what the cursor was at seq 4, which is
/// exactly the question a crash-injection test asks. [`replay`] is the last
/// element of this.
///
/// # What the fold knows that a reader might not
///
/// A boundary exit does **not** end a run and does **not** skip the epilogue: it
/// leaves the iteration body, the epilogue runs, and the next iteration starts.
/// So an `Exited` record moves the cursor to [`Phase::Epilogue`], and it is the
/// epilogue's own record that steps the iteration counter. A run that ends
/// instead — `RunEnded` — bypasses the epilogue entirely, which is the behaviour
/// the design insists must be preserved: *"`Epilogue` runs on
/// `NextIteration`/`Retry` outcomes only, never on `Terminal`/`Pause`/`Park`"*.
pub fn replay_each(records: &[JournalRecord]) -> Result<Vec<ReplayedCursor>, JournalError> {
    replay_each_from(ReplayedCursor::start(), records)
}

fn replay_each_from(
    mut cursor: ReplayedCursor,
    records: &[JournalRecord],
) -> Result<Vec<ReplayedCursor>, JournalError> {
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        if let Some(terminal) = cursor.terminal {
            // A resumable ending is not the end of the log. `WaitingForUser`,
            // `WaitingForChildren`, `PausedByUser`, `WaitingForConfirmation` and
            // `Sleeping` all end the *invocation* and leave the execution alive
            // for something outside to answer — and when it does, the run is
            // picked up and appends again.
            //
            // Refusing that was a real defect in this function's first cut: the
            // first pause an execution took made its journal permanently
            // unreplayable, and the failure would have surfaced as a resumed run
            // that could not load rather than anywhere near the pause. Only a
            // terminal that genuinely ends the run forbids what follows it.
            if !terminal.is_resumable() {
                return Err(JournalError::RecordAfterTerminal {
                    seq: record.seq,
                    terminal,
                });
            }
            cursor.terminal = None;
        }
        cursor.seq = record.seq;
        match &record.body {
            // NONE of the three moves the cursor, and each is named rather than
            // grouped under an `_` so the next body added has to answer the
            // question instead of inheriting an answer.
            //
            // - `Event` and `NamedEvent` are outbox entries. They record what a
            //   phase wants EMITTED; the cursor records where the loop GOT TO,
            //   and a phase that emits four events and one that emits none
            //   resume in the same place. `NamedEvent` is stated here on its own
            //   rather than folded into the `Event` arm because inheriting
            //   cursor-neutrality silently is exactly how the next stale comment
            //   gets written: it is neutral for the same reason, and the reason
            //   is what has to be re-checked, not the grouping.
            // - `OwnerTransition` is applied at phase entry and changes who runs
            //   the phase, not which phase runs.
            JournalBody::Event { .. }
            | JournalBody::NamedEvent { .. }
            | JournalBody::OwnerTransition { .. } => {},
            JournalBody::RecoveryRewind { to } => {
                let supported = matches!(
                    (record.phase, *to),
                    (Phase::Apply, Phase::Resolve)
                        | (Phase::Apply, Phase::Observe)
                        | (Phase::Resolve, Phase::Observe)
                        | (Phase::Decide, Phase::Observe)
                );
                if !supported
                    || cursor.iteration != record.iteration
                    || cursor.phase != record.phase
                {
                    return Err(JournalError::ImpossibleTransition {
                        seq: record.seq,
                        phase: record.phase,
                        reason: "a recovery rewind may only move the current Apply cursor to \
                                 Resolve/Observe or the current Decide/Resolve cursor to Observe",
                    });
                }
                cursor.iteration = record.iteration;
                cursor.phase = *to;
            },
            JournalBody::PhaseCompleted { step } => match step {
                RecordedStep::Continued => match record.phase.next_in_iteration() {
                    Some(next) => {
                        cursor.iteration = record.iteration;
                        cursor.phase = next;
                    },
                    // The epilogue has no next phase within its iteration, and
                    // that is deliberate: the step from one iteration to the
                    // next increments the counter the iteration ceiling is
                    // checked against. This is the one place that step is taken.
                    None => {
                        cursor.iteration = record.iteration.saturating_add(1);
                        cursor.phase = Phase::first();
                    },
                },
                RecordedStep::Exited { .. } => {
                    if record.phase == Phase::Epilogue {
                        return Err(JournalError::ImpossibleTransition {
                            seq: record.seq,
                            phase: record.phase,
                            reason: "the epilogue holds no exits; it is where exits land",
                        });
                    }
                    cursor.iteration = record.iteration;
                    cursor.phase = Phase::Epilogue;
                },
                RecordedStep::RunEnded { terminal } => {
                    cursor.iteration = record.iteration;
                    // The cursor names where the run ended rather than where it
                    // would have gone next. A resumable terminal — a pause, a
                    // wait — is picked up by re-running this phase, so this is
                    // the position a resume needs.
                    cursor.phase = record.phase;
                    cursor.terminal = Some(*terminal);
                },
            },
        }
        out.push(cursor);
    }
    Ok(out)
}

/// The cursor after the last record, or the start when there are none.
pub fn replay(records: &[JournalRecord]) -> Result<ReplayedCursor, JournalError> {
    Ok(replay_each(records)?
        .last()
        .copied()
        .unwrap_or_else(ReplayedCursor::start))
}

/// Continue replay from a previously verified committed prefix.
///
/// Durable stores use this for a bounded tail index: the persisted cursor is
/// cryptographically chained to the exact journal file generation, and only
/// records appended after that prefix need folding. Callers must not supply an
/// arbitrary cursor; the ordinary whole-log entry point remains [`replay`].
pub(crate) fn replay_from(
    start: ReplayedCursor,
    records: &[JournalRecord],
) -> Result<ReplayedCursor, JournalError> {
    Ok(replay_each_from(start, records)?
        .last()
        .copied()
        .unwrap_or(start))
}

/// How far a projector has emitted, and what it must not emit again.
///
/// # The two halves are not the same guarantee
///
/// `emitted_through_seq` is a **high-water mark**: everything at or below it has
/// been emitted, so a scan need not start from the beginning again. It holds only
/// while records are marked in seq order — marking a later record before an
/// earlier one leaves the earlier one beneath the mark, and it is then never
/// emitted at all — so the mark is a correctness property rather than a pure
/// optimisation. It used to be a correctness property of the *caller's* loop; see
/// the ordering section below for why it is this module's now.
///
/// `recent_keys` is the **correctness** half. A crashed phase re-runs and
/// appends its event again at a new seq *above* the mark, so the mark alone
/// cannot recognise it. The content identity can, which is why dedupe is by
/// `event_key` rather than by seq.
///
/// The window is bounded, and the bound is a real limit rather than a detail: a
/// duplicate whose original has fallen out of the window is emitted twice. It is
/// sized against the re-run distance a crash can produce — a handful of records
/// within one iteration — not against the length of a run.
///
/// The identity includes a digest of the complete journal body and an
/// identical-body occurrence within its append batch. Retry admission may add
/// or remove an earlier member without making the body that shifts into its
/// ordinal collide. Exact repeats remain dedupe candidates; distinct facts are
/// never discarded merely because their positional ordinal matches.
///
/// # The ordering hazard is now ENFORCED, not stated
///
/// An earlier version of this type published `mark_emitted` and asked the caller
/// to mark in the order `pending` returned, noting that marking a later record
/// first buries the earlier one and its event is never emitted. That is a
/// correctness property of somebody else's loop, defended by a doc comment —
/// which is to say, undefended. `pending` itself is `#[cfg(test)]` for the same
/// reason: published, it was the read side an external caller would reach for,
/// and there is no public method that advances the mark past what it returns.
///
/// `mark_emitted` is private now and [`Self::project`] is the only way to move
/// the mark. It walks records in seq order and advances one record at a time, so
/// the mark cannot pass a record that still needs emitting, and a **refused emit
/// stops the walk** rather than skipping to the next record. The bad order is not
/// discouraged; it is unreachable.
///
/// # It is serialized, and the store is where it lands
///
/// The mark is durable: the type round-trips, a reload clamps the window rather
/// than trusting the file, and
/// [`LoopStateStore::save_projector_cursor`](super::store::LoopStateStore::save_projector_cursor)
/// publishes it under its own key. So a worker that picks an execution up
/// restores both halves — the high-water mark and the dedupe window that
/// recognises a re-run — instead of starting at zero and re-emitting everything
/// authoritative it finds.
///
/// **It is deliberately NOT a field on [`LoopState`](super::state::LoopState),
/// and an earlier version of this paragraph said it should be.** Two reasons, and
/// the second is the one that decides it:
///
/// - **Size.** [`Self::DEFAULT_WINDOW`] is 1_024 [`EventKey`]s and every new key
///   carries the journal address plus a 64-character digest, so a full window
///   remains comfortably under the filesystem store's one-megabyte ceiling.
///   The worst case is the figure `store::fs`'s `MAX_PROJECTOR_BYTES` is sized
///   from, and that is the one to reuse: it is computed against a real
///   implementation rather than estimated. `LoopState` is committed at *every*
///   phase boundary and the filesystem store retains several revisions of each,
///   so riding it would multiply a one-kilobyte record by more than two orders of
///   magnitude, six times an iteration, for a value no commit decision reads.
/// - **The mark cannot be written by the commit it belongs to.** A projection
///   may only walk records at or below the *committed* watermark, so the order
///   is append → commit → emit → mark: the mark is known only after the commit
///   that made those records authoritative has already landed. Riding `LoopState`
///   would therefore mean a second compare-and-swap per boundary — one that can
///   lose the race and take the mark down with it, re-emitting a boundary's
///   events for a reason that has nothing to do with the outbox. Its own record
///   is last-writer-wins under the lease and survives a refused commit.
///
/// Do not describe any of this as exactly-once. Emission happens before the mark
/// is saved, so a crash between the two re-emits at most one boundary's events.
/// That is at-least-once with a bounded duplicate window, which is what an outbox
/// buys; the address is what makes the duplicate recognisable, not the mark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ProjectorCursorWire")]
pub struct ProjectorCursor {
    emitted_through_seq: u64,
    /// Exact committed terminal watermark whose cross-layer runtime
    /// settlement was durably accepted. This is deliberately independent of
    /// the event high-water mark: a reconciler retirement can append only
    /// `RunEnded { CannotProceed }` after all earlier events were projected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_settled_terminal_seq: Option<u64>,
    recent_keys: Vec<EventKey>,
    window: usize,
}

/// A persisted [`ProjectorCursor`] before its bounds have been re-checked.
///
/// Deserializing straight into the real type would let a file set `window` to
/// zero — evicting every address the moment it is remembered, which disables
/// dedupe and re-emits every re-run's duplicate. That is the exact failure
/// [`ProjectorCursor::with_window`] clamps on construction, and a constructor
/// bound that a deserializer walks around is not a bound. Same argument as
/// [`Journal::from_records`]: whichever door a value comes in, it meets the same
/// checks.
///
/// # `window` is not the only invariant this door has to re-establish
///
/// `recent_keys` is maintained by [`ProjectorCursor::mark_emitted`] under two
/// properties the constructor path never breaks and a file can:
///
/// - **Bounded length.** `mark_emitted` evicts down to `window` on every push. A
///   naive `Vec<EventKey>` field allocates whatever the file names *before* any
///   trim runs, so a cursor carrying a million addresses is a million
///   allocations to throw away — the trim would bound the per-record scan and
///   not the memory. [`recent_keys_bounded`] therefore caps during the walk.
/// - **No duplicates.** `ProjectorCursor::project` `continue`s on an address
///   already in the window and only reaches `mark_emitted` for an absent one —
///   so a live window of size N holds N *distinct* addresses. (The check used to
///   be repeated inside `mark_emitted` and was removed on 2026-08-28 as a second
///   scan of the same `Vec` for the same answer; a `debug_assert!` there keeps it
///   enforced under test.) A file is under no
///   such obligation, and a window of 1_024 copies of one address restores a
///   cursor with an effective dedupe distance of ONE — every re-run duplicate
///   beyond the immediately preceding record emitted twice, which is the failure
///   the type exists to prevent arriving through the door this block was added
///   to close.
#[derive(Deserialize)]
struct ProjectorCursorWire {
    #[serde(default)]
    emitted_through_seq: u64,
    #[serde(default)]
    runtime_settled_terminal_seq: Option<u64>,
    #[serde(default, deserialize_with = "recent_keys_bounded")]
    recent_keys: Vec<EventKey>,
    #[serde(default)]
    window: usize,
}

/// Read `recent_keys` without ever holding more than [`ProjectorCursor::MAX_WINDOW`]
/// of them.
///
/// A plain `Vec<EventKey>` field would let the file decide the allocation and
/// leave the trim in `From<ProjectorCursorWire>` to discard the excess after the
/// fact — a bound that is applied one step too late to be a bound. This keeps a
/// sliding window of the NEWEST addresses as it walks, for the same reason the
/// trim keeps the newest: a re-run's original lives at the recent end.
///
/// `VecDeque` rather than `Vec::remove(0)`, which is `O(n)` per eviction and
/// would turn a hand-edited million-key cursor into a quadratic walk — trading
/// the memory problem for a CPU one.
fn recent_keys_bounded<'de, D>(deserializer: D) -> Result<Vec<EventKey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use std::collections::VecDeque;

    struct BoundedKeys;

    impl<'de> serde::de::Visitor<'de> for BoundedKeys {
        type Value = Vec<EventKey>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a sequence of recently emitted event addresses")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut kept: VecDeque<EventKey> = VecDeque::new();
            while let Some(key) = seq.next_element::<EventKey>()? {
                if kept.len() == ProjectorCursor::MAX_WINDOW {
                    kept.pop_front();
                }
                kept.push_back(key);
            }
            Ok(kept.into())
        }
    }

    deserializer.deserialize_seq(BoundedKeys)
}

impl From<ProjectorCursorWire> for ProjectorCursor {
    fn from(wire: ProjectorCursorWire) -> Self {
        // Zero is indistinguishable from absence here — `#[serde(default)]`
        // produces the same value for a missing field and for a written `0` —
        // and both resolve to the default rather than to one. Nothing writes a
        // deliberate zero (`with_window` clamps), so the reachable causes are an
        // older cursor and an edited file, and neither is a request for no
        // dedupe. Resolving to one would satisfy the clamp while leaving a
        // window of size one, which is a silently broken dedupe rather than an
        // obviously default one.
        // `tests::a_persisted_window_of_zero_restores_the_default_and_not_a_window_of_one`
        // is what distinguishes the two, because an argument in a comment is not
        // a check. It needs THREE records at two addresses; the clamp test's two
        // records at one address pass under a window of one as well.
        let window = if wire.window == 0 {
            ProjectorCursor::DEFAULT_WINDOW
        } else {
            wire.window.min(ProjectorCursor::MAX_WINDOW)
        };
        // Re-establish `mark_emitted`'s no-duplicate invariant before the trim,
        // and in that order: trimming first would count duplicates against the
        // window and leave a cursor remembering fewer distinct addresses than it
        // claims. Kept by LAST occurrence, which is where `mark_emitted` would
        // have left the address had it seen the same sequence.
        let mut seen = HashSet::with_capacity(wire.recent_keys.len().min(window));
        let mut recent_keys: Vec<EventKey> = wire
            .recent_keys
            .into_iter()
            .rev()
            .filter(|key| seen.insert(key.clone()))
            .collect();
        recent_keys.reverse();
        // Keep the NEWEST, which is where a re-run's original lives. Truncating
        // from the end would keep the oldest addresses and drop exactly the ones
        // dedupe is about to be asked about.
        if recent_keys.len() > window {
            recent_keys.drain(..recent_keys.len() - window);
        }
        Self {
            emitted_through_seq: wire.emitted_through_seq,
            runtime_settled_terminal_seq: wire.runtime_settled_terminal_seq,
            recent_keys,
            window,
        }
    }
}

/// Why a sink could not take an event.
///
/// A projection **stops** at a refusal rather than skipping past it, so this is
/// the value that decides where the next projection resumes. It carries a reason
/// rather than being a unit, because the operator question after a stalled
/// outbox is always "stalled on what".
///
/// # It means "not now", and there is no way for it to mean "not ever"
///
/// [`ProjectorCursor::project`] treats every refusal as transient: the walk
/// stops, the mark is left *below* the refused record, and the next projection
/// retries the same record. There is no retry bound, no attempt counter, no
/// poison-pill skip, and no method a caller can reach for to advance past a
/// record its sink will never accept — [`ProjectorCursor`]'s only mark-moving
/// method is private and is `project` itself.
///
/// So a refusal that is a **deterministic function of the record** — an
/// `event_type` this build cannot map, a payload shape it cannot parse — is a
/// permanent head-of-line block on that execution's outbox: every later event,
/// including every one the build understands perfectly, is never emitted for the
/// life of the run, and [`Projection::stopped_at`] reports the same seq on every
/// call forever. `tests::a_permanently_refusing_sink_never_advances_past_the_record`
/// pins that, deliberately, so the cost is a measured property rather than a
/// surprise at the first call site.
///
/// **A sink must therefore refuse only what a later attempt could accept** — a
/// transport that is down, a channel that is full, a dependency not yet ready. A
/// sink that cannot map a record at all must take it (`Ok`) and drop it on its
/// own side, where it can be counted and logged, rather than refuse it and stall
/// everything behind it. That is a real loss — the mark moves past a record
/// nothing emitted — and it is the smaller one, because the alternative loses
/// every subsequent event of the run instead of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitRefused {
    pub reason: String,
}

impl fmt::Display for EmitRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Where a projected event goes.
///
/// # Deliberately synchronous
///
/// The one emitter this exists to feed — `ActionExecutors::emit_event` — is a
/// synchronous fire-and-forget onto a broadcaster. An `async` trait here would
/// buy nothing today and would make [`ProjectorCursor::project`] an `async fn`
/// holding a `&mut` cursor across an await, which is the shape that makes a
/// caller reach for a clone of the mark — and a cloned mark is how a mark gets
/// rewound.
///
/// # The payload is a `Value`, and the sink owns the vocabulary
///
/// [`JournalBody::Event`] carries `event_type` and an untyped payload because the
/// event vocabulary belongs to the emitter (see that variant's docs). A sink is
/// therefore the place where an unrecognised `event_type` is decided about.
///
/// **The answer there is `Ok` plus a counter on the sink's own side, NOT
/// [`EmitRefused`].** An earlier version of this paragraph said the opposite — an
/// event this build cannot map is one a later build might, so refuse it and
/// leave it in the journal — and that reads well and is wrong, because it is a
/// standing instruction to produce the one condition this projector cannot
/// recover from. An unrecognised `event_type` is deterministic and permanent: the
/// same record refused today is refused on every retry, [`ProjectorCursor::project`]
/// has no bound on those retries and no way to step over the record, and the
/// whole tail of the execution's outbox is buried behind it. See
/// [`EmitRefused`]'s own docs for the mechanism.
///
/// Taking-and-dropping is the cheaper mistake, not a free one, and the
/// difference is worth being exact about. Refusing keeps the record *addressable
/// by this cursor*: the mark stays below it and a build that learns the event
/// type emits it on the next pass. Taking it advances the mark past it, so once
/// cursors are persisted, nothing re-offers it — the record survives in the
/// journal for an operator or a deliberately rewound mark, and not for the
/// projector. What that buys is every event above it: the ones this build
/// understands keep reaching the transport instead of being buried behind one it
/// does not.
/// # `Send`, because every caller of [`ProjectorCursor::project`] is a worker
///
/// The projection runs inside `driver_worker::commit_boundary`, which is an
/// `async fn` whose future is spawned. A `&mut dyn ProjectedEventSink` that were
/// not `Send` would make that future not `Send` the moment a compiler less
/// generous about borrow liveness decided the sink was live across the store
/// call that saves the mark — and the resulting error names the executor's
/// spawn, several files from the sink that caused it. The bound is stated here,
/// where a sink author reads it, rather than discovered there.
pub trait ProjectedEventSink: Send {
    fn emit(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused>;

    /// The same emit, plus what the producer decided about this event.
    ///
    /// [`ProjectorCursor::project`] calls **this**, never [`Self::emit`]
    /// directly, so a sink that overrides it is handed the routing and a sink
    /// that does not keeps the behaviour it had before this method existed.
    ///
    /// # THIS DEFAULT DISCARDS THE ROUTING, AND SAYS SO RATHER THAN IMPLYING OTHERWISE
    ///
    /// The default forwards to [`Self::emit`] and drops `routing` on the floor.
    /// That is deliberate — it is what makes adding this method break no
    /// existing sink — and it means the protection [`RecordedEventRouting`]
    /// exists to provide is **NOT in force for a sink that has not overridden
    /// this**. A sink on the default takes every record down whatever single
    /// path its `emit` has, which for a sink built on
    /// `phases::outbox::rejoin` alone is the transport-only branch, which is
    /// the silent failure that type documents.
    ///
    /// So: the record now carries the decision, and the last delivery step is
    /// the sink's to honour. Overriding this is what discharges the obligation;
    /// the field alone does not.
    fn emit_routed(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        let _ = routing;
        self.emit(key, event_type, payload)
    }

    /// Emit one [`JournalBody::NamedEvent`] record.
    ///
    /// The five arguments after `key` are
    /// `RuntimeTransportBroadcaster::emit_named`'s five arguments. A sink that
    /// holds a broadcaster should call that method with them and nothing else;
    /// see the variant's docs for why the *call* is what the record holds rather
    /// than the `RuntimeTransportEvent` it ends up as.
    ///
    /// # REQUIRED, and the three defaults it could have had are all wrong
    ///
    /// [`Self::emit_routed`] has a default and this deliberately does not, so
    /// the reason is worth writing down rather than leaving as an inconsistency
    /// for somebody to tidy away:
    ///
    /// - **Forwarding to [`Self::emit`]** is what `emit_routed` does, and it
    ///   works there because the record's other two fields *are* `emit`'s two
    ///   arguments. Here there is no `event_type`/`payload` pair that means the
    ///   same thing — synthesising one would be the second event vocabulary this
    ///   design refuses, and it would deliver through a branch that loses the
    ///   chat fan-out.
    /// - **`Ok(())`** advances the mark past a record nothing emitted. Once the
    ///   mark is durable nothing re-offers it, so every named event of every run
    ///   would be lost silently — verbatim the "marked emitted by nobody"
    ///   failure [`ProjectorCursor::project`]'s exhaustive match exists to make
    ///   unreachable.
    /// - **`Err`** buries every record above it for the life of the run, because
    ///   the retry is unbounded and the refusal is one no later attempt clears.
    ///   See [`EmitRefused`].
    ///
    /// So there is no honest default, and a required method is the only shape
    /// that makes the next sink author answer. There were three implementors
    /// when this was added — `driver_worker::HostEventSink` and one recording
    /// sink in each of this module's and `store`'s test suites — and requiring
    /// it is what put the question in front of all three.
    fn emit_named(
        &mut self,
        key: &EventKey,
        name: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused>;
}

/// What one call to [`ProjectorCursor::project`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Projection {
    /// Records handed to the sink and accepted.
    pub emitted: usize,
    /// Records skipped because their address had already been emitted — a
    /// re-run's duplicate, which is the case dedupe exists for.
    pub deduped: usize,
    /// Where the walk stopped, when a sink refused. The seq is the record that
    /// was **not** emitted, and the mark sits below it, so the next projection
    /// starts there.
    pub stopped_at: Option<(u64, EmitRefused)>,
    /// The mark points past the committed watermark.
    ///
    /// Not reachable through this module — [`Self::stopped_at`] aside, `project`
    /// never marks a record above the watermark it was given. It means a
    /// persisted cursor was restored against a journal that no longer reaches it:
    /// a hand edit, a restored backup, or a mark saved for a different execution.
    /// Nothing is emitted while it holds, and a projector that answered a silent
    /// `emitted: 0` would look identical to a run with nothing to say.
    pub mark_beyond_watermark: bool,
}

/// One record's emittable content, borrowed out of the record.
///
/// # Why a type and not two dispatch arms
///
/// [`ProjectorCursor::project`] has to do three things in a fixed order:
/// decide whether a record emits at all, dedupe it on its address, then hand it
/// to the sink. Written as one match per body, the dedupe would appear once per
/// emitting arm, and a rail whose arm forgot it would re-emit a duplicate that
/// [`Projection`] reports as an ordinary `emitted`. Classifying first collapses
/// that to one dedupe for every rail there will ever be, and leaves exactly two
/// exhaustive matches: one over [`JournalBody`], which a new body breaks, and
/// one over this, which a new rail breaks.
///
/// # It allocates nothing
///
/// Every field is a borrow of the record the walk is already holding, including
/// the two `Option<&str>` that `as_deref` makes from the record's owned
/// `Option<String>`. A projection of *n* records therefore costs *n* of these on
/// the stack and no heap traffic at all, which matters because the walk runs on
/// every commit boundary — six times an iteration.
enum Emittable<'r> {
    Event {
        event_type: &'r str,
        payload: &'r serde_json::Value,
        routing: &'r RecordedEventRouting,
    },
    Named {
        name: &'r str,
        agent_id: &'r str,
        principal: Option<&'r str>,
        workspace: Option<&'r str>,
        payload: &'r serde_json::Value,
    },
}

impl ProjectorCursor {
    /// How many recently-emitted addresses are remembered.
    ///
    /// Several iterations' worth. A re-run after a crash repeats at most the
    /// records of the phase that crashed, so the distance between an original
    /// and its duplicate is bounded by an iteration.
    pub const DEFAULT_WINDOW: usize = 1_024;

    /// The largest window a cursor may carry.
    ///
    /// Projection builds a bounded `HashSet` index over the persisted ordered
    /// vector, so membership is expected `O(1)` per record while the vector
    /// retains the stable wire shape and eviction order. A window is still a
    /// *number in a file* once cursors are persisted, so it remains bounded to
    /// constrain memory and the one-time index build.
    ///
    /// # It is bounded by what the durable store can WRITE BACK, not by taste
    ///
    /// It was 64× the default — 65_536 addresses — and that admitted a cursor
    /// `store::fs` can never persist. A hand-edited `projector.json` naming a
    /// window of 65_536 with an empty key list is a few dozen bytes, so it passes
    /// both read ceilings; the run then grows `recent_keys` normally, and the
    /// encoded cursor crosses `MAX_PROJECTOR_JSON_NODES` at about 3_276 addresses
    /// and `MAX_PROJECTOR_BYTES` at about 4_766. From there
    /// `save_projector_cursor` fails on every boundary for the rest of the run —
    /// logged and never returned, per `driver_worker::project_outbox`, so the run
    /// carries on with an unsaved mark and re-emits that boundary's events on
    /// every subsequent pass.
    ///
    /// So the ceiling is **2× the default**: a full `MAX_WINDOW` is ~10_244 JSON
    /// nodes and remains comfortably inside what that store reads and
    /// writes. That makes the property structural rather than documented — no
    /// window this reader accepts can grow into a mark this store refuses — and
    /// it is why lowering it was the fix rather than raising the store's
    /// ceilings, which exist to reject a file this store did not write.
    ///
    /// Two dimensions remain unbounded by this constant and are named so they are
    /// not mistaken for covered: a hand-edited file may carry `EventKey`s whose
    /// `execution_id` is far past the 128 bytes `ExecutionKey` caps a real one
    /// at, and the byte ceiling rather than this one is what stops those.
    ///
    /// # What the ceiling does NOT do
    ///
    /// It caps the in-memory index and persisted cursor, not the journal. At the
    /// reader's own [`MAX_JOURNAL_RECORDS`], a projection may still walk 200,000
    /// records, but membership no longer multiplies that work by the window.
    ///
    /// What keeps a real projection cheap is the *mark*, not this constant: a
    /// projection walks from `emitted_through_seq + 1`, which is a boundary's
    /// worth of records rather than the whole file, and every honest window is
    /// [`DEFAULT_WINDOW`](Self::DEFAULT_WINDOW). The in-memory `HashSet` keeps
    /// membership independent of the configured window while the `Vec` remains
    /// the durable ordering.
    pub const MAX_WINDOW: usize = 2 * Self::DEFAULT_WINDOW;

    pub fn new() -> Self {
        Self::with_window(Self::DEFAULT_WINDOW)
    }

    /// A projector with a smaller memory. `window` is clamped into
    /// `1..=`[`Self::MAX_WINDOW`]: zero would silently disable dedupe, which is
    /// the failure this type exists to prevent.
    pub fn with_window(window: usize) -> Self {
        Self {
            emitted_through_seq: 0,
            runtime_settled_terminal_seq: None,
            recent_keys: Vec::new(),
            window: window.clamp(1, Self::MAX_WINDOW),
        }
    }

    pub fn emitted_through_seq(&self) -> u64 {
        self.emitted_through_seq
    }

    /// Whether projection must start at seq one to migrate a legacy positional
    /// dedupe key. Once the converted cursor is saved, indexed stores may read
    /// only the pending append batch.
    pub(crate) fn requires_complete_history(&self, execution_id: &str) -> bool {
        self.recent_keys
            .iter()
            .any(|key| key.execution_id == execution_id)
    }

    /// Exact `RunEnded` watermark accepted by the Artifact/runtime lifecycle.
    /// Equality, not ordering, is the discovery predicate so a stale or
    /// prepublished receipt can never suppress a different committed ending.
    pub fn runtime_settled_terminal_seq(&self) -> Option<u64> {
        self.runtime_settled_terminal_seq
    }

    /// Record cross-layer settlement only after its owner has acknowledged the
    /// exact terminal. The enclosing store write remains lease-fenced.
    pub fn mark_runtime_terminal_settled(&mut self, terminal_seq: u64) {
        self.runtime_settled_terminal_seq = Some(terminal_seq);
    }

    /// Acknowledge an exact committed terminal batch that a stronger durable
    /// lifecycle disposition deliberately suppresses. Cancellation is the only
    /// production caller: after the runtime has durably become `Cancelled`, a
    /// receipt-owned HITL request must never be published later. The enclosing
    /// store write is still lease-fenced and the caller must have verified the
    /// terminal watermark before using this narrow escape hatch.
    pub(crate) fn suppress_terminal_batch_through(&mut self, terminal_seq: u64) {
        self.emitted_through_seq = self.emitted_through_seq.max(terminal_seq);
        self.runtime_settled_terminal_seq = Some(terminal_seq);
    }

    /// The records this projector should emit now.
    ///
    /// Bounded by `watermark` — the committed `journal_seq` — so an orphaned
    /// attempt's events are never emitted at all. That is the watermark rule
    /// paying for itself twice: it keeps replay honest, and it means the
    /// duplicate-record case above mostly never arises.
    ///
    /// # Test-only, and that is the same enforcement `mark_emitted` gets
    ///
    /// This was `pub`, with the hazard held off by the sentence "a caller that
    /// emits from this list is a caller that will emit the same list again" — a
    /// correctness property of somebody else's loop defended by a doc comment,
    /// which is exactly the reliance [`Self::mark_emitted`] was made private to
    /// remove. It was worse than the original hazard: the only method that moves
    /// the mark is private, so an external caller that reached for the
    /// obvious-looking read side and emitted from it had no way to advance past
    /// what it emitted, and would re-emit the same records on every pass forever.
    ///
    /// There were never any callers outside this module. `#[cfg(test)]` costs
    /// nothing and makes the claim in the type docs — that the bad order is
    /// unreachable rather than discouraged — true of the whole surface instead of
    /// one method of it. A future call site that wants to know how much is
    /// waiting should get a **count** from this module, not a list it can emit.
    #[cfg(test)]
    fn pending<'a>(
        &self,
        journal: &'a Journal,
        execution_id: &str,
        watermark: u64,
    ) -> Vec<&'a JournalRecord> {
        journal
            .authoritative_from(self.emitted_through_seq.saturating_add(1), watermark)
            .iter()
            // Both emittable bodies, so this read side and [`Self::project`]'s
            // walk agree about what is waiting. A filter that named only `Event`
            // would under-report a run whose phase emitted named events, which is
            // exactly the kind of quiet disagreement a test-only read side is
            // for finding rather than causing.
            .filter(|record| {
                matches!(
                    record.body,
                    JournalBody::Event { .. } | JournalBody::NamedEvent { .. }
                )
            })
            .filter(|record| !self.recent_keys.contains(&record.event_key(execution_id)))
            .collect()
    }

    /// Emit everything authoritative that has not been emitted, in seq order.
    ///
    /// The outbox's read side, and the only thing that moves the mark.
    ///
    /// # What it walks, and what it refuses to walk
    ///
    /// `watermark` is the committed
    /// [`LoopState::journal_seq`](super::state::LoopState::journal_seq). Records
    /// above it are an orphaned attempt by a worker that appended and then failed
    /// to commit; they are never projected, so a crashed attempt's events are
    /// never emitted at all rather than emitted and then retracted.
    ///
    /// # Three things happen to a record, and only one of them emits
    ///
    /// - **Not an event** — a phase completion, an owner transition. The mark
    ///   advances past it. It will never need emitting and rescanning it on every
    ///   projection is the only thing not advancing would buy.
    /// - **An event whose address is already in the window** — a re-run's
    ///   duplicate. Counted in [`Projection::deduped`], and the mark advances past
    ///   it, because it will never need emitting either.
    /// - **An event with a new address** — handed to the sink. On `Ok` the mark
    ///   advances to it and its address enters the window. On [`EmitRefused`] the
    ///   walk **stops with the mark still below it**, so the next projection
    ///   retries the same record. Skipping ahead here is the bug this ordering is
    ///   built to make unreachable: the mark would pass a record that was never
    ///   emitted, and nothing would ever come back for it.
    ///
    /// # The retry is UNBOUNDED, and that is a constraint on sinks
    ///
    /// There is no attempt counter and no poison-pill skip. A sink that refuses
    /// the same record every time buries every record above it for the life of
    /// the run — the correct answer for a transport that is down, and a permanent
    /// outage for a refusal the record itself determines. [`EmitRefused`]'s docs
    /// carry the rule that follows from it; a sink author must read them before
    /// returning `Err` for anything other than a condition a later attempt could
    /// clear.
    ///
    /// # `execution_id` is the JOURNAL'S key, and a sink must not read identity
    /// # out of it
    ///
    /// The only production caller — `driver_worker::project_outbox` — passes
    /// `key.execution_id()`, a loop-state address that carries `-r{n}` /
    /// `-n{…}` / `-p{n}` for a resumed, nested or refinement-pass invocation.
    /// It flows into every [`EventKey`] this walk builds, which is correct for
    /// dedupe and wrong for identity. A sink implementing
    /// [`ProjectedEventSink::emit_routed`] must take the execution id of a
    /// [`RecordedEventRouting::CanonicalRuntimeFact`]'s scope from the run, not
    /// from this argument. See [`RecordedCanonicalScope`]'s *THE FILE KEY IS NOT
    /// THE EXECUTION ID*.
    pub fn project(
        &mut self,
        journal: &Journal,
        execution_id: &str,
        watermark: u64,
        sink: &mut dyn ProjectedEventSink,
    ) -> Projection {
        self.project_records(
            journal.authoritative(watermark),
            execution_id,
            watermark,
            sink,
        )
    }

    /// Project a store-verified authoritative window.
    ///
    /// The window begins at seq one when [`Self::requires_complete_history`]
    /// is true. Otherwise it begins at the start of the append batch containing
    /// `emitted_through_seq + 1`; that prefix is enough to reconstruct stable
    /// occurrence numbers without walking the historical log.
    pub(crate) fn project_records(
        &mut self,
        authoritative: &[JournalRecord],
        execution_id: &str,
        watermark: u64,
        sink: &mut dyn ProjectedEventSink,
    ) -> Projection {
        let mut projection = Projection::default();
        if self.emitted_through_seq > watermark {
            // Reported rather than returning an indistinguishable empty result.
            // The two states — "nothing to emit" and "this cursor does not belong
            // to this journal" — look identical in a count and need different
            // answers from whoever is watching.
            projection.mark_beyond_watermark = true;
            return projection;
        }
        let from = self.emitted_through_seq.saturating_add(1);
        // Cursors written before content-addressed keys contain positional
        // addresses. Convert each to the FIRST authoritative body that occupied
        // it. The old projector emitted that body and suppressed later
        // occupants, so blindly matching the positional key against a later,
        // different body would preserve the old data-loss bug during migration.
        // This whole-log pass runs only while an old-format key remains; the
        // converted cursor is saved by the caller and later passes return to a
        // pending-suffix walk.
        if self
            .recent_keys
            .iter()
            .any(|key| key.execution_id == execution_id)
        {
            let mut migrations: HashMap<EventKey, EventKey> = HashMap::new();
            let mut previous_address: Option<(usize, Phase, u32)> = None;
            let mut occurrences: HashMap<String, u32> = HashMap::new();
            for record in authoritative {
                let address = (record.iteration, record.phase, record.ordinal);
                if previous_address.is_some_and(|previous| {
                    previous.0 != address.0 || previous.1 != address.1 || address.2 <= previous.2
                }) {
                    occurrences.clear();
                }
                previous_address = Some(address);
                let Some(fingerprint) = record.event_fingerprint() else {
                    continue;
                };
                let occurrence = occurrences.entry(fingerprint.clone()).or_insert(0);
                let stable = record.stable_event_key(execution_id, &fingerprint, *occurrence);
                *occurrence = occurrence.saturating_add(1);
                migrations
                    .entry(record.event_key(execution_id))
                    .or_insert(stable);
            }
            let mut seen = HashSet::new();
            self.recent_keys = self
                .recent_keys
                .drain(..)
                .filter_map(|key| {
                    let migrated = if key.execution_id == execution_id {
                        migrations.get(&key).cloned()
                    } else {
                        Some(key)
                    };
                    migrated.filter(|key| seen.insert(key.clone()))
                })
                .collect();
        }
        // `recent_keys` is the stable wire ordering; this bounded set is the hot
        // membership index. Rebuilding once per projection is O(window), rather
        // than scanning the vector for every pending record.
        let mut recent_key_set: HashSet<EventKey> = self.recent_keys.iter().cloned().collect();
        let first_pending = authoritative.partition_point(|record| record.seq < from);
        if first_pending == authoritative.len() {
            return projection;
        }

        // Recover the beginning of the append batch containing `first_pending`.
        // Ordinals restart at zero for each host batch; phase completion uses
        // the reserved maximum. Starting here, rather than at seq one, keeps the
        // work proportional to the pending batch while still reconstructing the
        // occurrence number of an identical event before the durable mark.
        let mut batch_start = first_pending;
        while batch_start > 0 {
            let previous = &authoritative[batch_start - 1];
            let current = &authoritative[batch_start];
            if previous.iteration != current.iteration
                || previous.phase != current.phase
                || current.ordinal <= previous.ordinal
            {
                break;
            }
            batch_start -= 1;
        }

        let mut previous_address: Option<(usize, Phase, u32)> = None;
        let mut occurrences: HashMap<String, u32> = HashMap::new();
        for record in &authoritative[batch_start..] {
            let address = (record.iteration, record.phase, record.ordinal);
            if previous_address.is_some_and(|previous| {
                previous.0 != address.0 || previous.1 != address.1 || address.2 <= previous.2
            }) {
                occurrences.clear();
            }
            previous_address = Some(address);

            let stable_key = match &record.body {
                JournalBody::Event { .. } | JournalBody::NamedEvent { .. } => {
                    let fingerprint = record
                        .event_fingerprint()
                        .expect("the match above selected an event body");
                    let occurrence = occurrences.entry(fingerprint.clone()).or_insert(0);
                    let key = record.stable_event_key(execution_id, &fingerprint, *occurrence);
                    *occurrence = occurrence.saturating_add(1);
                    Some(key)
                },
                JournalBody::PhaseCompleted { .. }
                | JournalBody::RecoveryRewind { .. }
                | JournalBody::OwnerTransition { .. } => None,
            };

            // Records before the durable mark are visited only to reconstruct
            // the current batch's duplicate occurrence counters.
            if record.seq < from {
                continue;
            }
            // EXHAUSTIVE, with the non-emitting bodies named, and that is the
            // point of writing it out rather than `else`. Advancing the mark past
            // a record is the irreversible half of this walk: once the mark is
            // durable nothing re-offers what it passed. A catch-all would make
            // every future `JournalBody` variant silently non-emittable — the
            // exact "marked emitted by nobody" failure the ordering above exists
            // to prevent — with no compile error and no log line. Same standard
            // as [`JournalAppend::event`]'s probe match: the next author has to
            // answer the question.
            //
            // It classifies into [`Emittable`] rather than dispatching here, so
            // the dedupe below runs once for both rails. Two matches that each
            // dispatched would each need their own dedupe, and a rail deduped in
            // one and not the other is a re-emit nothing reports.
            let emittable = match &record.body {
                JournalBody::Event {
                    event_type,
                    payload,
                    routing,
                } => Emittable::Event {
                    event_type,
                    payload,
                    routing,
                },
                JournalBody::NamedEvent {
                    name,
                    agent_id,
                    principal,
                    workspace,
                    payload,
                } => Emittable::Named {
                    name,
                    agent_id,
                    principal: principal.as_deref(),
                    workspace: workspace.as_deref(),
                    payload,
                },
                JournalBody::PhaseCompleted { .. }
                | JournalBody::RecoveryRewind { .. }
                | JournalBody::OwnerTransition { .. } => {
                    self.advance_past(record.seq);
                    continue;
                },
            };
            // Both rails share a CONTENT address space. Different bodies at one
            // positional ordinal therefore remain different facts when retry
            // admission changes membership, while logically equivalent bodies retain
            // one identity. The occurrence counter preserves two deliberate
            // identical emissions from one append batch.
            let key = stable_key.expect("every emittable journal body has a stable key");
            if recent_key_set.contains(&key) {
                projection.deduped += 1;
                self.advance_past(record.seq);
                continue;
            }
            // Both arms end at a `Result<(), EmitRefused>` handled identically
            // below, so the emit/mark/refuse ordering — the irreversible half of
            // this walk — has one implementation and cannot be got right for one
            // rail and wrong for the other.
            let delivered = match emittable {
                // `emit_routed`, not `emit`. The routing is the half of the
                // record a sink cannot re-derive — see [`RecordedEventRouting`]
                // — and the walk is the only place that has it. Calling `emit`
                // here would put the field in the log and never hand it to the
                // one party that acts on it.
                Emittable::Event {
                    event_type,
                    payload,
                    routing,
                } => sink.emit_routed(&key, event_type, payload, routing),
                Emittable::Named {
                    name,
                    agent_id,
                    principal,
                    workspace,
                    payload,
                } => sink.emit_named(&key, name, agent_id, principal, workspace, payload),
            };
            match delivered {
                Ok(()) => {
                    projection.emitted += 1;
                    // The key built above, MOVED rather than rebuilt.
                    // `mark_emitted` used to take `(&record, execution_id)` and
                    // call `record.event_key(execution_id)` for itself, which
                    // allocated a second `String` for the id and reassembled a
                    // key this function had already built and already used for
                    // the dedupe test twelve lines up.
                    let indexed = key.clone();
                    if let Some(evicted) = self.mark_emitted(record.seq, key) {
                        recent_key_set.remove(&evicted);
                    }
                    recent_key_set.insert(indexed);
                },
                Err(refused) => {
                    projection.stopped_at = Some((record.seq, refused));
                    return projection;
                },
            }
        }
        projection
    }

    /// Move the mark past a record that will never need emitting.
    ///
    /// Monotonic, so a record arriving out of order — which [`Self::project`]'s
    /// walk cannot produce — cannot rewind the mark and re-emit everything above
    /// it.
    fn advance_past(&mut self, seq: u64) {
        self.emitted_through_seq = self.emitted_through_seq.max(seq);
    }

    /// Record that a projector emitted a record.
    ///
    /// **Private, and that is the enforcement.** See the type docs: published, it
    /// asked a caller's loop to mark in `pending` order, and a caller that marked
    /// a later record first buried an earlier one whose event was then never
    /// emitted. [`Self::project`] is the only caller and it walks in seq order, so
    /// the order is a property of this module rather than a request made of
    /// another one.
    ///
    /// # It takes the KEY, not the record — changed 2026-08-28
    ///
    /// The signature was `(&JournalRecord, &str)` and the first thing the body
    /// did was `record.event_key(execution_id)`: a fresh `EventKey`, with a
    /// fresh `String` for the id, identical to the one [`Self::project`] built a
    /// dozen lines earlier and tested the window against. One address was
    /// therefore assembled twice for every record that emitted. Taking the key
    /// by value moves the caller's.
    ///
    /// # And it no longer re-tests membership, which is the second half
    ///
    /// The body used to be `if !self.recent_keys.contains(&key) { push }`. That
    /// test could not fail: `project` runs the *same* `contains` against the
    /// *same* `recent_keys` before it emits and `continue`s on a hit, and
    /// nothing between the two touches the window — the sink is a separate
    /// `&mut dyn ProjectedEventSink`. So the guard was a second linear scan of
    /// up to [`Self::MAX_WINDOW`] keys, with a `String` compare each, per
    /// emitted record, to re-derive an answer the caller had.
    ///
    /// **The no-duplicate invariant is unchanged; only its enforcement moved.**
    /// `ProjectorCursorWire`'s deduplication comment and
    /// [`recent_keys_bounded`] both rely on that invariant, and both still hold.
    /// The `debug_assert!` keeps it checked under `cargo test` and every debug
    /// build, so a second caller added later that skipped the dedupe branch
    /// fails loudly there rather than silently shrinking the window's effective
    /// distance in release.
    fn mark_emitted(&mut self, seq: u64, key: EventKey) -> Option<EventKey> {
        self.advance_past(seq);
        debug_assert!(
            !self.recent_keys.contains(&key),
            "mark_emitted was handed an address already in the dedupe window; its caller must \
             have taken the dedupe branch instead of emitting"
        );
        self.recent_keys.push(key);
        if self.recent_keys.len() > self.window {
            Some(self.recent_keys.remove(0))
        } else {
            None
        }
    }
}

impl Default for ProjectorCursor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_event_reference_is_stable_and_field_sensitive() {
        let key = EventKey {
            execution_id: "exec:with:delimiters#body".to_string(),
            iteration: 4,
            phase: Phase::Apply,
            ordinal: 2,
        };
        let reference = key.source_event_ref();
        assert_eq!(reference, key.clone().source_event_ref());
        assert!(reference.starts_with("evt_loop_v1_"));
        assert_eq!(reference.len(), "evt_loop_v1_".len() + 64);

        let mut changed = key.clone();
        changed.ordinal += 1;
        assert_ne!(reference, changed.source_event_ref());
        changed = key.clone();
        changed.execution_id.push_str(":suffix");
        assert_ne!(reference, changed.source_event_ref());
    }

    /// The instant this module's one commit projection is judged against.
    ///
    /// A constant rather than `Utc::now()`, for the same reason the equivalent
    /// in `state.rs` is: `CommitPoint::now_ms` feeds a pin expiry, and a fixture
    /// that read the real clock would make what it asserts depend on when the
    /// suite ran.
    const TEST_NOW_MS: i64 = 1_700_000_000_000;

    fn record(seq: u64, iteration: usize, phase: Phase, step: RecordedStep) -> JournalRecord {
        JournalRecord {
            seq,
            iteration,
            phase,
            ordinal: 0,
            at_ms: 1_700_000_000_000,
            body: JournalBody::PhaseCompleted { step },
        }
    }

    fn event(seq: u64, iteration: usize, phase: Phase, ordinal: u32) -> JournalRecord {
        JournalRecord {
            seq,
            iteration,
            phase,
            ordinal,
            at_ms: 1_700_000_000_000,
            body: JournalBody::Event {
                event_type: "iteration_started".to_string(),
                payload: serde_json::json!({ "iteration": iteration }),
                // `Unrecorded`, and named rather than defaulted: the cases this
                // fixture feeds are about where the MARK ends up, and giving it
                // a routing would put a value in them that no assertion reads.
                // The routing's own delivery is covered by `event_of`'s cases.
                routing: RecordedEventRouting::Unrecorded,
            },
        }
    }

    /// An event carrying a caller-chosen type and payload.
    ///
    /// [`event`] hardcodes both, which is fine for the tests that only care where
    /// the mark ends up and useless for the one that asks whether the sink got
    /// what the record held: every fixture sharing one type means an assertion
    /// against that type is an assertion against a constant, and passes a
    /// projector that hardcoded the same string.
    fn event_of(
        seq: u64,
        iteration: usize,
        phase: Phase,
        ordinal: u32,
        event_type: &str,
        payload: serde_json::Value,
        routing: RecordedEventRouting,
    ) -> JournalRecord {
        JournalRecord {
            seq,
            iteration,
            phase,
            ordinal,
            at_ms: 1_700_000_000_000,
            body: JournalBody::Event {
                event_type: event_type.to_string(),
                payload,
                routing,
            },
        }
    }

    /// A canonical scope with every field distinct.
    ///
    /// All four differ from each other on purpose: a scope built from one
    /// repeated string would pass a reader that transposed `workspace` and
    /// `task_id`, which is the mistake a four-field positional rebuild makes.
    fn a_scope() -> RecordedCanonicalScope {
        RecordedCanonicalScope {
            principal: "principal-p".to_string(),
            workspace: "workspace-w".to_string(),
            task_id: "task-t".to_string(),
            ui_thread_id: "thread-u".to_string(),
        }
    }

    /// One full iteration that ends by exiting, then the epilogue.
    fn one_iteration(start_seq: u64, iteration: usize) -> Vec<JournalRecord> {
        vec![
            record(
                start_seq,
                iteration,
                Phase::Prepare,
                RecordedStep::Continued,
            ),
            record(
                start_seq + 1,
                iteration,
                Phase::Observe,
                RecordedStep::Continued,
            ),
            record(
                start_seq + 2,
                iteration,
                Phase::Decide,
                RecordedStep::Continued,
            ),
            record(
                start_seq + 3,
                iteration,
                Phase::Resolve,
                RecordedStep::Continued,
            ),
            record(
                start_seq + 4,
                iteration,
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
            ),
            record(
                start_seq + 5,
                iteration,
                Phase::Epilogue,
                RecordedStep::Continued,
            ),
        ]
    }

    #[test]
    fn a_torn_final_line_is_dropped_and_everything_before_it_survives() {
        // The crash this tolerates: an append interrupted mid-write. The record
        // was never acknowledged, so dropping it loses nothing — but dropping
        // anything else would lose a step of history.
        let complete = one_iteration(1, 1);
        let mut raw = String::new();
        for entry in &complete {
            raw.push_str(&entry.to_line().expect("a fixture record must encode"));
            raw.push('\n');
        }
        raw.push_str("{\"seq\":7,\"iteration\":2,\"pha");

        let journal = Journal::parse(&raw).expect("a torn tail must be tolerated");
        assert_eq!(journal.all_records().len(), 6);
        assert_eq!(journal.last_seq(), 6);
    }

    #[test]
    fn a_torn_line_that_is_not_the_tail_is_corruption() {
        let complete = one_iteration(1, 1);
        let mut raw = String::new();
        raw.push_str(&complete[0].to_line().expect("encode"));
        raw.push('\n');
        raw.push_str("{\"seq\":2,\"iterat");
        raw.push('\n');
        raw.push_str(&complete[2].to_line().expect("encode"));
        raw.push('\n');

        let error = Journal::parse(&raw).expect_err("a hole in the middle must be refused");
        assert!(
            matches!(error, JournalError::CorruptRecord { line: 2, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_complete_final_line_that_does_not_parse_is_still_corruption() {
        // The tolerance is for a torn tail, which is only possible when the file
        // does not end in a newline. A file that DOES end in one has no
        // incomplete line by construction, so a parse failure on its last line
        // is real damage and must not ride the same exemption.
        let mut raw = String::new();
        raw.push_str(
            &one_iteration(1, 1)[0]
                .to_line()
                .expect("a fixture record must encode"),
        );
        raw.push('\n');
        raw.push_str("{\"seq\":2,\"nonsense\":true}\n");

        let error = Journal::parse(&raw).expect_err("a newline-terminated bad line is corruption");
        assert!(
            matches!(error, JournalError::CorruptRecord { line: 2, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_blank_final_line_does_not_lend_its_exemption_to_the_record_above_it() {
        // The torn-tail exemption belongs to the last line OF THE FILE. Anchoring
        // it to the last line that held something instead — which is what
        // filtering blank lines before choosing the tail does — hands the
        // exemption to a complete, newline-terminated record and swallows real
        // corruption as a torn write.
        let mut raw = String::new();
        raw.push_str(
            &record(1, 1, Phase::Prepare, RecordedStep::Continued)
                .to_line()
                .expect("encode"),
        );
        raw.push('\n');
        raw.push_str("{\"seq\":2,\"iterat");
        raw.push('\n');
        raw.push_str("   ");

        let error = Journal::parse(&raw)
            .expect_err("a newline-terminated bad line is corruption whatever follows it");
        assert!(
            matches!(error, JournalError::CorruptRecord { line: 2, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_line_too_long_to_have_been_written_is_refused_even_as_a_tail() {
        // The write bound is only half of one. A file written by something else —
        // an older build, a hand edit, two records run together by a corruption —
        // must not make a reader take a record no writer could have produced, and
        // the torn-tail exemption must not be the way it gets in: a torn line is
        // a PREFIX of a complete one, so it is never longer than one.
        let mut raw = String::new();
        raw.push_str(
            &record(1, 1, Phase::Prepare, RecordedStep::Continued)
                .to_line()
                .expect("encode"),
        );
        raw.push('\n');
        raw.push_str(&"x".repeat(MAX_JOURNAL_RECORD_BYTES + 1));

        let error =
            Journal::parse(&raw).expect_err("an over-long line is corruption, not a record");
        assert!(
            matches!(error, JournalError::CorruptRecord { line: 2, .. }),
            "got {error:?}"
        );
        assert!(error.to_string().contains("record limit"), "{error}");
    }

    #[test]
    fn an_empty_file_is_an_empty_journal_and_not_a_torn_one() {
        // Zero length is a real state — a journal whose directory exists and
        // whose first append has not landed — and it must read as "no records"
        // rather than as damage. An unreadable file is the store's error to
        // raise; it must never arrive here as this same value.
        let journal = Journal::parse("").expect("an empty file is an empty journal");
        assert!(journal.all_records().is_empty());
        assert_eq!(journal.last_seq(), 0);
        assert_eq!(
            replay(journal.authoritative(0)),
            Ok(ReplayedCursor::start())
        );

        // A file of nothing but blank lines is the same answer, not a torn tail
        // that swallowed a record.
        let blank = Journal::parse("\n\n   \n").expect("blank lines hold no records");
        assert!(blank.all_records().is_empty());
    }

    #[test]
    fn a_seq_gap_is_refused_rather_than_replayed_over() {
        let mut raw = String::new();
        raw.push_str(
            &record(1, 1, Phase::Prepare, RecordedStep::Continued)
                .to_line()
                .expect("encode"),
        );
        raw.push('\n');
        raw.push_str(
            &record(3, 1, Phase::Decide, RecordedStep::Continued)
                .to_line()
                .expect("encode"),
        );
        raw.push('\n');

        let error = Journal::parse(&raw).expect_err("a lost record must not be papered over");
        assert_eq!(
            error,
            JournalError::SeqGap {
                expected: 2,
                found: 3
            }
        );
    }

    #[test]
    fn a_seq_that_goes_backwards_names_the_right_cause() {
        // A duplicate seq means two holders appended to one journal — a lease
        // that was not held. Reporting it as a gap would send an operator
        // looking for lost bytes when the bytes are all there and the lease is
        // the problem.
        let error = Journal::from_records(vec![
            record(1, 1, Phase::Prepare, RecordedStep::Continued),
            record(1, 1, Phase::Observe, RecordedStep::Continued),
        ])
        .expect_err("a repeated seq must be refused");
        assert_eq!(
            error,
            JournalError::SeqRewind {
                expected: 2,
                found: 1
            }
        );
        assert!(error.to_string().contains("two holders"), "{error}");

        // And a genuine gap still reads as a gap.
        let error = Journal::from_records(vec![
            record(1, 1, Phase::Prepare, RecordedStep::Continued),
            record(3, 1, Phase::Observe, RecordedStep::Continued),
        ])
        .expect_err("a missing seq must be refused");
        assert_eq!(
            error,
            JournalError::SeqGap {
                expected: 2,
                found: 3
            }
        );
        assert!(error.to_string().contains("were lost"), "{error}");
    }

    #[test]
    fn replay_reproduces_the_cursor_at_every_seq() {
        // The determinism property the design asks for, at the grain it asks for
        // it: "replaying a journal reproduces the recorded state at every seq",
        // not only at the end.
        let records = one_iteration(1, 1);
        let cursors = replay_each(&records).expect("a well-formed journal must replay");

        assert_eq!(cursors.len(), records.len());
        assert_eq!(cursors[0].phase, Phase::Observe);
        assert_eq!(cursors[1].phase, Phase::Decide);
        assert_eq!(cursors[2].phase, Phase::Resolve);
        assert_eq!(cursors[3].phase, Phase::Apply);
        // The exit does not end the run and does not skip the epilogue.
        assert_eq!(cursors[4].phase, Phase::Epilogue);
        assert_eq!(cursors[4].iteration, 1);
        // The epilogue is the only place the iteration counter steps.
        assert_eq!(cursors[5].iteration, 2);
        assert_eq!(cursors[5].phase, Phase::Prepare);
        assert!(cursors.iter().all(|cursor| cursor.terminal.is_none()));

        // Replaying a prefix must give the same cursor the full replay recorded
        // at that seq. That is the property; "the last element is right" is not.
        for (index, expected) in cursors.iter().enumerate() {
            let prefix = replay(&records[..=index]).expect("a prefix must replay");
            assert_eq!(&prefix, expected, "prefix of length {}", index + 1);
        }
    }

    /// A log that grows must never change what replay already said about the
    /// part of it that was there before.
    ///
    /// # What this adds over [`replay_reproduces_the_cursor_at_every_seq`]
    ///
    /// Not the anti-**lookahead** property, and an earlier version of this
    /// comment claimed otherwise. That test already runs the per-index prefix
    /// loop — `replay(&records[..=index])` against `replay_each(all)[index]`, at
    /// every index — so a fold that peeked at the next record to decide whether
    /// an exit had ended a run is caught there. Saying it was not was a claim
    /// about a test twenty lines above this one, and it was wrong.
    ///
    /// Three things it does add:
    ///
    /// 1. **The record vocabulary.** The log above is six `PhaseCompleted`
    ///    records over one clean iteration. This one is built out of the shapes
    ///    an ordinary run actually produces: a boundary exit, the two record
    ///    kinds that move no cursor ([`JournalBody::OwnerTransition`] and
    ///    [`JournalBody::Event`]), a **resumable** terminal, and the record that
    ///    resumes it in the same file. `replay_each`'s first cut refused that
    ///    last shape outright, and no clean-iteration log can find it.
    ///
    ///    Three of the four bodies, not four: [`JournalBody::NamedEvent`] is
    ///    cursor-neutral for the same reason [`JournalBody::Event`] is, and
    ///    `a_named_event_moves_no_cursor` pins that on its own rather than
    ///    widening this fixture, so that a change which made only one of the two
    ///    move the cursor fails in the case named for it.
    /// 2. **`replay_each` on a prefix, at every element.** The test above
    ///    compares only the LAST element a prefix replays — [`replay`] is
    ///    `replay_each(..).last()` — against the full replay. This compares the
    ///    whole vector a prefix replays against the full replay's first *k*
    ///    elements, so a `replay_each` that landed the end of a short log
    ///    correctly and its interior wrongly is caught.
    /// 3. **The seq stamp.** Every cursor here is asserted to name the seq of
    ///    the record it reflects. Nothing above looks at `seq` at all, and a
    ///    fold that computed the right position under the wrong address would
    ///    satisfy every iteration/phase assertion in this file.
    #[test]
    fn a_longer_log_never_changes_what_replay_said_about_a_shorter_one() {
        let mut records = one_iteration(1, 1);
        // Iteration 2. The completion record first and the extra record after
        // it, which is the order `driver_worker::commit_boundary` appends a
        // batch in — and it matters here rather than being decoration, because
        // the committed watermark is stamped from the LAST append. So in
        // production a state's `journal_seq` routinely names a record that moves
        // nothing, and a replay that could only answer at cursor-moving records
        // could not answer at the watermark.
        records.push(record(7, 2, Phase::Prepare, RecordedStep::Continued));
        records.push(JournalRecord {
            seq: 8,
            iteration: 2,
            phase: Phase::Prepare,
            ordinal: 1,
            at_ms: 1_700_000_000_000,
            body: JournalBody::OwnerTransition {
                from_agent_id: Some("agent-a".to_string()),
                to_agent_id: "agent-b".to_string(),
                transition_authorization: Some("invocation-7".to_string()),
            },
        });
        records.push(record(9, 2, Phase::Observe, RecordedStep::Continued));
        records.push(event(10, 2, Phase::Observe, 1));
        records.push(record(
            11,
            2,
            Phase::Decide,
            RecordedStep::RunEnded {
                terminal: TerminalKind::WaitingForUser,
            },
        ));
        records.push(record(12, 2, Phase::Decide, RecordedStep::Continued));

        let cursors = replay_each(&records).expect("a well-formed journal must replay");
        assert_eq!(cursors.len(), records.len());

        // The two records that move nothing are asserted against the cursor
        // BEFORE them rather than against a literal phase name, so the claim
        // under test is "unchanged" and not "happens to be `Observe`" — the
        // second would still hold if some future fold moved the cursor and
        // something else moved it back.
        assert_eq!(
            (cursors[7].iteration, cursors[7].phase),
            (cursors[6].iteration, cursors[6].phase),
            "an owner transition changes who runs the phase, not which phase runs"
        );
        assert_eq!(
            (cursors[9].iteration, cursors[9].phase),
            (cursors[8].iteration, cursors[8].phase),
            "an event is an outbox entry and moves no cursor"
        );
        assert_eq!(cursors[10].terminal, Some(TerminalKind::WaitingForUser));
        assert_eq!(
            cursors[11].terminal, None,
            "the answer arrived and the run is live again"
        );

        // Every cursor names the seq it reflects. A fold that computed the right
        // position and stamped the wrong seq would satisfy every
        // iteration/phase assertion in this file and still leave a caller unable
        // to address the answer it had just computed.
        for (index, cursor) in cursors.iter().enumerate() {
            assert_eq!(
                cursor.seq, records[index].seq,
                "the cursor at index {index} names the wrong seq"
            );
        }

        // THE PROPERTY. Replaying the first k records is EXACTLY the first k
        // elements of replaying all of them, at every k — which is what
        // "reproduces the recorded state at every seq" means once a log is
        // allowed to grow past the seq being asked about.
        for length in 1..=records.len() {
            let prefix = replay_each(&records[..length]).expect("a prefix must replay");
            assert_eq!(
                prefix.as_slice(),
                &cursors[..length],
                "replaying {length} records disagreed with replaying all {} about the first \
                 {length}",
                records.len()
            );
        }
    }

    #[test]
    fn a_run_that_ends_for_good_admits_no_further_records() {
        let records = vec![
            record(1, 4, Phase::Prepare, RecordedStep::Continued),
            record(
                2,
                4,
                Phase::Apply,
                RecordedStep::RunEnded {
                    terminal: TerminalKind::BudgetExhausted,
                },
            ),
        ];
        let cursor = replay(&records).expect("a terminated run must replay");
        assert_eq!(cursor.terminal, Some(TerminalKind::BudgetExhausted));
        assert_eq!(
            cursor.phase,
            Phase::Apply,
            "the cursor names where the run ended, which is where a resume would re-enter"
        );
        assert!(!TerminalKind::BudgetExhausted.is_resumable());

        let mut after = records.clone();
        after.push(record(3, 5, Phase::Prepare, RecordedStep::Continued));
        let error = replay(&after).expect_err("a record after the end must be refused");
        assert!(
            matches!(error, JournalError::RecordAfterTerminal { seq: 3, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_paused_run_that_is_answered_carries_on_in_the_same_journal() {
        // The defect this pins. A pause IS an `AgenticOutcome`, so it records as
        // a terminal — but the execution stays alive waiting for a person, and
        // when they answer it appends again. Treating every terminal as final
        // made the first pause an execution took render its journal permanently
        // unreplayable.
        let records = vec![
            record(
                1,
                4,
                Phase::Apply,
                RecordedStep::RunEnded {
                    terminal: TerminalKind::WaitingForUser,
                },
            ),
            record(2, 5, Phase::Prepare, RecordedStep::Continued),
        ];
        let cursors = replay_each(&records).expect("a resumed run must replay");

        assert_eq!(cursors[0].terminal, Some(TerminalKind::WaitingForUser));
        assert_eq!(
            cursors[1].terminal, None,
            "the answer arrived and the run is live again"
        );
        assert_eq!(cursors[1].iteration, 5);
        assert_eq!(cursors[1].phase, Phase::Observe);

        // Every resumable ending behaves the same way; the list is not a
        // sampling.
        for terminal in [
            TerminalKind::WaitingForUser,
            TerminalKind::WaitingForConfirmation,
            TerminalKind::PausedByUser,
            TerminalKind::WaitingForChildren,
            TerminalKind::Sleeping,
        ] {
            assert!(terminal.is_resumable(), "{terminal:?}");
            let resumed = vec![
                record(1, 1, Phase::Apply, RecordedStep::RunEnded { terminal }),
                record(2, 2, Phase::Prepare, RecordedStep::Continued),
            ];
            assert!(
                replay(&resumed).is_ok(),
                "{terminal:?} must not wedge the journal"
            );
        }
    }

    #[test]
    fn the_epilogue_cannot_record_an_exit() {
        let records = vec![record(
            1,
            1,
            Phase::Epilogue,
            RecordedStep::Exited {
                boundary: RecordedBoundary::Advance,
            },
        )];
        let error = replay(&records).expect_err("the epilogue is where exits land, not a source");
        assert!(
            matches!(error, JournalError::ImpossibleTransition { seq: 1, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn records_past_the_watermark_are_orphans_and_never_replayed() {
        // A worker that appended and then died leaves records ahead of the
        // committed state. Replaying them would apply work nobody committed.
        let journal = Journal::from_records(one_iteration(1, 1)).expect("well-formed");
        let authoritative = journal.authoritative(4);

        assert_eq!(authoritative.len(), 4);
        assert_eq!(journal.orphaned(4).len(), 2);
        let cursor = replay(authoritative).expect("the committed prefix must replay");
        assert_eq!(cursor.phase, Phase::Apply);
        assert_eq!(cursor.iteration, 1);
    }

    #[test]
    fn a_boundary_survives_the_round_trip_including_its_delay() {
        // The delay travels WITH the outcome — the whole reason `Retry` carries
        // a Duration rather than being signalled through a side channel. A
        // record that dropped it would let a failing provider be hammered.
        for original in [
            BoundaryOutcome::Advance,
            BoundaryOutcome::NextIteration,
            BoundaryOutcome::Retry(Duration::from_millis(2_500)),
            BoundaryOutcome::PopFrame,
        ] {
            let recorded = RecordedBoundary::from(original);
            let encoded = serde_json::to_string(&recorded).expect("a boundary must encode");
            let decoded: RecordedBoundary =
                serde_json::from_str(&encoded).expect("a boundary must decode");
            assert_eq!(BoundaryOutcome::from(decoded), original);
        }
    }

    /// A sink that keeps what it was handed and can be told to refuse an address.
    ///
    /// Refusal is addressable rather than a single "fail next" flag on purpose:
    /// the property under test is *where the walk stops and where the mark ends
    /// up*, and a fake that can only refuse the first record cannot express
    /// "emitted one, refused the second, never reached the third" — which is the
    /// only shape that tells a correct walk from one that skips ahead.
    #[derive(Default)]
    struct RecordingSink {
        /// Every address the projector ASKED about, refusals included.
        ///
        /// Distinct from `emitted`, and the distinction is the one that tells a
        /// walk that stopped at a refusal from one that carried on refusing:
        /// both accept nothing, and only this says how far the projector got.
        attempts: Vec<EventKey>,
        emitted: Vec<EventKey>,
        types: Vec<String>,
        /// Kept, not discarded. A sink that took `_payload` and dropped it left
        /// `project` free to hand the transport `Value::Null` for every event —
        /// contents stripped, every test still green. The payload is most of what
        /// an event IS, so the fake that stands in for a transport has to be able
        /// to say what it received.
        payloads: Vec<serde_json::Value>,
        /// What the record said about how the producer emitted it.
        ///
        /// Kept for the same reason `payloads` is. The routing is the one half
        /// of the record a sink cannot re-derive, so a fake that dropped it
        /// would leave `project` free to hand every sink
        /// [`RecordedEventRouting::Unrecorded`] — every live surface still fed,
        /// the persisted runtime-fact stream silently stopped, and every case
        /// here still green.
        routings: Vec<RecordedEventRouting>,
        /// Every `emit_named` call, with all five of its arguments.
        ///
        /// All five, and separate from `types`/`payloads`, for the reason those
        /// two are kept at all: a fake that recorded only the name would leave
        /// `project` free to hand the broadcaster the wrong `agent_id`, an
        /// unscoped envelope where the record said scoped, or `Value::Null` for
        /// the payload, with every case here still green. The scope pair is the
        /// one most worth carrying — `emit_scoped_or_unscoped` builds an
        /// unscoped envelope unless BOTH are `Some`, so a walk that collapsed
        /// them would change the envelope a replay produces.
        #[allow(clippy::type_complexity)]
        named: Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            serde_json::Value,
        )>,
        refuse: HashSet<EventKey>,
        /// Refuse everything, on every pass — a sink whose refusal the record
        /// determines rather than the transport's health.
        refuse_all: Option<String>,
    }

    impl RecordingSink {
        /// The address bookkeeping and the refusal decision, in ONE place.
        ///
        /// Both `emit` and `emit_named` go through it. A fake that duplicated
        /// this per rail would be free to attempt an address in one method and
        /// refuse it in the other, and every case asserting on `attempts` versus
        /// `emitted` would then be asserting about the fake rather than about
        /// the walk.
        fn admit(&mut self, key: &EventKey) -> Result<(), EmitRefused> {
            self.attempts.push(key.clone());
            if let Some(reason) = &self.refuse_all {
                return Err(EmitRefused {
                    reason: reason.clone(),
                });
            }
            if self.refuse.contains(key) {
                return Err(EmitRefused {
                    reason: format!("the transport refused {key}"),
                });
            }
            self.emitted.push(key.clone());
            Ok(())
        }

        fn named_names(&self) -> Vec<&str> {
            self.named.iter().map(|call| call.0.as_str()).collect()
        }

        fn refusing(keys: impl IntoIterator<Item = EventKey>) -> Self {
            Self {
                refuse: keys.into_iter().collect(),
                ..Self::default()
            }
        }

        fn refusing_everything(reason: &str) -> Self {
            Self {
                refuse_all: Some(reason.to_string()),
                ..Self::default()
            }
        }

        fn ordinals(&self) -> Vec<u32> {
            self.emitted.iter().map(|key| key.ordinal).collect()
        }

        fn attempted_ordinals(&self) -> Vec<u32> {
            self.attempts.iter().map(|key| key.ordinal).collect()
        }
    }

    impl ProjectedEventSink for RecordingSink {
        fn emit(
            &mut self,
            key: &EventKey,
            event_type: &str,
            payload: &serde_json::Value,
        ) -> Result<(), EmitRefused> {
            self.admit(key)?;
            self.types.push(event_type.to_string());
            self.payloads.push(payload.clone());
            Ok(())
        }

        /// Records the routing, then defers every other decision to
        /// [`Self::emit`].
        ///
        /// Split this way rather than inlined so the refusal bookkeeping stays
        /// in one place: a fake that duplicated it here would be free to attempt
        /// an address in one method and refuse it in the other, and the cases
        /// that assert on `attempts` versus `emitted` would be asserting about
        /// the fake.
        ///
        /// The push happens BEFORE the delegation, so a refused record still
        /// records the routing it was offered with — otherwise this vector
        /// would silently be "routings of records that were accepted", which is
        /// a different claim from the one its name makes.
        fn emit_routed(
            &mut self,
            key: &EventKey,
            event_type: &str,
            payload: &serde_json::Value,
            routing: &RecordedEventRouting,
        ) -> Result<(), EmitRefused> {
            self.routings.push(routing.clone());
            self.emit(key, event_type, payload)
        }

        /// Records the call, then defers the refusal decision to
        /// [`Self::admit`] — the same decision `emit` defers to.
        ///
        /// The push happens BEFORE `admit`, so a refused named record is still
        /// recorded with the arguments it was offered with. Pushing after would
        /// make `named` mean "named calls that were accepted", which is a
        /// different claim from the one the field's name makes and would hide a
        /// walk that offered the wrong arguments to a refusing sink.
        fn emit_named(
            &mut self,
            key: &EventKey,
            name: &str,
            agent_id: &str,
            principal: Option<&str>,
            workspace: Option<&str>,
            payload: &serde_json::Value,
        ) -> Result<(), EmitRefused> {
            self.named.push((
                name.to_string(),
                agent_id.to_string(),
                principal.map(str::to_string),
                workspace.map(str::to_string),
                payload.clone(),
            ));
            self.admit(key)
        }
    }

    fn key_of(record: &JournalRecord, occurrence: u32) -> EventKey {
        let fingerprint = record
            .event_fingerprint()
            .expect("the fixture passed to key_of must be an event");
        record.stable_event_key("exec-1", &fingerprint, occurrence)
    }

    #[test]
    fn a_projector_emits_a_re_run_event_once_even_though_its_seq_changed() {
        // The failure this prevents: a phase crashes after appending its event,
        // re-runs, and appends the same event at a NEW seq. A high-water mark
        // alone cannot recognise the second — the address can.
        let execution_id = "exec-1";
        let first = event(1, 1, Phase::Prepare, 0);
        let duplicate_after_crash = event(2, 1, Phase::Prepare, 0);
        let journal =
            Journal::from_records(vec![first, duplicate_after_crash]).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, execution_id, 2, &mut sink);

        assert_eq!(
            sink.emitted.len(),
            1,
            "the re-run's record carries the same address and must not reach the sink twice"
        );
        assert_eq!(projection.emitted, 1);
        assert_eq!(projection.deduped, 1);
        assert_eq!(projection.stopped_at, None);
        assert_eq!(
            projector.emitted_through_seq(),
            2,
            "the duplicate will never need emitting, so the mark passes it"
        );
    }

    #[test]
    fn a_refused_emit_stops_the_walk_and_leaves_the_mark_below_it() {
        // THE ordering property, and the reason `mark_emitted` is private. A walk
        // that skipped a refused record would leave the mark above an event
        // nothing will ever come back for — the exact "marking a later record
        // buries an earlier one" hazard, arriving from inside this module instead
        // of from a caller's loop.
        let execution_id = "exec-1";
        let records = vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 1),
            event(3, 1, Phase::Prepare, 2),
        ];
        let refused = key_of(&records[1], 1);
        let journal = Journal::from_records(records).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::refusing([refused.clone()]);
        let projection = projector.project(&journal, execution_id, 3, &mut sink);

        assert_eq!(
            sink.ordinals(),
            vec![0],
            "the walk must stop at the refusal, not carry on to the record above it"
        );
        assert_eq!(
            sink.attempted_ordinals(),
            vec![0, 1],
            "and 'stopped' must mean the record above was never OFFERED. Accepting only \
             ordinal 0 is also what a walk that offered all three and refused two would \
             produce; the attempt list is what tells the two apart"
        );
        assert_eq!(projection.emitted, 1);
        match &projection.stopped_at {
            Some((seq, refusal)) => {
                assert_eq!(*seq, 2);
                assert!(refusal.reason.contains(":1:prepare:1"), "{refusal}");
            },
            None => panic!("a refused emit must be reported, not swallowed"),
        }
        assert_eq!(
            projector.emitted_through_seq(),
            1,
            "the mark must sit BELOW the refused record so the next projection retries it"
        );

        // The transport comes back. The retry re-emits the refused record and
        // then the one above it, and does NOT re-emit the one already accepted.
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, execution_id, 3, &mut sink);
        assert_eq!(sink.ordinals(), vec![1, 2]);
        assert_eq!(projection.emitted, 2);
        assert_eq!(projection.deduped, 0);
        assert_eq!(projector.emitted_through_seq(), 3);
    }

    /// The COST of the rule above, pinned so it is a measured property rather
    /// than something the first call site discovers.
    ///
    /// The test above hands the retry a fresh, non-refusing sink, which is the
    /// transport-came-back case and the only one anything covered. A refusal the
    /// *record* determines — an `event_type` this build cannot map — never clears.
    /// `project` has no retry bound, no attempt counter and no way to step over a
    /// record, so the same record is refused on every pass and every record above
    /// it is buried for the life of the run.
    ///
    /// That is why [`ProjectedEventSink`]'s docs now tell a sink to take-and-drop
    /// an unrecognised event rather than refuse it. An earlier version told it to
    /// refuse, which was a standing instruction to produce exactly this.
    ///
    /// **What production change leaves this green?** Only one that keeps the
    /// retry unbounded. Adding a poison-pill skip or an attempt ceiling turns
    /// this red — which is the point: the next author has to delete this test
    /// deliberately, and at that moment [`EmitRefused`]'s docs stop being true and
    /// have to be rewritten with it.
    #[test]
    fn a_permanently_refusing_sink_never_advances_past_the_record() {
        let execution_id = "exec-1";
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 1),
            event(3, 1, Phase::Prepare, 2),
        ])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink =
            RecordingSink::refusing_everything("this build cannot map iteration_started");

        for pass in 1..=5 {
            let projection = projector.project(&journal, execution_id, 3, &mut sink);
            assert_eq!(
                projection.emitted, 0,
                "pass {pass}: nothing gets past the first record"
            );
            assert_eq!(
                projection.stopped_at.map(|(seq, _)| seq),
                Some(1),
                "pass {pass}: the walk stops on the same record every time"
            );
            assert_eq!(
                projector.emitted_through_seq(),
                0,
                "pass {pass}: the mark never moves, so seqs 2 and 3 are never reached"
            );
        }

        // NOT `sink.emitted.is_empty()`, which the `projection.emitted == 0`
        // above already forces and which therefore cannot fail. What the sink was
        // ASKED about is the independent fact: five passes over a three-record
        // journal put the same single address in front of it five times and never
        // mentioned the two records above it. A projector that walked on past a
        // refusal, collecting them and reporting the first, would satisfy every
        // assertion in the loop and fail here with fifteen attempts.
        assert_eq!(
            sink.attempted_ordinals(),
            vec![0, 0, 0, 0, 0],
            "the two records ABOVE the refusal are the cost: this build understands them \
             perfectly and is never offered them"
        );
    }

    #[test]
    fn a_projection_never_hands_a_sink_an_orphaned_attempts_event() {
        // The watermark rule, from the emitting side rather than the replay side:
        // seq 2 was appended by a worker that then failed to commit, so its event
        // must never be emitted at all — not emitted and then retracted.
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Observe, 0),
        ])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 1, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(sink.emitted.len(), 1);
        assert_eq!(sink.emitted[0].phase, Phase::Prepare);
        assert_eq!(projector.emitted_through_seq(), 1);
    }

    #[test]
    fn a_projection_carries_the_type_and_the_payload_each_record_holds() {
        // The sink receives what was journaled, not a re-derivation of it. A
        // projector that handed over the address alone would make every event
        // indistinguishable at the transport.
        //
        // TWO records with DIFFERENT types and DIFFERENT payloads, because the
        // earlier version of this test used one record built by a fixture that
        // hardcodes `"iteration_started"` — so it compared a constant against the
        // same constant and stayed green for a `project` that hardcoded the
        // string, or that passed `Value::Null` for every payload.
        //
        // What production change leaves this green? Only one that reads
        // `event_type` and `payload` off the record it is projecting. Hardcoding
        // either turns the second element red; stripping the payload turns both.
        let journal = Journal::from_records(vec![
            event_of(
                1,
                3,
                Phase::Epilogue,
                0,
                "iteration_completed",
                serde_json::json!({ "no_action_streak": 2 }),
                RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
            ),
            event_of(
                2,
                3,
                Phase::Epilogue,
                1,
                "step_stuck_warning",
                serde_json::json!({ "tool": "browser__screenshot", "repeats": 4 }),
                RecordedEventRouting::TransportOnly,
            ),
        ])
        .expect("well-formed");

        let mut sink = RecordingSink::default();
        ProjectorCursor::new().project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(
            sink.types,
            vec![
                "iteration_completed".to_string(),
                "step_stuck_warning".to_string()
            ]
        );
        assert_eq!(
            sink.payloads,
            vec![
                serde_json::json!({ "no_action_streak": 2 }),
                serde_json::json!({ "tool": "browser__screenshot", "repeats": 4 }),
            ],
            "the payload is most of what an event is; a projector that handed the transport an \
             address and an empty body would satisfy every other test in this file"
        );

        // The third thing a record holds, and the one a sink cannot rebuild.
        //
        // TWO routings, and they are different variants, so this cannot pass a
        // `project` that hands every sink one constant — which is exactly what a
        // walk calling `emit` instead of `emit_routed` would look like from
        // here, since the default `emit_routed` discards its argument.
        //
        // The scope is asserted whole rather than field by field: a rebuild that
        // transposed `workspace` and `task_id` is the mistake four same-typed
        // strings invite, and `a_scope` makes all four distinct so a transposition
        // shows up.
        assert_eq!(
            sink.routings,
            vec![
                RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
                RecordedEventRouting::TransportOnly,
            ],
            "the routing is the half of the record a sink cannot re-derive; a projector that \
             dropped it would keep every live surface fed and silently stop the persisted \
             runtime-fact stream"
        );
    }

    #[test]
    fn a_record_that_is_never_an_event_still_moves_the_mark() {
        // Phase completions and owner transitions outnumber events by five to
        // one. A mark that stopped below them would rescan the whole tail of the
        // journal on every projection for the rest of the run.
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 1),
            record(2, 1, Phase::Prepare, RecordedStep::Continued),
        ])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        projector.project(&journal, "exec-1", 2, &mut sink);
        assert_eq!(projector.emitted_through_seq(), 2);
    }

    #[test]
    fn a_mark_beyond_the_watermark_is_reported_rather_than_read_as_nothing_to_do() {
        // A cursor restored against a journal that no longer reaches it. Emitting
        // nothing is correct; being indistinguishable from a quiet run is not,
        // because the two need different answers from whoever is watching.
        let mut projector: ProjectorCursor =
            serde_json::from_str(r#"{"emitted_through_seq":9,"recent_keys":[],"window":8}"#)
                .expect("a persisted cursor must load");
        let journal =
            Journal::from_records(vec![event(1, 1, Phase::Prepare, 0)]).expect("well-formed");

        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 1, &mut sink);

        assert!(projection.mark_beyond_watermark);
        assert_eq!(projection.emitted, 0);
        assert!(sink.emitted.is_empty());
    }

    #[test]
    fn a_persisted_cursor_still_recognises_a_re_run_after_a_restart() {
        // The half of the dedupe that a bare high-water mark cannot do. The
        // original is at seq 1 and the crashed phase's re-run appends the same
        // address at seq 2 — ABOVE the mark — so only the remembered address can
        // recognise it. A cursor that persisted the mark and dropped the window
        // round-trips perfectly and emits the duplicate.
        let execution_id = "exec-1";
        let original = event(1, 1, Phase::Prepare, 0);
        let before_crash = Journal::from_records(vec![original]).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        projector.project(&before_crash, execution_id, 1, &mut sink);
        assert_eq!(sink.emitted.len(), 1);

        let persisted = serde_json::to_string(&projector).expect("a cursor must serialize");
        let mut restored: ProjectorCursor =
            serde_json::from_str(&persisted).expect("a cursor must load");

        let after_restart = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 0),
        ])
        .expect("well-formed");
        let mut sink = RecordingSink::default();
        let projection = restored.project(&after_restart, execution_id, 2, &mut sink);

        assert!(
            sink.emitted.is_empty(),
            "the re-run's duplicate must be recognised across the restart"
        );
        assert_eq!(projection.deduped, 1);
    }

    #[test]
    fn a_persisted_window_of_zero_is_clamped_on_load_and_not_only_on_construction() {
        // `with_window` clamps a zero. A deserializer that walked around it would
        // restore a cursor that evicts every address the moment it is remembered
        // — dedupe off, every re-run's duplicate emitted — which is the failure
        // the constructor bound exists to prevent, arriving through the other
        // door. Asserted through BEHAVIOUR rather than by reading the field, so a
        // clamped field that nothing consults would not satisfy it.
        let mut projector: ProjectorCursor =
            serde_json::from_str(r#"{"emitted_through_seq":0,"recent_keys":[],"window":0}"#)
                .expect("a persisted cursor must load");
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 0),
        ])
        .expect("well-formed");

        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 2, &mut sink);
        assert_eq!(
            sink.emitted.len(),
            1,
            "a restored cursor must still recognise a re-run's duplicate"
        );
        assert_eq!(projection.deduped, 1);
    }

    /// The zero-window resolution argued for at length in
    /// `From<ProjectorCursorWire>` — a zero becomes `DEFAULT_WINDOW`, never `1`
    /// — and nothing checked it.
    ///
    /// The clamp test above uses two records at ONE address, and a window of one
    /// passes it: emit the first, remember its key, dedupe the second. So the
    /// comment's whole reason for the choice ("resolving to one is a silently
    /// broken dedupe rather than an obviously default one") was the one property
    /// with no test behind it.
    ///
    /// Three records at two addresses is the smallest shape that tells them
    /// apart: A, then B, then A again. A window of one evicts A when B lands and
    /// emits the duplicate; anything ≥ 2 recognises it.
    ///
    /// **What production change leaves this green?** Only a resolution of two or
    /// more. Changing the zero case to `1` — the alternative the comment argues
    /// against — turns `deduped` to 0 and `emitted` to 3.
    #[test]
    fn a_persisted_window_of_zero_restores_the_default_and_not_a_window_of_one() {
        let mut projector: ProjectorCursor =
            serde_json::from_str(r#"{"emitted_through_seq":0,"recent_keys":[],"window":0}"#)
                .expect("a persisted cursor must load");
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 1),
            // The crashed phase re-runs and re-appends its FIRST event.
            event(3, 1, Phase::Prepare, 0),
        ])
        .expect("well-formed");

        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 3, &mut sink);

        assert_eq!(
            sink.ordinals(),
            vec![0, 1],
            "a window of one would have evicted ordinal 0 when ordinal 1 landed and emitted the \
             re-run's duplicate a third time"
        );
        assert_eq!(projection.emitted, 2);
        assert_eq!(projection.deduped, 1);
    }

    /// An ABSENT `window` field, which is the shape an older persisted cursor
    /// actually has — the field is `#[serde(default)]`, so it reads as `0` and
    /// takes the same door as a written zero. Nothing covered the absent case.
    #[test]
    fn a_persisted_cursor_with_no_window_field_at_all_still_dedupes() {
        let mut projector: ProjectorCursor =
            serde_json::from_str(r#"{"emitted_through_seq":0,"recent_keys":[]}"#)
                .expect("a cursor written before the field existed must still load");
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 1),
            event(3, 1, Phase::Prepare, 0),
        ])
        .expect("well-formed");

        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 3, &mut sink);
        assert_eq!(sink.ordinals(), vec![0, 1]);
        assert_eq!(projection.deduped, 1);
    }

    /// `project` reaches `mark_emitted` only for an absent address, so a live window of
    /// N holds N DISTINCT addresses. A file is under no such obligation, and
    /// nothing re-established the invariant on the way in.
    ///
    /// A cursor whose `recent_keys` is the same address repeated fills its whole
    /// window with one entry and restores with an effective dedupe distance of
    /// ONE — every re-run duplicate beyond the immediately preceding record
    /// emitted twice, which is the failure the type exists to prevent arriving
    /// through the door the wire type was added to close.
    ///
    /// **What production change leaves this green?** Only one that de-duplicates
    /// `recent_keys` on load. Delete the `seen` filter in
    /// `From<ProjectorCursorWire>` and the five slots trim to
    /// `[1, 9, 9, 9]` — three of the four remembering the same address, ordinal
    /// 0 forgotten, and its record emitted a second time.
    ///
    /// Doing the dedupe BEFORE the trim is also load-bearing, and the fixture is
    /// built to catch the other order: trimming first discards ordinal 0 while
    /// the duplicate 9s are still occupying slots, so a de-duplication that ran
    /// afterwards would tidy a window that had already lost the address.
    #[test]
    fn a_persisted_window_full_of_one_repeated_address_still_remembers_every_distinct_one() {
        let repeated = |ordinal: u32| {
            serde_json::json!({
                "execution_id": "exec-1",
                "iteration": 1,
                "phase": "prepare",
                "ordinal": ordinal,
            })
        };
        let wire = serde_json::json!({
            "emitted_through_seq": 0,
            "window": 4,
            // Four addresses' worth of slots, three distinct addresses, and the
            // one that repeats is the NEWEST — so a trim that ran before the
            // dedupe would keep three copies of it and forget ordinal 0.
            "recent_keys": [
                repeated(0),
                repeated(1),
                repeated(9),
                repeated(9),
                repeated(9),
            ],
        });
        let mut projector: ProjectorCursor =
            serde_json::from_value(wire).expect("a persisted cursor must load");

        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Prepare, 1),
            event(3, 1, Phase::Prepare, 9),
        ])
        .expect("well-formed");

        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 3, &mut sink);

        assert!(
            sink.emitted.is_empty(),
            "all three addresses were already emitted; the repeated one must not have crowded \
             the oldest out of a window with room for four distinct addresses. Emitted: {:?}",
            sink.ordinals()
        );
        assert_eq!(projection.deduped, 3);
    }

    /// The allocation, not the scan. `recent_keys` used to be a plain
    /// `Vec<EventKey>` field, so the file decided how much was allocated and the
    /// trim in `From<ProjectorCursorWire>` discarded the excess AFTER the fact —
    /// a bound applied one step too late to be a bound.
    ///
    /// Asserted through the window's CONTENTS rather than through a memory
    /// measurement, because what the bounded reader must also get right is
    /// *which* end it keeps: a reader that capped at `MAX_WINDOW` by taking the
    /// first N would keep the oldest addresses, which is the opposite of what
    /// dedupe is about to be asked for.
    #[test]
    fn a_persisted_window_longer_than_the_ceiling_keeps_the_newest_and_never_holds_the_rest() {
        let over = ProjectorCursor::MAX_WINDOW + 2_048;
        let wire = serde_json::json!({
            "emitted_through_seq": 0,
            "window": 4,
            "recent_keys": (0..over as u32)
                .map(|ordinal| serde_json::json!({
                    "execution_id": "exec-1",
                    "iteration": 1,
                    "phase": "prepare",
                    "ordinal": ordinal,
                }))
                .collect::<Vec<_>>(),
        });
        let projector: ProjectorCursor =
            serde_json::from_value(wire).expect("a persisted cursor must load");

        // One assertion, not a length check followed by a contents check: the
        // contents already fix the length, and an assertion an earlier one has
        // made unfailable is noise that reads as coverage.
        assert_eq!(
            projector
                .recent_keys
                .iter()
                .map(|key| key.ordinal)
                .collect::<Vec<_>>(),
            vec![
                over as u32 - 4,
                over as u32 - 3,
                over as u32 - 2,
                over as u32 - 1
            ],
            "the window is what survives whatever the file carried, and it must be the NEWEST \
             four. A bounded read that kept the first N would keep the oldest addresses and \
             drop exactly the ones a re-run is about to re-address"
        );
    }

    #[test]
    fn a_persisted_window_keeps_the_newest_addresses_when_it_is_trimmed() {
        // A file may carry more addresses than its window allows — a cursor saved
        // by a build with a larger window, or an edited one. Trimming from the
        // end would keep the OLDEST addresses and drop exactly the ones a re-run
        // is about to re-address: dedupe that passes a round-trip test and fails
        // at the only moment it is asked anything.
        //
        // Five addresses, a window of four, and the journal asks about exactly
        // two of them — the one the trim must forget and the one it must keep.
        // Asking about all five would not discriminate: emitting a forgotten
        // address pushes it into the window and evicts a remembered one, so by
        // the end of a five-record walk every address has been emitted whichever
        // end the trim took.
        let wire = serde_json::json!({
            "emitted_through_seq": 0,
            "window": 4,
            "recent_keys": (1..=5u32)
                .map(|ordinal| serde_json::json!({
                    "execution_id": "exec-1",
                    "iteration": 1,
                    "phase": "prepare",
                    "ordinal": ordinal,
                }))
                .collect::<Vec<_>>(),
        });
        let mut projector: ProjectorCursor =
            serde_json::from_value(wire).expect("a persisted cursor must load");

        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 1),
            event(2, 1, Phase::Prepare, 5),
        ])
        .expect("well-formed");
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(
            sink.ordinals(),
            vec![0],
            "the oldest address is the one the trim drops, so it re-emits; the newest is kept \
             and deduped. Stable projector ordinals count same-body occurrences, so this first \
             emitted occurrence is zero. Trimming the other end reverses both."
        );
        assert_eq!(projection.deduped, 1);
    }

    #[test]
    fn a_persisted_window_over_the_ceiling_is_capped() {
        // The dedupe check is a linear scan per record, so an untrusted window of
        // ten million turns a projection into a stall rather than into an
        // over-wide memory.
        let projector: ProjectorCursor =
            serde_json::from_str(r#"{"emitted_through_seq":0,"recent_keys":[],"window":10000000}"#)
                .expect("a persisted cursor must load");
        assert_eq!(projector.window, ProjectorCursor::MAX_WINDOW);
    }

    #[test]
    fn a_projector_never_sees_an_orphaned_attempts_events() {
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            event(2, 1, Phase::Observe, 0),
        ])
        .expect("well-formed");
        let projector = ProjectorCursor::new();

        // Only the first record is committed.
        let pending = projector.pending(&journal, "exec-1", 1);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 1);
    }

    #[test]
    fn a_window_of_zero_is_clamped_rather_than_silently_disabling_dedupe() {
        // A window of zero evicts every address as soon as it is remembered,
        // which leaves a projector that emits every re-run's duplicate — the one
        // failure this type exists to prevent, arriving through its own
        // constructor.
        let mut projector = ProjectorCursor::with_window(0);
        let first = event(1, 1, Phase::Prepare, 0);
        let after_crash = event(2, 1, Phase::Prepare, 0);
        let journal = Journal::from_records(vec![first.clone(), after_crash]).expect("well-formed");

        let _ = projector.mark_emitted(first.seq, first.event_key("exec-1"));
        assert!(
            projector.pending(&journal, "exec-1", 2).is_empty(),
            "the re-run's record carries the same address and must still be recognised"
        );
    }

    #[test]
    fn the_dedupe_window_does_not_grow_without_bound() {
        let mut projector = ProjectorCursor::with_window(4);
        for seq in 1..=10u64 {
            let record = event(seq, 1, Phase::Prepare, seq as u32);
            let _ = projector.mark_emitted(record.seq, record.event_key("exec-1"));
        }
        assert_eq!(projector.recent_keys.len(), 4);
        assert_eq!(projector.emitted_through_seq(), 10);
    }

    #[test]
    fn an_oversized_record_is_refused_at_encode_rather_than_at_read() {
        let mut record = event(1, 1, Phase::Prepare, 0);
        record.body = JournalBody::Event {
            event_type: "oversized".to_string(),
            payload: serde_json::json!({ "blob": "x".repeat(MAX_JOURNAL_RECORD_BYTES) }),
            routing: RecordedEventRouting::Unrecorded,
        };
        let error = record
            .to_line()
            .expect_err("a record no bounded read could take back must not be written");
        assert!(
            matches!(error, JournalError::RecordTooLarge { seq: 1, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn an_oversized_event_is_refused_at_the_producer_rather_than_at_the_commit() {
        // Where the refusal lands is the whole point. An append that refuses one
        // record fails the whole batch, which fails the commit, which refuses the
        // boundary — a live run stalled by the size of something emitted for
        // observability. The producer is the only party that can answer it (drop
        // it, truncate it, or emit a reference to it), so the refusal is put
        // where that decision can be taken.
        let error = JournalAppend::event(
            1,
            Phase::Prepare,
            "oversized",
            serde_json::json!({ "blob": "x".repeat(MAX_JOURNAL_RECORD_BYTES) }),
            RecordedEventRouting::Unrecorded,
        )
        .expect_err("a body no bounded read could take back must not reach a batch");
        assert!(
            // Seq ZERO, not the probe's `u64::MAX`: the store has assigned
            // nothing yet, and an operator sent looking for line 18446744073709551615
            // of a file with none has been told something false.
            matches!(error, JournalError::RecordTooLarge { seq: 0, .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_body_the_producer_accepted_fits_whatever_the_store_stamps_on_it() {
        // The producer measures a record NOBODY has stamped yet: `seq` and
        // `at_ms` are the store's, and `iteration`, `phase` and `ordinal` are the
        // driver's — `commit_boundary` discards the caller's and writes its own.
        // So the probe stamps the widest value of all five. A probe that copied
        // any of them accepts a body a few bytes larger, and that body then fails
        // at the store, on a real record, at a commit boundary, which is the
        // failure the constructor exists to prevent arriving through the check
        // meant to prevent it.
        //
        // So: find the largest payload this constructor accepts, then check that
        // the record the DRIVER AND STORE would write still encodes at the
        // extremes of every field neither the producer nor this test controls.
        //
        // The caller-side arguments below are deliberately NARROW — iteration 0
        // and the shortest phase name — because that is what a producer passes
        // when the doc tells it the values are re-stamped anyway. Under a probe
        // that copied them, the loop settles on a larger payload and the
        // `usize::MAX` / `Phase::Epilogue` row below then fails to encode. That
        // was the live gap: the old version of this test pinned
        // `iteration: append.iteration, phase: append.phase`, so it varied the
        // three widened fields and held constant exactly the two that were not.
        const NARROW_ITERATION: usize = 0;
        const NARROW_PHASE: Phase = Phase::Apply;

        let mut payload_len = MAX_JOURNAL_RECORD_BYTES;
        let append = loop {
            match JournalAppend::event(
                NARROW_ITERATION,
                NARROW_PHASE,
                "step_completed",
                serde_json::json!({ "blob": "x".repeat(payload_len) }),
                // The WIDEST routing, because the probe measures the body and
                // the routing is part of it. Measuring the boundary with
                // `Unrecorded` would find a larger payload than a real
                // canonical-fact producer can write, and every row below would
                // then be asserting about a body no producer of that kind
                // builds.
                RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
            ) {
                Ok(append) => break append,
                Err(_) => {
                    payload_len = payload_len
                        .checked_sub(1)
                        .expect("some payload is small enough to be accepted");
                },
            }
        };
        assert!(
            JournalAppend::event(
                NARROW_ITERATION,
                NARROW_PHASE,
                "step_completed",
                serde_json::json!({ "blob": "x".repeat(payload_len + 1) }),
                // The SAME routing as the accepting call above. A wider one here
                // would make this pass for the extra bytes rather than for the
                // extra payload byte, which is not what the assertion claims.
                RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
            )
            .is_err(),
            "one byte more must be refused, or the loop stopped short of the boundary and \
             everything below is about a comfortably small record"
        );

        for (seq, at_ms, ordinal, iteration, phase) in [
            (1u64, TEST_NOW_MS, 0u32, NARROW_ITERATION, NARROW_PHASE),
            // Every field at its widest at once — the record no store will
            // actually write, and the one every real record is bounded by.
            (u64::MAX, i64::MIN, u32::MAX, usize::MAX, Phase::Epilogue),
            (u64::MAX, i64::MAX, 3, usize::MAX, Phase::Epilogue),
            // The plausible re-stamp: a long-running iteration that happens to
            // end in the phase with the longest name. This is the row a narrow
            // probe fails on in production rather than in a contrived extreme.
            (412, TEST_NOW_MS, 2, 250, Phase::Epilogue),
        ] {
            let record = JournalRecord {
                seq,
                iteration,
                phase,
                ordinal,
                at_ms,
                body: append.body.clone(),
            };
            let _line = record.to_line().unwrap_or_else(|error| {
                panic!(
                    "a body the producer accepted must still encode once the driver and store \
                     stamp it (seq {seq}, at_ms {at_ms}, ordinal {ordinal}, iteration \
                     {iteration}, phase {phase}): {error}"
                )
            });
        }
    }

    /// The probe stamps one phase for every caller, so that phase has to be the
    /// widest one — otherwise a body sized against a short name fails once the
    /// driver stamps a longer one, which is the narrow-probe failure with a
    /// different field in it.
    ///
    /// A test rather than a comment because the constant cannot check itself, and
    /// because adding a phase is the change to expect here: a `Phase::Reconcile`
    /// would be nine characters and would turn this red, which is the moment to
    /// move `WIDEST_PHASE`.
    #[test]
    fn the_size_probe_stamps_the_widest_phase_name() {
        let widest = serde_json::to_string(&WIDEST_PHASE).expect("a phase must serialize");
        for phase in Phase::ORDER {
            let encoded = serde_json::to_string(&phase).expect("a phase must serialize");
            assert!(
                encoded.len() <= widest.len(),
                "{encoded} encodes wider than the probe's {widest}, so a body accepted by \
                 `JournalAppend::event` could still fail at the store once the driver stamped \
                 {phase}. Move WIDEST_PHASE."
            );
        }
    }

    /// The narrow-probe failure, asserted directly rather than only implied by
    /// the test above.
    ///
    /// A producer passes the placeholders the docs invite — iteration 0, a short
    /// phase — and the driver re-stamps its own. If the probe measured the
    /// caller's values, there would be a payload size this constructor accepts
    /// and the store then refuses; the two `expect`s below are what says there is
    /// not.
    ///
    /// **What production change leaves this green?** Only a probe that widens
    /// `iteration` and `phase`. Restoring `iteration, phase` in the probe makes
    /// the second `expect` fail on the re-stamped record.
    #[test]
    fn a_placeholder_iteration_and_phase_do_not_buy_the_producer_extra_bytes() {
        let mut payload_len = MAX_JOURNAL_RECORD_BYTES;
        let append = loop {
            match JournalAppend::event(
                0,
                Phase::Apply,
                "step_completed",
                serde_json::json!({ "blob": "x".repeat(payload_len) }),
                RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
            ) {
                Ok(append) => break append,
                Err(_) => {
                    payload_len = payload_len
                        .checked_sub(1)
                        .expect("some payload is small enough to be accepted");
                },
            }
        };

        // What `commit_boundary` actually writes: its own iteration and phase,
        // not the ones handed to the constructor.
        let re_stamped = JournalRecord {
            seq: 9_001,
            iteration: 250,
            phase: Phase::Epilogue,
            ordinal: 1,
            at_ms: TEST_NOW_MS,
            body: append.body,
        };
        re_stamped
            .to_line()
            .expect("the driver's re-stamp must not push an accepted body over the ceiling");
    }

    #[test]
    fn an_event_append_carries_no_ordinal_of_its_own() {
        // The driver re-stamps `iteration`, `phase` and `ordinal` on every record
        // a phase reports, so an ordinal set here would be overwritten and a
        // producer reading it back would be reading a number that never reached
        // the log. The payload must also survive the size probe intact.
        let append = JournalAppend::event(
            2,
            Phase::Observe,
            "iteration_started",
            serde_json::json!({ "iteration": 2 }),
            RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
        )
        .expect("a small event must be accepted");
        assert_eq!(append.ordinal, 0);
        assert_eq!(append.iteration, 2);
        assert_eq!(append.phase, Phase::Observe);
        match append.body {
            JournalBody::Event {
                event_type,
                payload,
                routing,
            } => {
                assert_eq!(event_type, "iteration_started");
                assert_eq!(payload["iteration"], 2);
                // The constructor must carry the caller's routing through
                // unchanged. It has to be asserted here because the probe
                // rebuilds the body into a `JournalRecord` and hands
                // `probe.body` back — a rebuild that dropped the field, or
                // replaced it with the default, would be invisible to every
                // size assertion in this file.
                assert_eq!(
                    routing,
                    RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() }
                );
            },
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn two_records_in_one_append_may_not_claim_one_address() {
        let appends = vec![
            JournalAppend {
                iteration: 1,
                phase: Phase::Apply,
                ordinal: 0,
                body: JournalBody::Event {
                    event_type: "a".to_string(),
                    payload: serde_json::Value::Null,
                    routing: RecordedEventRouting::Unrecorded,
                },
            },
            JournalAppend {
                iteration: 1,
                phase: Phase::Apply,
                ordinal: 0,
                body: JournalBody::Event {
                    event_type: "b".to_string(),
                    payload: serde_json::Value::Null,
                    routing: RecordedEventRouting::Unrecorded,
                },
            },
        ];
        let error = Journal::check_batch_addresses("exec-1", &appends)
            .expect_err("one of these events would be silently dropped at projection");
        assert!(
            matches!(error, JournalError::DuplicateEventKey { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn an_absent_owner_authorization_stays_absent_through_a_round_trip() {
        // `None` here means NO authorization was recorded. A round trip that
        // invented an empty string would make a reader checking `is_some()`
        // treat an unauthorized transition as authorized.
        let record = JournalRecord {
            seq: 1,
            iteration: 2,
            phase: Phase::Prepare,
            ordinal: 0,
            at_ms: 0,
            body: JournalBody::OwnerTransition {
                from_agent_id: Some("agent-a".to_string()),
                to_agent_id: "agent-b".to_string(),
                transition_authorization: None,
            },
        };
        let line = record.to_line().expect("encode");
        assert!(
            !line.contains("transition_authorization"),
            "an absent authorization must not be written at all: {line}"
        );
        let decoded: JournalRecord = serde_json::from_str(&line).expect("decode");
        assert_eq!(decoded, record);
    }

    // ========================================================================
    // The routing a record remembers
    // ========================================================================

    fn an_event_record(routing: RecordedEventRouting) -> JournalRecord {
        JournalRecord {
            seq: 1,
            iteration: 2,
            phase: Phase::Prepare,
            ordinal: 0,
            at_ms: TEST_NOW_MS,
            body: JournalBody::Event {
                event_type: "AgenticIterationStarted".to_string(),
                payload: serde_json::json!({ "iteration": 2 }),
                routing,
            },
        }
    }

    #[test]
    fn every_routing_survives_the_line_a_store_writes() {
        // Through `to_line` and the parser `Journal::from_records`' caller uses,
        // not through a bare `serde_json` round trip on the enum: the durable
        // path is what a foreign process reads, and an attribute that broke only
        // there would be invisible to a test that encoded the enum on its own.
        for routing in [
            RecordedEventRouting::Unrecorded,
            RecordedEventRouting::TransportOnly,
            RecordedEventRouting::CanonicalRuntimeFact { scope: a_scope() },
        ] {
            let record = an_event_record(routing.clone());
            let line = record.to_line().expect("encode");
            let decoded: JournalRecord = serde_json::from_str(&line).expect("decode");
            assert_eq!(
                decoded, record,
                "a routing that did not survive the line is a decision the projector cannot \
                 read back: {line}"
            );
        }
    }

    #[test]
    fn a_record_written_before_the_routing_existed_reads_back_as_unrecorded() {
        // THE COMPATIBILITY CLAIM, checked rather than asserted. Journals written
        // by an earlier build carry no `routing` key at all, and they are read on
        // every boundary of every resumed run. Without `#[serde(default)]` those
        // runs stop parsing their own logs — which surfaces as a quarantine at
        // the next phase entry, not as a missing event, so nothing about the
        // failure would point here.
        //
        // The old line is made by DELETING the key from a current encoding rather
        // than by hand, so the rest of the record cannot drift out of shape and
        // leave this passing for the wrong reason. What is hand-controlled is the
        // one thing under test — that the key is gone — and the assert below says
        // so, because a `remove` that found nothing would otherwise leave this
        // testing a record that still had its routing.
        let mut encoded: serde_json::Value = serde_json::from_str(
            &an_event_record(RecordedEventRouting::TransportOnly)
                .to_line()
                .expect("encode"),
        )
        .expect("a written line is JSON");
        let removed = encoded
            .get_mut("body")
            .and_then(serde_json::Value::as_object_mut)
            .expect("the body is an object")
            .remove("routing");
        assert!(
            removed.is_some(),
            "the fixture must actually have carried a routing to delete, or this case is about \
             a record shape that never existed: {encoded}"
        );

        let decoded: JournalRecord = serde_json::from_value(encoded).expect(
            "a record written before this field existed must still parse, or every resumed run \
             on an older journal is quarantined",
        );
        match decoded.body {
            JournalBody::Event {
                event_type,
                routing,
                ..
            } => {
                assert_eq!(event_type, "AgenticIterationStarted");
                assert_eq!(
                    routing,
                    RecordedEventRouting::Unrecorded,
                    "an absent decision must read back as the ABSENCE of one; reading it as \
                     `TransportOnly` would silently downgrade every fact in every journal \
                     written before this field"
                );
            },
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unrecorded_and_transport_only_are_different_values_on_the_wire() {
        // They mean different things — no decision versus a decision to send
        // transport-only — and a reader can only tell them apart if the encoding
        // does. An enum that wrote both the same way would make the distinction
        // undetectable, and every claim in `RecordedEventRouting`'s docs about
        // falling back loudly would be describing something a reader cannot see.
        let unrecorded = serde_json::to_value(RecordedEventRouting::Unrecorded).expect("encode");
        let transport_only =
            serde_json::to_value(RecordedEventRouting::TransportOnly).expect("encode");
        assert_ne!(
            unrecorded, transport_only,
            "{unrecorded} vs {transport_only}"
        );
    }

    // ========================================================================
    // The retry-delay ceiling
    // ========================================================================

    fn retry_record(seq: u64, after_ms: u64) -> JournalRecord {
        JournalRecord {
            seq,
            iteration: 1,
            phase: Phase::Resolve,
            ordinal: 0,
            at_ms: 1_700_000_000_000,
            body: JournalBody::PhaseCompleted {
                step: RecordedStep::Exited {
                    boundary: RecordedBoundary::Retry { after_ms },
                },
            },
        }
    }

    #[test]
    fn a_retry_delay_no_writer_could_produce_is_refused_where_it_enters() {
        // The defect: `after_ms` had no read ceiling. "It fails toward waiting,
        // never toward re-firing" is right about the direction and wrong about
        // the consequence — under a worker driver this value is read off disk
        // into `runnable_at_ms`, and a run parked for a thousand years looks
        // exactly like a run that is still working.
        //
        // Refused at PARSE, so the operator is pointed at the line rather than
        // at a run three phases later that never woke.
        let line = serde_json::to_string(&retry_record(1, MAX_RETRY_AFTER_MS + 1))
            .expect("a hand-built record encodes");
        let error = Journal::parse(&format!("{line}\n"))
            .expect_err("a delay over the ceiling must refuse the parse");
        assert!(
            matches!(error, JournalError::RetryDelayTooLarge { seq: 1, .. }),
            "got {error:?}"
        );

        // The other door into a `Journal` applies the same bound. A store that
        // held records in memory and one that held them in a file must not
        // disagree about what is loadable.
        let error = Journal::from_records(vec![retry_record(1, MAX_RETRY_AFTER_MS + 1)])
            .expect_err("the in-memory door must refuse it too");
        assert!(
            matches!(error, JournalError::RetryDelayTooLarge { seq: 1, .. }),
            "got {error:?}"
        );

        // And the boundary value itself loads, or the ceiling would be off by
        // one and every assertion above would pass for the wrong reason. Both
        // doors at the edge, not just the one: a bound tested at MAX+1 on both
        // and at MAX on one cannot tell `<` from `<=` in the door it skipped.
        let line = serde_json::to_string(&retry_record(1, MAX_RETRY_AFTER_MS)).expect("encode");
        Journal::parse(&format!("{line}\n")).expect("the ceiling itself is loadable");
        Journal::from_records(vec![retry_record(1, MAX_RETRY_AFTER_MS)])
            .expect("and loadable through the in-memory door too");
        retry_record(1, MAX_RETRY_AFTER_MS)
            .to_line()
            .expect("and writable, or the write side could not produce its own clamp");
    }

    #[test]
    fn the_write_side_clamps_so_it_can_never_produce_what_the_read_side_refuses() {
        // The two sides of one bound, and they are deliberately not the same
        // action. The conversion narrows — it already saturates a duration too
        // large for u64 milliseconds — so clamping is the same act with a bound
        // a reader will accept. Refusing there would fail a live run for a
        // condition the runtime can correctly narrow.
        let absurd = RecordedBoundary::from(BoundaryOutcome::Retry(Duration::from_secs(
            60 * 60 * 24 * 365,
        )));
        assert_eq!(
            absurd,
            RecordedBoundary::Retry {
                after_ms: MAX_RETRY_AFTER_MS
            },
            "a year-long backoff must clamp, not saturate to u64::MAX"
        );

        // Which is the whole point: whatever the write side produces, the read
        // side takes.
        let line = serde_json::to_string(&JournalRecord {
            seq: 1,
            iteration: 1,
            phase: Phase::Resolve,
            ordinal: 0,
            at_ms: 0,
            body: JournalBody::PhaseCompleted {
                step: RecordedStep::Exited { boundary: absurd },
            },
        })
        .expect("encode");
        Journal::parse(&format!("{line}\n")).expect("a clamped record always loads");

        // And an ordinary backoff is untouched. Without this the clamp could be
        // `after_ms = 0` and every assertion above would still pass.
        assert_eq!(
            RecordedBoundary::from(BoundaryOutcome::Retry(Duration::from_millis(1_500))),
            RecordedBoundary::Retry { after_ms: 1_500 }
        );

        // The write door refuses a hand-built record over the ceiling rather
        // than emitting a line the reader would then reject.
        let error = retry_record(1, MAX_RETRY_AFTER_MS + 1)
            .to_line()
            .expect_err("a bound checked on only one side is a bound that fails when needed");
        assert!(
            matches!(error, JournalError::RetryDelayTooLarge { .. }),
            "got {error:?}"
        );
    }

    // ========================================================================
    // Owner transitions
    // ========================================================================

    fn portable_run() -> LoopState {
        LoopState::new(super::super::state::RunIdentity {
            agent_id: Some("parent".to_string()),
            browser_transports: vec!["cdp".to_string()],
            ..super::super::state::RunIdentity::default()
        })
    }

    #[test]
    fn a_handover_cannot_widen_a_portable_runs_browser_ceiling() {
        // The case no start-time check catches, and the reason Decision 2 is an
        // invariant rather than a gate. A run starts portable under a `[cdp]`
        // ceiling and hands over to an agent that declares nothing — which parses
        // as `allowed: None` and permits every transport. Placement unchanged and
        // correct, ceiling unchanged and correct, combination wrong: a portable
        // execution now permitted to launch a local Chrome on whichever worker
        // picks it up.
        let mut state = portable_run();
        apply_owner_transition(&mut state, "unrestricted-delegate", &[])
            .expect("the handover is narrowed, not refused");

        assert_eq!(
            state.identity.browser_transports,
            vec!["cdp".to_string()],
            "an unrestricted incoming ceiling must be intersected with [cdp], \
             not installed"
        );
        assert_eq!(
            state.identity.agent_id.as_deref(),
            Some("unrestricted-delegate")
        );

        // A ceiling that names headed explicitly is the same widening spelled
        // out, and narrows the same way.
        let mut state = portable_run();
        apply_owner_transition(
            &mut state,
            "headed-delegate",
            &["cdp".to_string(), "headed".to_string()],
        )
        .expect("narrowed");
        assert_eq!(state.identity.browser_transports, vec!["cdp".to_string()]);

        // And the narrowed state is one the commit projection will accept — the
        // two halves of the invariant have to agree, or a handover would produce
        // a state that could never be published.
        let ctx = crate::magician_v2::execution::agentic::types::AgenticContext::new("g", "c");
        state
            .for_commit(super::super::state::CommitPoint {
                ctx: &ctx,
                worker: &super::super::state::WorkerId::new("worker-a"),
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("the intersection satisfies the commit-time invariant");
    }

    #[test]
    fn a_pinned_run_still_takes_its_new_owners_ceiling_whole() {
        // The intersection is for portable runs only. A pinned run holds a
        // worker, so a process-local transport is exactly what it is for, and
        // narrowing it here would demote every delegated browser run on a
        // machine that has a Chrome.
        let mut state = portable_run();
        state.placement = Placement::Pinned {
            worker: super::super::state::WorkerId::new("worker-a"),
            // Live, not lapsed: this case is about a PINNED run taking its new
            // owner's ceiling whole, and a lapsed pin is not a pinned run.
            pinned_until_ms: i64::MAX,
        };
        apply_owner_transition(
            &mut state,
            "headed-delegate",
            &["cdp".to_string(), "headed".to_string()],
        )
        .expect("a pinned run takes the incoming ceiling");
        assert_eq!(
            state.identity.browser_transports,
            vec!["cdp".to_string(), "headed".to_string()],
            "replaced, never merged — and not narrowed either, for a pinned run"
        );
    }

    #[test]
    fn a_handover_that_would_leave_a_portable_run_no_transport_is_refused() {
        // The trap in writing the intersection literally: `[headless] ∩ [cdp]` is
        // empty, and an empty ceiling is not "permits nothing" — it is
        // `BrowserTransportCeiling::parse(&[])`, which permits EVERYTHING. The
        // narrowest possible outcome would have been written as the widest.
        let mut state = portable_run();
        let error = apply_owner_transition(&mut state, "headless-only", &["headless".to_string()])
            .expect_err("an empty intersection cannot be expressed, so it is refused");
        assert!(
            matches!(
                error,
                LoopStateRefusal::HandoverLeavesPortableRunNoTransport { .. }
            ),
            "got {error}"
        );
        assert_eq!(
            state.identity.browser_transports,
            vec!["cdp".to_string()],
            "and the refusal left the ceiling where it was"
        );
        assert_eq!(
            state.identity.agent_id.as_deref(),
            Some("parent"),
            "a refused handover must not have moved the owner either"
        );
    }

    #[test]
    fn a_handover_naming_a_transport_nobody_recognises_is_refused() {
        // `BrowserTransportCeiling::parse` refuses a typo rather than dropping
        // it, because a silently dropped `cdp` is a quiet demotion and a silently
        // dropped entry in the other direction hands out the owner's Chrome.
        // Inheriting that refusal here is what keeps a misspelling from reaching
        // the dispatcher through a handover.
        let mut state = portable_run();
        let error = apply_owner_transition(&mut state, "typo", &["cpd".to_string()])
            .expect_err("an unrecognised transport name refuses the handover");
        assert!(
            matches!(error, LoopStateRefusal::UnreadableCeiling { .. }),
            "got {error}"
        );
    }
    /// The REPLAY half of "a paused run must not read as live" — the mechanism,
    /// not the guard. See the section at the bottom before relying on it.
    ///
    /// `replay_each` clears a resumable terminal as soon as any record follows
    /// it, and that is correct for the case it was written for: a pause ends the
    /// invocation, something outside answers, and the run appends again. What it
    /// cannot see is that the following record belongs to the SAME commit batch,
    /// written by the same phase that just paused.
    ///
    /// `commit_boundary` used to append the completion record FIRST, so a phase
    /// that paused while also recording an owner transition emitted
    /// `[RunEnded{WaitingForConfirmation}, OwnerTransition]` — and replay then
    /// cleared the terminal, `verify_journal` answered `Ok(None)`, and the next
    /// claim re-entered the phase that had just paused. Nothing refused, nothing
    /// was logged. An ordinary owner *collapse* during `Resolve` produces exactly
    /// this batch, so it was a live path and not a hypothetical.
    ///
    /// The driver now writes a run-ending completion record LAST in its batch,
    /// which is also what actually happened: the transition occurred during the
    /// phase, before it ended the run.
    ///
    /// # What production change leaves this green? ALL OF THEM — read this before
    /// budgeting it as a guard
    ///
    /// This test builds both record vectors as literals and calls [`replay`]. It
    /// never reaches `commit_boundary`, so **reverting the driver to
    /// completion-first leaves both assertions green** and nothing here goes red.
    /// What it pins is one step down from the defect: that `replay_each` reads
    /// the two orderings differently, which is *why* the batch order matters and
    /// is not a check that the driver still writes it that way.
    ///
    /// An earlier version of this comment claimed the opposite — "reverting
    /// `commit_boundary` turns the second assertion red" — which is both false
    /// and self-refuting: the second assertion *is* `terminal == None`, so
    /// "replay clears the terminal" is the thing that makes it pass. A reader who
    /// trusted it budgeted a guard that does not exist here and might have
    /// deleted or weakened the one that does.
    ///
    /// **The driver-level guard is
    /// `driver_worker::tests::a_pause_that_also_hands_over_is_still_a_pause_when_replayed`.**
    /// It drives the real `commit_boundary` and reads back the journal it
    /// actually wrote, and it is the one that goes red on a revert. Its own doc
    /// says so and says this test builds its own records; the two comments now
    /// agree.
    #[test]
    fn a_terminal_is_not_cleared_by_a_record_from_its_own_batch() {
        // The batch a pausing phase writes, in the order the driver now writes
        // it: what happened during the phase, then the ending.
        let batch = vec![
            JournalRecord {
                seq: 1,
                iteration: 1,
                phase: Phase::Resolve,
                ordinal: 0,
                at_ms: 1_700_000_000_000,
                body: JournalBody::OwnerTransition {
                    from_agent_id: Some("agent-a".to_string()),
                    to_agent_id: "agent-b".to_string(),
                    transition_authorization: None,
                },
            },
            JournalRecord {
                seq: 2,
                iteration: 1,
                phase: Phase::Resolve,
                ordinal: 1,
                at_ms: 1_700_000_000_001,
                body: JournalBody::PhaseCompleted {
                    step: RecordedStep::RunEnded {
                        terminal: TerminalKind::WaitingForConfirmation,
                    },
                },
            },
        ];

        let cursor = replay(&batch).expect("a pausing batch must replay");
        assert_eq!(
            cursor.terminal,
            Some(TerminalKind::WaitingForConfirmation),
            "the run paused, so the replayed cursor must still say so; a cleared terminal is \
             read as a live run and the next claim re-enters the phase that paused"
        );

        // The same two records in the order the driver used to write them. This
        // is the shape that produced the defect, asserted directly so the
        // hazard is pinned rather than described.
        let completion_first = vec![
            JournalRecord {
                ordinal: 0,
                seq: 1,
                ..batch[1].clone()
            },
            JournalRecord {
                ordinal: 1,
                seq: 2,
                ..batch[0].clone()
            },
        ];
        let cleared = replay(&completion_first).expect("replays, wrongly");
        assert_eq!(
            cleared.terminal, None,
            "this pins WHY the order matters: with the terminal written first, the record \
             behind it clears the pause and the run reads as live"
        );
    }

    // ========================================================================
    // The named rail — `JournalBody::NamedEvent`
    // ========================================================================

    /// The agent id every named fixture carries.
    ///
    /// NOT `"__system__"`, which is what production substitutes when a context
    /// has no agent: a fixture using the fallback would stay green under a walk
    /// that dropped the field and re-derived it.
    const NAMED_AGENT: &str = "agent-named-rail";

    /// A named record, built through the **production constructor**.
    ///
    /// Through `JournalAppend::named_event` rather than a `JournalBody` literal,
    /// so a fixture cannot journal a record the store would refuse and cannot
    /// drift from the ceiling the store applies. `seq` and `at_ms` are the
    /// store's to assign and are supplied here because a placed record needs
    /// them; `iteration`, `phase` and `ordinal` are re-stamped exactly the way
    /// `driver_worker::commit_boundary` re-stamps them.
    ///
    /// The scope is `Some` on both halves and neither value is the field's serde
    /// default. `principal` and `workspace` are `skip_serializing_if =
    /// "Option::is_none"`, so a fixture built on `None` would round-trip through
    /// a reader that dropped both fields and prove nothing about persistence.
    fn named(
        seq: u64,
        iteration: usize,
        phase: Phase,
        ordinal: u32,
        name: &str,
        payload: serde_json::Value,
    ) -> JournalRecord {
        let append = JournalAppend::named_event(
            iteration,
            phase,
            name,
            NAMED_AGENT,
            Some("principal-p".to_string()),
            Some("workspace-w".to_string()),
            payload,
        )
        .expect("a small named event must be accepted");
        JournalRecord {
            seq,
            iteration,
            phase,
            ordinal,
            at_ms: TEST_NOW_MS,
            body: append.body,
        }
    }

    /// The constructor carries all five arguments through the probe unchanged.
    ///
    /// It has to be asserted rather than assumed: the probe rebuilds the body
    /// into a `JournalRecord` and hands `probe.body` back, so a rebuild that
    /// dropped the scope or replaced a field with a default would be invisible
    /// to every size assertion in this file — which is exactly the reason
    /// `an_event_append_carries_no_ordinal_of_its_own` exists for the other
    /// rail.
    ///
    /// **What production change leaves this green?** One that changed the
    /// address stamping, which the three assertions above the body cover, or one
    /// that changed a field this does not read — and there is no such field:
    /// all five are read.
    #[test]
    fn a_named_append_carries_all_five_arguments_and_no_ordinal_of_its_own() {
        let append = JournalAppend::named_event(
            2,
            Phase::Observe,
            "plan.step.started",
            NAMED_AGENT,
            Some("principal-p".to_string()),
            Some("workspace-w".to_string()),
            serde_json::json!({ "step_id": "s-1", "iteration": 2 }),
        )
        .expect("a small named event must be accepted");

        assert_eq!(append.ordinal, 0);
        assert_eq!(append.iteration, 2);
        assert_eq!(append.phase, Phase::Observe);
        match append.body {
            JournalBody::NamedEvent {
                name,
                agent_id,
                principal,
                workspace,
                payload,
            } => {
                assert_eq!(name, "plan.step.started");
                assert_eq!(agent_id, NAMED_AGENT);
                assert_eq!(principal.as_deref(), Some("principal-p"));
                assert_eq!(workspace.as_deref(), Some("workspace-w"));
                assert_eq!(payload["step_id"], "s-1");
            },
            other => panic!("got {other:?}"),
        }
    }

    /// An unscoped call stays unscoped.
    ///
    /// Its own case rather than a variation inside the one above, because the
    /// scope pair is the field a reader is most likely to normalise: with both
    /// halves `None`, `emit_scoped_or_unscoped` builds
    /// `AgentEventEnvelope::new` and not `new_scoped`, so a record that filled
    /// in an empty string for either would replay a **scoped** envelope where
    /// the producer sent an unscoped one — a different wire shape reaching a
    /// different set of subscriptions.
    #[test]
    fn a_named_append_keeps_an_absent_scope_absent() {
        let append = JournalAppend::named_event(
            1,
            Phase::Prepare,
            "plan.step.started",
            NAMED_AGENT,
            None,
            None,
            serde_json::json!({}),
        )
        .expect("a small named event must be accepted");
        match append.body {
            JournalBody::NamedEvent {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal, None);
                assert_eq!(workspace, None);
            },
            other => panic!("got {other:?}"),
        }
    }

    /// A named record moves the cursor no further than an absent one does.
    ///
    /// Its own case rather than an extra element in `one_iteration`, so a change
    /// that made ONE of the two outbox bodies move the cursor fails in the case
    /// named for it rather than inside a fixture about something else.
    ///
    /// **What production change leaves this green?** Deleting the
    /// `JournalBody::NamedEvent` arm from `replay_each` does not: the match is
    /// exhaustive, so that is a compile error. Moving it into the
    /// `PhaseCompleted` arm cannot compile either. What it catches is an arm
    /// that stepped the iteration or the phase — the two things a body added to
    /// this match is most likely to be given by mistake.
    #[test]
    fn a_named_event_moves_no_cursor() {
        let with_named = vec![
            record(1, 1, Phase::Prepare, RecordedStep::Continued),
            named(
                2,
                1,
                Phase::Observe,
                0,
                "plan.step.started",
                serde_json::json!({ "step_id": "s-1" }),
            ),
            record(3, 1, Phase::Observe, RecordedStep::Continued),
        ];
        let cursors = replay_each(&with_named).expect("a named record must replay");

        assert_eq!(
            (cursors[0].iteration, cursors[0].phase),
            (cursors[1].iteration, cursors[1].phase),
            "the named record sits between two completions and must leave the cursor exactly \
             where the first one put it"
        );
        assert_eq!(
            cursors[1].seq, 2,
            "cursor-neutral is about iteration and phase, not about seq: the seq must still \
             advance, or a replay could not say which record it had reached"
        );
        assert_eq!(cursors[1].terminal, None);

        // The same log without the named record, so the claim is a comparison
        // against what the loop would have done rather than against a constant
        // this test chose.
        let without_named = vec![
            record(1, 1, Phase::Prepare, RecordedStep::Continued),
            record(2, 1, Phase::Observe, RecordedStep::Continued),
        ];
        let plain = replay(&without_named).expect("the same log without the outbox entry replays");
        let widened = replay(&with_named).expect("replays");
        assert_eq!(
            (widened.iteration, widened.phase, widened.terminal),
            (plain.iteration, plain.phase, plain.terminal),
            "a log with an outbox entry in it must resume in the same place as one without"
        );
    }

    /// The five arguments reach the sink, and reach it through the named rail.
    ///
    /// **What production change leaves this green?** One that changed the
    /// transport rail, which `types` being empty is what rules out. A walk that
    /// hardcoded the name, dropped the agent id, collapsed the scope or handed
    /// the sink `Value::Null` fails one of the five assertions — which is why
    /// all five are read rather than the name alone.
    #[test]
    fn a_named_events_five_arguments_reach_the_sink() {
        let journal = Journal::from_records(vec![named(
            1,
            1,
            Phase::Prepare,
            0,
            "tool.result.projected",
            serde_json::json!({ "tool_name": "gmail__send", "included_records": 3 }),
        )])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 1, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(projection.deduped, 0);
        assert!(
            sink.types.is_empty(),
            "a named record must not be delivered down the transport-event rail: that rail \
             replays through `emit_transport_only`, which performs no chat fan-out"
        );
        assert_eq!(sink.named.len(), 1);
        let (name, agent_id, principal, workspace, payload) = &sink.named[0];
        assert_eq!(name, "tool.result.projected");
        assert_eq!(agent_id, NAMED_AGENT);
        assert_eq!(principal.as_deref(), Some("principal-p"));
        assert_eq!(workspace.as_deref(), Some("workspace-w"));
        assert_eq!(payload["tool_name"], "gmail__send");
        assert_eq!(payload["included_records"], 3);
    }

    /// A re-run's named duplicate is recognised by its address, exactly as a
    /// transport event's is.
    ///
    /// The failure it prevents is the one the whole outbox exists for: a phase
    /// crashes after appending, re-runs, and appends the same named call at a
    /// NEW seq. A high-water mark alone cannot recognise the second.
    ///
    /// **What production change leaves this green?** One that deduped named
    /// records under a key of their own — the address would still be equal, so
    /// this would still pass. `one_address_space_covers_both_rails` is the case
    /// that rules that out.
    #[test]
    fn a_re_run_named_event_is_deduped_on_its_address() {
        let first = named(
            1,
            1,
            Phase::Prepare,
            0,
            "plan.step.started",
            serde_json::json!({ "step_id": "s-1" }),
        );
        let duplicate_after_crash = named(
            2,
            1,
            Phase::Prepare,
            0,
            "plan.step.started",
            serde_json::json!({ "step_id": "s-1" }),
        );
        let journal =
            Journal::from_records(vec![first, duplicate_after_crash]).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(projection.deduped, 1);
        assert_eq!(
            sink.named.len(),
            1,
            "the re-run's copy carries the same (iteration, phase, ordinal), so the projector \
             must recognise it rather than delivering the step lifecycle twice"
        );
        assert_eq!(projector.emitted_through_seq(), 2);
    }

    #[test]
    fn a_logical_retry_dedupes_when_only_emit_time_is_regenerated() {
        let first = event_of(
            1,
            1,
            Phase::Prepare,
            0,
            "execution_step_started",
            serde_json::json!({
                "execution_id": "exec-1",
                "step_id": "step-stable",
                "timestamp": 100,
                "correlation_id": "semantic-operation-1"
            }),
            RecordedEventRouting::Unrecorded,
        );
        let retry = event_of(
            2,
            1,
            Phase::Prepare,
            0,
            "execution_step_started",
            serde_json::json!({
                "execution_id": "exec-1",
                "step_id": "step-stable",
                "timestamp": 200,
                "correlation_id": "semantic-operation-1"
            }),
            RecordedEventRouting::Unrecorded,
        );
        let journal = Journal::from_records(vec![first, retry]).expect("well-formed");
        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();

        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(projection.deduped, 1);
    }

    #[test]
    fn different_correlation_join_keys_are_distinct_even_when_everything_else_matches() {
        let first = event_of(
            1,
            1,
            Phase::Prepare,
            0,
            "execution_step_started",
            serde_json::json!({
                "execution_id": "exec-1",
                "step_id": "step-stable",
                "timestamp": 100,
                "correlation_id": "operation-a"
            }),
            RecordedEventRouting::Unrecorded,
        );
        let second = event_of(
            2,
            1,
            Phase::Prepare,
            0,
            "execution_step_started",
            serde_json::json!({
                "execution_id": "exec-1",
                "step_id": "step-stable",
                "timestamp": 200,
                "correlation_id": "operation-b"
            }),
            RecordedEventRouting::Unrecorded,
        );
        let journal = Journal::from_records(vec![first, second]).expect("well-formed");
        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();

        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 2);
        assert_eq!(projection.deduped, 0);
    }

    /// A retry may put a different record at the same positional ordinal.
    /// Content-addressed identities preserve both facts instead of silently
    /// treating the second as a duplicate of the first.
    #[test]
    fn different_retry_members_at_one_positional_ordinal_both_emit() {
        // Same (iteration, phase, ordinal), different rails, different seqs.
        let transport = event(1, 1, Phase::Prepare, 0);
        let named_at_the_same_address = named(
            2,
            1,
            Phase::Prepare,
            0,
            "plan.step.started",
            serde_json::json!({ "step_id": "s-1" }),
        );
        let journal =
            Journal::from_records(vec![transport, named_at_the_same_address]).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 2);
        assert_eq!(projection.deduped, 0);
        assert_eq!(sink.types.len(), 1, "the transport record went first");
        assert_eq!(
            sink.named.len(),
            1,
            "the different named fact must not collide"
        );
    }

    #[test]
    fn a_legacy_positional_cursor_does_not_drop_a_new_body_at_the_same_ordinal() {
        let transport = event(1, 1, Phase::Prepare, 0);
        let named_at_the_same_address = named(
            2,
            1,
            Phase::Prepare,
            0,
            "plan.step.started",
            serde_json::json!({ "step_id": "new-member" }),
        );
        let journal =
            Journal::from_records(vec![transport, named_at_the_same_address]).expect("well-formed");
        // Shape written by the positional projector after it emitted seq one.
        let mut projector: ProjectorCursor = serde_json::from_value(serde_json::json!({
            "emitted_through_seq": 1,
            "recent_keys": [{
                "execution_id": "exec-1",
                "iteration": 1,
                "phase": "prepare",
                "ordinal": 0
            }],
            "window": 1024
        }))
        .expect("legacy cursor");
        let mut sink = RecordingSink::default();

        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(projection.deduped, 0);
        assert_eq!(sink.named_names(), vec!["plan.step.started"]);
    }

    #[test]
    fn identical_events_in_one_batch_keep_distinct_occurrences() {
        let journal = Journal::from_records(vec![
            named(
                1,
                1,
                Phase::Prepare,
                0,
                "plan.step.started",
                serde_json::json!({ "step_id": "same" }),
            ),
            named(
                2,
                1,
                Phase::Prepare,
                1,
                "plan.step.started",
                serde_json::json!({ "step_id": "same" }),
            ),
        ])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 2, &mut sink);

        assert_eq!(projection.emitted, 2);
        assert_eq!(projection.deduped, 0);
        assert_eq!(sink.named.len(), 2);
    }

    /// A mixed batch at DIFFERENT addresses emits both, in seq order.
    ///
    /// The ordinary shape: `emit_step_events_if_signaled` produces
    /// `AgenticStepStarted` and `plan.step.started` from one phase, adjacent in
    /// the same batch. It is the case above's complement, and both are needed —
    /// one says a shared address collides, the other says a shared address space
    /// does not make distinct addresses collide.
    #[test]
    fn a_phase_that_produces_both_rails_has_both_projected() {
        let journal = Journal::from_records(vec![
            event(1, 1, Phase::Prepare, 0),
            named(
                2,
                1,
                Phase::Prepare,
                1,
                "plan.step.started",
                serde_json::json!({ "step_id": "s-1" }),
            ),
            event(3, 1, Phase::Prepare, 2),
        ])
        .expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = projector.project(&journal, "exec-1", 3, &mut sink);

        assert_eq!(projection.emitted, 3);
        assert_eq!(projection.deduped, 0);
        assert_eq!(sink.types.len(), 2);
        assert_eq!(sink.named_names(), vec!["plan.step.started"]);
        assert_eq!(
            sink.attempted_ordinals(),
            vec![0, 0, 1],
            "the walk must offer both rails in seq order; a walk that ran one rail's records \
             first would re-order the timeline it is replaying. Stable ordinals count \
             occurrences per logical body, so each rail's first body starts at zero"
        );
    }

    /// A sink that refuses a NAMED record stops the walk with the mark below it.
    ///
    /// The same head-of-line rule the transport rail has, asserted on this rail
    /// rather than assumed from it: the two go through one `match` on the
    /// delivery `Result`, and this is what says so.
    ///
    /// **What production change leaves this green?** One that made the named arm
    /// answer `Ok` on a refusal and advance the mark — the third assertion is
    /// what catches it, and it is the assertion that matters, because a mark
    /// past an unemitted record is permanent.
    #[test]
    fn a_refused_named_record_stops_the_walk_below_itself() {
        let first = named(
            1,
            1,
            Phase::Prepare,
            0,
            "plan.step.started",
            serde_json::json!({ "step_id": "s-1" }),
        );
        let refused = named(
            2,
            1,
            Phase::Prepare,
            1,
            "plan.step.finished",
            serde_json::json!({ "step_id": "s-1" }),
        );
        let behind_it = named(
            3,
            1,
            Phase::Prepare,
            2,
            "tool.result.projected",
            serde_json::json!({ "tool_name": "gmail__send" }),
        );
        let refused_key = key_of(&refused, 0);
        let journal = Journal::from_records(vec![first, refused, behind_it]).expect("well-formed");

        let mut projector = ProjectorCursor::new();
        let mut sink = RecordingSink::refusing([refused_key]);
        let projection = projector.project(&journal, "exec-1", 3, &mut sink);

        assert_eq!(projection.emitted, 1);
        assert_eq!(
            projection.stopped_at.map(|(seq, _)| seq),
            Some(2),
            "the walk must name the record it could not emit"
        );
        assert_eq!(
            projector.emitted_through_seq(),
            1,
            "the mark must sit BELOW the refused record, or the next pass never comes back for \
             it and nothing ever will"
        );
        assert_eq!(
            sink.named_names(),
            vec!["plan.step.started", "plan.step.finished"],
            "the refused record was offered and the one behind it was not: a walk that skipped \
             ahead would have offered all three"
        );
    }

    /// The named rail's size probe measures the record the STORE will write, not
    /// the one the caller described.
    ///
    /// The mirror of `a_placeholder_iteration_and_phase_do_not_buy_the_producer_
    /// extra_bytes`, and it exists because the two constructors could have had
    /// two probes. They do not — both go through
    /// `measure_against_the_widest_record` — and this is what would fail if a
    /// later edit gave this rail one of its own that measured the caller's
    /// values.
    ///
    /// **What production change leaves this green?** Only a probe that widens
    /// `iteration`, `phase` and `ordinal`. A narrow probe makes the `expect`
    /// below fail on the re-stamped record.
    #[test]
    fn the_named_rails_probe_measures_the_widest_record_too() {
        let mut payload_len = MAX_JOURNAL_RECORD_BYTES;
        let append = loop {
            match JournalAppend::named_event(
                0,
                Phase::Apply,
                "plan.step.finished",
                NAMED_AGENT,
                Some("principal-p".to_string()),
                Some("workspace-w".to_string()),
                serde_json::json!({ "blob": "x".repeat(payload_len) }),
            ) {
                Ok(append) => break append,
                Err(_) => {
                    payload_len = payload_len
                        .checked_sub(1)
                        .expect("some payload is small enough to be accepted");
                },
            }
        };

        // What `commit_boundary` actually writes.
        let re_stamped = JournalRecord {
            seq: 9_001,
            iteration: 250,
            phase: Phase::Epilogue,
            ordinal: 7,
            at_ms: TEST_NOW_MS,
            body: append.body,
        };
        re_stamped
            .to_line()
            .expect("the driver's re-stamp must not push an accepted named body over the ceiling");
    }

    /// An oversized named payload is refused by the producer's constructor.
    ///
    /// Stated as its own case because the answer to the refusal is the
    /// producer's — drop the record and keep emitting — and the constructor
    /// returning `Result` is what moves that decision to somebody who can make
    /// it. A named constructor that did not measure would put the refusal in the
    /// store's batch encode, where the answer is to fail the commit and stall a
    /// live run.
    #[test]
    fn an_oversized_named_payload_is_refused_at_the_constructor() {
        let refused = JournalAppend::named_event(
            1,
            Phase::Apply,
            "tool.result.projected",
            NAMED_AGENT,
            Some("principal-p".to_string()),
            Some("workspace-w".to_string()),
            serde_json::json!({ "blob": "x".repeat(MAX_JOURNAL_RECORD_BYTES + 1) }),
        );
        assert!(
            matches!(refused, Err(JournalError::RecordTooLarge { seq: 0, .. })),
            "a payload over the ceiling must be refused, and the error must carry the \
             no-seq-assigned sentinel rather than the probe's u64::MAX: got {refused:?}"
        );
    }

    /// A named record survives the line the store writes, scope included.
    ///
    /// Round-tripped through `to_line` and `serde_json::from_str` rather than
    /// through `clone`, because the question is what the BYTES carry.
    ///
    /// **What production change leaves this green?** One that changed a field
    /// this does not read — and there is none: equality covers all five, and the
    /// two raw-string assertions additionally pin the wire tag and the fact that
    /// the scope is present on the wire rather than being reconstructed by a
    /// `serde(default)` on the way back in.
    #[test]
    fn a_named_record_round_trips_through_a_line_with_its_scope_on_the_wire() {
        let original = named(
            4,
            3,
            Phase::Epilogue,
            2,
            "plan.step.finished",
            serde_json::json!({ "step_id": "s-9", "status": "completed" }),
        );
        let line = original.to_line().expect("a small named record encodes");

        assert!(
            line.contains(r#""kind":"named_event""#),
            "the variant tag must be the snake_case name the enum declares, because a reader on \
             an older build distinguishes bodies by it: {line}"
        );
        assert!(
            line.contains(r#""principal":"principal-p""#),
            "the scope must be ON THE WIRE. `principal` is `skip_serializing_if = \
             \"Option::is_none\"`, so a record that lost it would come back as `None` through \
             the field's own default and this round trip would still be equal: {line}"
        );

        let decoded: JournalRecord =
            serde_json::from_str(&line).expect("a named record must decode");
        assert_eq!(decoded, original);
    }

    /// Records written before this variant existed still load.
    ///
    /// Hand-written bytes, not a round trip: the claim is about a file already on
    /// disk, and a round trip through today's `Serialize` can only ever produce
    /// today's shape. The three lines are the three bodies that existed before
    /// `NamedEvent`, and the `event` line deliberately carries **no** `routing`
    /// field — that is what a record written before that field existed looks
    /// like, and it must read back as `Unrecorded` rather than failing to parse.
    ///
    /// **What production change leaves this green?** One that adds a variant, in
    /// the direction this checks. What it catches is `deny_unknown_fields`
    /// arriving on `JournalRecord` or on a body, or a field losing its
    /// `serde(default)` — either of which turns every historical journal into a
    /// `CorruptRecord` that fails the whole file.
    #[test]
    fn records_written_before_the_named_variant_still_load() {
        let raw = concat!(
            r#"{"seq":1,"iteration":1,"phase":"prepare","ordinal":4294967295,"at_ms":1700000000000,"body":{"kind":"phase_completed","step":{"step":"continued"}}}"#,
            "\n",
            r#"{"seq":2,"iteration":1,"phase":"prepare","ordinal":0,"at_ms":1700000000001,"body":{"kind":"event","event_type":"AgenticIterationStarted","payload":{"iteration":1}}}"#,
            "\n",
            r#"{"seq":3,"iteration":1,"phase":"resolve","ordinal":1,"at_ms":1700000000002,"body":{"kind":"owner_transition","to_agent_id":"agent-b"}}"#,
            "\n",
        );

        let journal = Journal::parse(raw).expect(
            "a journal written before the named variant must still parse; a variant added to a \
             tagged enum is safe in the read direction only while nothing denies unknown fields",
        );
        // `authoritative` is the only way records leave that type, and it takes
        // the watermark for exactly that reason — see its own docs. Three is the
        // seq of the last line, so this is "everything in the file".
        let records = journal.authoritative(3);
        assert_eq!(records.len(), 3);

        assert!(matches!(
            records[0].body,
            JournalBody::PhaseCompleted {
                step: RecordedStep::Continued
            }
        ));
        match &records[1].body {
            JournalBody::Event {
                event_type,
                payload,
                routing,
            } => {
                assert_eq!(event_type, "AgenticIterationStarted");
                assert_eq!(payload["iteration"], 1);
                assert_eq!(
                    routing,
                    &RecordedEventRouting::Unrecorded,
                    "a record written before the routing field must read as an ABSENCE of a \
                     decision, not as a decision"
                );
            },
            other => panic!("got {other:?}"),
        }
        match &records[2].body {
            JournalBody::OwnerTransition {
                from_agent_id,
                to_agent_id,
                transition_authorization,
            } => {
                assert_eq!(from_agent_id, &None);
                assert_eq!(to_agent_id, "agent-b");
                assert_eq!(transition_authorization, &None);
            },
            other => panic!("got {other:?}"),
        }

        // And the fold still runs over them, which is the half that matters:
        // parsing a historical file buys nothing if replay refuses it.
        let cursor = replay(records).expect("a historical journal must still replay");
        assert_eq!(cursor.seq, 3);
    }
}
