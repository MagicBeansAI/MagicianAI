//! Where a run's state lives when no process is holding it.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Storage*. The trait is
//! the design's, narrowed where the design left a choice and widened where
//! building it showed the sketch was short a method.
//!
//! # The posture is fail-closed
//!
//! *"Store unavailable → fail closed — refuse to advance rather than run
//! unrecorded work."* Every method returns [`StoreResult`] and no method has a
//! "best effort" mode. A caller that cannot record what it is about to do must
//! not do it, which is the same posture `take_durable` already takes for pause
//! records.
//!
//! # What the design's sketch did not say, and this had to decide
//!
//! - **`load` returns the revision too.** `commit` takes `expected: Revision`,
//!   and the only honest source of that value is the load it is a
//!   compare-and-swap against. A caller that had to invent it would invent a
//!   stale one.
//! - **The store numbers journal records.** The sketch passes `&[JournalRecord]`,
//!   which carry a seq. A caller-assigned seq is a second source of truth about
//!   the log's length and the two disagree after a crash, so callers pass
//!   [`JournalAppend`] and the store places them.
//! - **Effects get their own methods.** They are a status map keyed by
//!   `effect_id`, not a log, because parallel batch members settle out of order —
//!   so "what happened to this one" has to be one lookup.
//! - **A wake token is resolved through the store.** A parked execution is not
//!   runnable until something outside says so, and that something — a job runner,
//!   a child execution — does not hold the parent's lease and must not have to
//!   compare-and-swap the parent's state to wake it.
//!
//! # Orphan sweeping is part of the contract, not an implementation detail
//!
//! A worker that appends and then dies leaves records above the committed
//! watermark. The design's rule is that they are never replayed and are *"swept
//! on next commit"* — and the sweep is load-bearing rather than tidy. Without it
//! the next attempt's records land **after** the orphans, so the next commit's
//! watermark buries them *below* itself and replay picks them up. Every
//! implementation therefore sweeps orphans before appending, and the contract
//! suite checks it.

pub mod fs;
pub mod memory;

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::effects::{EffectError, EffectId, EffectLedger, EffectLedgerEntry, EffectOutcome};
use super::journal::{
    replay as replay_journal, Journal, JournalAppend, JournalBody, JournalError, JournalRecord,
    ProjectorCursor, RecordedStep, ReplayedCursor, TerminalKind,
};
use super::state::{LoopState, WaitReason, WorkerId};

/// The longest an execution id may be as a directory name.
const MAX_EXECUTION_ID_BYTES: usize = 128;

/// The most outstanding resolutions one wake token may hold.
///
/// A parked run consumes what it acted on every time it wakes, so this bounds a
/// completer reporting without bound rather than a run waiting a long time.
///
/// Declared here rather than in `fs`, because it is **not** one of the ceilings
/// that exist because a file has to be read back. `memory`'s module docs say
/// every non-byte ceiling is shared by both implementations; this one was
/// enforced in `fs` alone, so a completer that ran the in-process driver out of
/// memory would have been refused in production and admitted in the tests that
/// were supposed to catch it.
pub(super) const MAX_WAKE_RESOLUTIONS: usize = 1_024;
/// Maximum execution directories one cursored discovery page may inspect,
/// regardless of how sparse its matches are. Output limits alone do not bound
/// a scan: a page seeking one runnable/parked key can otherwise walk the entire
/// store when every row is withheld.
pub(super) const MAX_DISCOVERY_VISITS_PER_PAGE: usize = 1_024;

/// Which execution, in which scope.
///
/// # Why the fields are private, and why private fields were not enough
///
/// Because [`Self::execution_id`] becomes a path component. A key whose fields
/// could be written after construction is a key whose validation can be bypassed,
/// and the value that bypasses it is the one that escapes the scope root.
///
/// Private fields close the construction route and leave the **deserializer**
/// wide open: a derived `Deserialize` builds the struct field by field and never
/// reaches [`ExecutionKey::new`]. That is not a theoretical entry point — the
/// filesystem store writes a key record beside every execution and reads it back
/// on every scan, and the key it recovers is joined into a path — so
/// deserialization goes through [`ExecutionKeyWire`] and lands on the same
/// refusal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "ExecutionKeyWire")]
pub struct ExecutionKey {
    principal: String,
    workspace: String,
    execution_id: String,
}

/// The shape a stored key is read back through.
///
/// Exists only so that reading a key applies the same rules as building one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionKeyWire {
    principal: String,
    workspace: String,
    execution_id: String,
}

impl TryFrom<ExecutionKeyWire> for ExecutionKey {
    type Error = KeyError;

    fn try_from(wire: ExecutionKeyWire) -> Result<Self, Self::Error> {
        ExecutionKey::new(wire.principal, wire.workspace, wire.execution_id)
    }
}

impl ExecutionKey {
    /// Build a key, refusing one that could not address a directory safely.
    ///
    /// # Rejected, not sanitized
    ///
    /// The scope segments go through the workspace layout's own normalisation,
    /// the same as every other scoped path — that is a shared convention and
    /// diverging from it would put this store's files somewhere no other tool
    /// looks.
    ///
    /// The **execution id** is different: this module creates that directory, and
    /// sanitizing it would map two distinct executions onto one directory, where
    /// they would silently share a journal and a lease. Two runs' state merging
    /// is a far worse failure than a rejected id, so an id that is not already a
    /// safe segment is refused.
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> Result<Self, KeyError> {
        let principal = principal.into();
        let workspace = workspace.into();
        let execution_id = execution_id.into();

        if principal.trim().is_empty() {
            return Err(KeyError::EmptyScope { field: "principal" });
        }
        if workspace.trim().is_empty() {
            return Err(KeyError::EmptyScope { field: "workspace" });
        }
        if execution_id.is_empty() {
            return Err(KeyError::EmptyScope {
                field: "execution_id",
            });
        }
        if execution_id.len() > MAX_EXECUTION_ID_BYTES {
            return Err(KeyError::ExecutionIdTooLong {
                bytes: execution_id.len(),
            });
        }
        // A leading dot would hide the directory and, more to the point, admits
        // `.` and `..` without a second rule.
        if execution_id.starts_with('.') {
            return Err(KeyError::UnsafeExecutionId {
                execution_id: execution_id.clone(),
            });
        }
        if !execution_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(KeyError::UnsafeExecutionId {
                execution_id: execution_id.clone(),
            });
        }

        Ok(Self {
            principal,
            workspace,
            execution_id,
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }
}

impl fmt::Display for ExecutionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.principal, self.workspace, self.execution_id
        )
    }
}

/// Why a key was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    EmptyScope { field: &'static str },
    ExecutionIdTooLong { bytes: usize },
    UnsafeExecutionId { execution_id: String },
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::EmptyScope { field } => write!(f, "an execution key needs a {field}"),
            KeyError::ExecutionIdTooLong { bytes } => {
                write!(f, "an execution id of {bytes} bytes is over the limit")
            },
            KeyError::UnsafeExecutionId { execution_id } => write!(
                f,
                "the execution id {execution_id:?} is not a safe directory name; it is refused \
                 rather than sanitized, because sanitizing would merge two runs' state into one \
                 directory"
            ),
        }
    }
}

impl std::error::Error for KeyError {}

/// Which committed version of an execution's state this is.
///
/// Monotonic within an execution and meaningless across executions.
/// [`Revision::INITIAL`] is what a caller passes to commit the first state — it
/// is "nothing is committed yet", not "revision zero exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    pub const INITIAL: Revision = Revision(0);

    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// The revision a commit against `self` produces.
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl Default for Revision {
    /// A fresh, uncommitted execution.
    ///
    /// Spelled out rather than derived so it reads as the same statement
    /// [`Revision::INITIAL`] makes: *nothing is committed yet*. A derived
    /// `Revision(0)` would be the same value carrying none of that meaning, and
    /// the first reader to treat it as "revision zero exists" would write a
    /// compare-and-swap that always succeeds.
    fn default() -> Self {
        Revision::INITIAL
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// A loaded state and the revision to commit against.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedLoopState {
    pub revision: Revision,
    pub state: LoopState,
}

/// The non-resumable terminal a **committed** journal prefix ends on.
///
/// # Why the store keeps this at all, given the journal already has it
///
/// Because a scheduler cannot afford to read a journal. `LoopState` carries no
/// terminal — a run that ended is claimable, unleased, unparked and inside every
/// timer — so a scan judging the committed state alone offers a finished run
/// forever, and a finished run at the head of the walk holds its slot in every
/// later scan's `limit`. Reading each run's log inside the scan would make the
/// listing `O(records)` per key, which is the cost the design rules out. So the
/// terminal is *published* by the commit that made it authoritative and is one
/// small read on the scan path.
///
/// # `seq` is not decoration, and neither field may default
///
/// `seq` is the record's position, and a reader must ignore this whole marker
/// unless `seq` **equals** the state's committed watermark: any other value is
/// a marker written for a different prefix, and acting on it would withhold a
/// run that has not ended. A `#[serde(default)]` on `seq` would read as zero,
/// zero can accidentally compare below every real watermark,
/// and one dropped key would therefore strand a live execution permanently and
/// silently. `terminal` cannot default either, for the plainer reason that
/// there is no honest default for *which way a run ended*.
///
/// Both are required on the wire and the file is `deny_unknown_fields`, so a
/// marker this store cannot fully understand is a read error rather than a
/// half-understood ending. The read path treats that error the way it treats an
/// unreadable wake ledger: the execution is withheld from scheduling and said
/// so, loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndedRun {
    /// The seq of the `RunEnded` record. Meaningful only against the watermark
    /// of the state loaded beside it.
    pub(super) seq: u64,
    pub(super) terminal: TerminalKind,
    /// Last authoritative event that must be projected before scheduler
    /// discovery may suppress this terminal execution. `None` means the entire
    /// committed prefix contained no event. Default preserves compatibility
    /// with markers written before this field existed.
    #[serde(default)]
    pub(super) last_event_seq: Option<u64>,
    /// Distinguishes a newly written `last_event_seq: null` (there were no
    /// events) from an older marker that predates outbox-debt metadata. Old
    /// markers fail safe by requiring projection through the terminal seq.
    #[serde(default)]
    pub(super) has_outbox_metadata: bool,
}

impl EndedRun {
    pub(super) fn projection_through_seq(self) -> Option<u64> {
        if self.has_outbox_metadata {
            self.last_event_seq
        } else {
            Some(self.last_event_seq.unwrap_or(self.seq))
        }
    }
}

/// The non-resumable terminal a committed prefix ends on, if it ends on one.
///
/// # Why the LAST record is the whole answer
///
/// [`super::journal::replay_each`] sets its cursor's terminal on a `RunEnded`
/// record and **clears it again** the moment any further record follows a
/// *resumable* one. So for every journal that replays at all, `replay(..)
/// .terminal == Some(t)` exactly when the last record is `RunEnded { t }` — and
/// this function is that statement without the fold. It is written here, once,
/// rather than at each store, so the two implementations cannot come to
/// different views of what "this run ended" means.
///
/// # What it answers for a journal replay would REFUSE
///
/// A record after a non-resumable terminal is `JournalError::RecordAfterTerminal`
/// on the next read. This function answers `None` for that shape, because its
/// last record is not a `RunEnded`. The consequence is that such a run stays
/// **offered**, is claimed, and is quarantined by `driver_worker::verify_journal`
/// where the corruption is legible. That is the safe direction: this function's
/// job is to withhold runs that have certainly ended, and never to withhold one
/// on a guess.
///
/// **That consequence depends on the caller retracting**, and for a while it did
/// not hold. The shape arrives at a run whose earlier commit already published
/// an ending, and a store that only ever wrote the marker left the old one in
/// place. Exact seq/watermark equality now makes that stale marker inert, and
/// both implementations still retract it so the index remains truthful. Both
/// implementations
/// therefore publish on `Some` and retract on `None`, and
/// `an_ending_is_retracted_when_the_prefix_no_longer_ends_on_one` is what holds
/// them to it. Nothing stops the shape from arising, either — `append_journal`
/// accepts a record after a committed terminal — so this is a case the store
/// handles rather than one a caller is trusted to avoid.
///
/// # A resumable terminal is deliberately not an ending
///
/// `WaitingForUser`, `WaitingForConfirmation`, `PausedByUser`,
/// `WaitingForChildren` and `Sleeping` end the *invocation* and leave the
/// execution alive for something outside to answer. Withholding those would make
/// a paused run unresumable, which is a worse failure than the starvation this
/// exists to fix — a spin is bounded and observable, a run nothing will ever
/// offer again is neither.
///
/// # Where the line is drawn is NOT this function's decision, and that matters
///
/// [`TerminalKind::is_resumable`] answers **false** for `BudgetExhausted` and
/// `MaxIterationsReached` even when their `AgenticOutcome` carries a pause.
/// That is intentional at this layer: the exact LoopState segment is final,
/// while its integrity-bound terminal settlement receipt can publish a
/// separate Artifact pause generation for a later segment.
///
/// The reason is that softening it would put the two halves of one rule in two
/// places. `driver_worker::advance_under_lease` already refuses such a run with
/// `Advanced::RunAlreadyEnded`, on this same predicate, so a scan that went on
/// offering it would be offering work no claim can take — which is the starvation
/// this exists to remove, kept alive for a case the claim refuses anyway. This
/// filter is exactly as conservative as that refusal, no more: if the line is in
/// the wrong place, it is in the wrong place on `TerminalKind`, and moving it
/// there moves both.
/// # It answers with the RECORD's seq, not the caller's watermark
///
/// The two are the same at the commit that publishes the ending and drift apart
/// afterwards, and the record's is the one a later reader can use: it means *the
/// run ended at record N*, so a state whose watermark has not reached N does not
/// vouch for the ending and must ignore it. A marker stamped with the
/// publishing commit's watermark instead would answer a subtly different
/// question — *some commit once saw an ending* — which is not one a scan can act
/// on.
pub(super) fn non_resumable_terminal(authoritative: &[JournalRecord]) -> Option<EndedRun> {
    let record = authoritative.last()?;
    match &record.body {
        JournalBody::PhaseCompleted {
            step: RecordedStep::RunEnded { terminal },
        } if !terminal.is_resumable() => Some(EndedRun {
            seq: record.seq,
            terminal: *terminal,
            last_event_seq: authoritative.iter().rev().find_map(|record| {
                matches!(
                    record.body,
                    JournalBody::Event { .. } | JournalBody::NamedEvent { .. }
                )
                .then_some(record.seq)
            }),
            has_outbox_metadata: true,
        }),
        _ => None,
    }
}

/// Where a scan of the runnable set left off.
///
/// # It names a POSITION IN A WALK, not a key that exists
///
/// Feeding one back means *"resume strictly after this point"*, and the point
/// need not still be there: an execution directory removed between two pages is
/// simply a position nothing sits at any more, and the walk carries on from
/// where it would have been. That is why this is a position rather than a
/// [`ExecutionKey`] — the two look alike and behave differently under deletion,
/// and a caller that treated a cursor as a key would eventually ask a store to
/// resume from a run it no longer holds.
///
/// # Opaque on purpose
///
/// Constructed and read only by implementations. A caller passes back exactly
/// what a scan handed it and nothing else, because the ordering a cursor is
/// compared against is the implementation's own: `fs` walks **directory** names,
/// which are the scope segments after the workspace layout's normalisation, and
/// `memory` walks its map's key order. Those two coincide for every key whose
/// segments survive normalisation unchanged and are not required to coincide in
/// general — a cursor is only ever fed back to the store that produced it.
///
/// # Three segments, compared as a tuple and never as a joined string
///
/// A scope segment may contain a `/`, so a single joined token would order
/// `a/b · c` and `a · b/c` identically while the walk does not. Holding the
/// segments apart makes the comparison the same lexicographic tuple order both
/// implementations already walk in.
///
/// # It is NOT serialisable, which is what makes the opacity above true
///
/// It derived `Serialize` and `Deserialize` for a while, and neither had a
/// user: no caller persists a cursor, and one that did would be persisting a
/// position in a live directory tree. The `Deserialize` half was the problem —
/// a derive builds the private field directly, so any module could mint a
/// position out of a JSON literal and skip whatever sorts below it, which is
/// exactly what "constructed only by implementations" says cannot happen.
/// [`ExecutionKey`] closes the same hole with a wire type because it genuinely
/// has to cross a file; this does not, so the cheaper answer is to not be
/// serialisable at all. Ordering is derived because both implementations
/// compare positions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScanCursor {
    after: [String; 3],
}

impl ScanCursor {
    /// The position of the execution a walk has just finished examining.
    pub(super) fn at(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        execution_id: impl Into<String>,
    ) -> Self {
        Self {
            after: [principal.into(), workspace.into(), execution_id.into()],
        }
    }

    /// The same position, when the caller already holds the three segments.
    pub(super) fn from_segments(after: [String; 3]) -> Self {
        Self { after }
    }

    /// The segments, for an implementation to compare its own walk against.
    pub(super) fn segments(&self) -> &[String; 3] {
        &self.after
    }
}

impl fmt::Display for ScanCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.after[0], self.after[1], self.after[2])
    }
}

/// One page of [`LoopStateStore::scan_runnable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnableScan {
    /// The keys this page offers, in the walk's order.
    pub keys: Vec<ExecutionKey>,
    /// Where the next page resumes, or `None` when the walk reached the end of
    /// the store.
    ///
    /// # `None` is a claim, not an absence
    ///
    /// It says *there is nothing after this*, and a caller stops paging on it.
    /// An implementation that stopped early — because the page filled, or
    /// because its own scan ceiling fired — **must** name a resume point here,
    /// or the tail of its store becomes work no caller ever asks for. That is
    /// the same failure the uncursored [`LoopStateStore::list_runnable`] has by
    /// construction, merely moved behind a field that looks like it answered.
    ///
    /// # And `Some` must be STRICTLY after the cursor the page was given
    ///
    /// A page that hands back the cursor it arrived with looks like an answer
    /// and is not one: the next call re-asks the same question, and a caller
    /// paging in a loop never leaves the prefix it was trying to escape — which
    /// is the defect this whole method exists to remove, wearing the shape of
    /// the fix. It is worth stating because the two fallbacks that produced it
    /// both read as prudence: *resume where I was told to start*, and *resume
    /// from the beginning*.
    ///
    /// Both implementations get this by naming a position on every stopping
    /// path — the entry a full page stopped on, and the last name of any level
    /// they could not finish — so no path is left needing a fallback. A stop
    /// that could genuinely name nothing answers `None` and ends the page,
    /// because an under-reported pass is recovered by the next one and a
    /// repeated page is recovered by nothing.
    pub resume: Option<ScanCursor>,
}

/// One bounded page of committed terminal executions that still owe outbox,
/// runtime-settlement, or operator-steer receipt projection.
///
/// Unlike [`RunnableScan`], the budget for this scan is the number of execution
/// entries **examined**, not the number of matching keys returned. Terminal
/// debt is normally sparse; bounding only matches would still permit a cadence
/// pass to walk the whole store while looking for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOutboxScan {
    /// Exact keys whose committed ending still has event/runtime projection
    /// debt, or whose terminal/control receipt awaits idempotent retirement.
    pub keys: Vec<ExecutionKey>,
    /// Where the next bounded page resumes. `None` means the walk reached the
    /// end of the store.
    pub resume: Option<ScanCursor>,
}

/// The right to advance one execution, for a while.
///
/// # The fence is not decoration
///
/// A worker whose lease expires does not find out at the moment it expires — it
/// finds out the next time it talks to the store. Between those two moments
/// another worker may hold the lease. `fence` increases on every acquisition and
/// every renewal, so a stale holder's renew or release names a fence the store
/// has moved past and is refused rather than silently clobbering the new holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub key: ExecutionKey,
    pub worker: WorkerId,
    /// Strictly increasing per execution. Never reused, including across a
    /// release, so a straggler can never re-present an old one.
    pub fence: u64,
    pub expires_at_ms: i64,
}

impl Lease {
    pub fn is_expired_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms <= now_ms
    }
}

/// The most recent lease an execution ever had, live, expired or released.
///
/// [`Lease`] is the right to advance a run and is only ever handed to the worker
/// that holds it. This is a **report about** a lease, handed to a reader that
/// holds no lease on the run it is reading about — the reconciler, which is
/// enumerating runs it has not claimed. That is why it is a separate type rather
/// than a `Lease` with a flag: nothing here can be presented to `renew` or
/// `release`.
///
/// The distinction survived the reconciler learning to write. A retirement takes
/// a **fresh** lease through [`LoopStateStore::claim`] and acts on that; nothing
/// in a listing is ever promoted into one, which is exactly what this type not
/// being a `Lease` makes impossible rather than merely discouraged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastLease {
    pub worker: WorkerId,
    pub fence: u64,
    /// **Zero whenever [`Self::released`] is true, in both implementations.**
    ///
    /// Releasing publishes a marker rather than recording a time, so a released
    /// lease carries no timestamp at all. A reader must therefore not read
    /// `expires_at_ms` as *"nothing has touched this run since"* — the honest
    /// reading of a long-past expiry is available only for a lease that was
    /// **never released**, which is a worker that died holding it. A reader that
    /// forgets this would report every ordinary parked run as abandoned, since
    /// the ordinary parked run's last lease is a released one.
    pub expires_at_ms: i64,
    pub released: bool,
}

/// A parked execution, as a reconciler needs to see it.
///
/// Deliberately **not** a `CommittedLoopState`: the listing enumerates runs
/// **nobody has claimed**, and handing a whole loop state to an unleased reader
/// invites it to reason about — or worse, to republish — a run it does not hold.
///
/// # This is a bound on the LISTING, not on the reconciler, and the difference
/// is the whole safety argument
///
/// [`super::reconciler`] does load and republish a state when it retires a park.
/// What it may not do is republish **this** one: it claims the key first and
/// loads again under the lease, and it is the re-loaded state — with its own
/// revision, which the commit compare-and-swaps against — that it writes back.
/// A state carried out of an unclaimed listing would have neither, so the row
/// stopping at what scheduling needs is what makes the shortcut unavailable
/// rather than merely unwise.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedExecution {
    pub key: ExecutionKey,
    /// What the committed state is waiting for. The wake token is derived from
    /// this rather than stored beside it, so there is one definition of the
    /// token a completer resolves.
    pub wait: WaitReason,
    /// When the commit that established this park was published, as the store
    /// can best tell.
    ///
    /// **Best effort, and every caller must handle `None`.** The filesystem
    /// store answers with the modification time of the newest snapshot, which is
    /// the commit that published the parked state — subject to clock skew, to a
    /// restore that rewrote mtimes, and to a filesystem that does not keep one.
    /// The in-memory store answers with the wall clock it read at commit.
    ///
    /// It is a **lower bound** on how long the run has been parked: the value is
    /// the *last* commit, and a run that parked earlier and re-committed the same
    /// park reads as younger than it is. Erring young means a reconciler waits
    /// longer before looking, which is the direction that produces no false
    /// alarms.
    pub parked_since_ms: Option<i64>,
    /// The most recent lease, if this run was ever claimed. See [`LastLease`]
    /// for why a released one carries no timestamp.
    pub last_lease: Option<LastLease>,
    /// The run's stored wall-clock deadline, copied straight off the state.
    ///
    /// Carried here because it is the one thing about a parked run that is both
    /// **free** — the listing has already read the state — and **decisive**: a
    /// park past its own deadline can never advance again whatever happens to its
    /// wake. `list_runnable` withholds an unresolved park, so no scheduler scan
    /// offers it; and if something claims it by key, `advance_under_lease`
    /// answers while the wait stands and commits the canonical timeout terminal
    /// the moment it does not. No ordinary phase runs on either path. An expired
    /// execution with no wait is offered specifically so the driver can record
    /// that retirement.
    ///
    /// A reconciler that had to take a second read to learn this would have put
    /// it behind an age gate, where the one run that is already provably dead
    /// would wait longest.
    ///
    /// `None` means no deadline, which is different from a deadline in the past.
    /// Every reader must keep them apart — the same rule
    /// [`LoopState::deadline_at_ms`](super::state::LoopState::deadline_at_ms)
    /// states at the field this is copied from.
    pub deadline_at_ms: Option<i64>,
}

/// What one pass over the store's parked executions found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParkedListing {
    pub parked: Vec<ParkedExecution>,
    /// Position strictly after the entries this page examined. `None` means
    /// the ordered walk reached its end; callers that need round-robin coverage
    /// reset to the beginning after that pass.
    pub resume: Option<ScanCursor>,
    /// **This pass did not see the whole store.** Four causes, deliberately one
    /// flag: the scan filled `limit`, it hit the implementation's own scan
    /// ceiling, it skipped an execution it could not read, or it skipped a
    /// directory entry it could not identify at all.
    ///
    /// The fourth is the one that was missing, and it was the quietest: a walk
    /// that cannot ask *is this a directory* — a filesystem that answers the
    /// question with an `lstat`, under a parent that is readable but not
    /// searchable — used to skip the entry and every execution beneath it
    /// without setting anything, so an entire subtree left the reconciler's
    /// coverage while the pass reported itself complete.
    ///
    /// One flag because every cause has the same consequence for the only caller
    /// that matters — a parked run may exist that this pass never looked at — and
    /// a reconciler that reported "nothing is wrong" off an incomplete pass would
    /// be claiming coverage it does not have.
    ///
    /// A skip whose answer is COMPLETE does not set it: a name that is not UTF-8
    /// and a path that is not a directory cannot be an execution this store
    /// wrote, and flagging them would make a stray file a permanent coverage
    /// alarm that no action clears.
    pub incomplete: bool,
}

/// Why a store operation could not be performed.
///
/// No variant means "it maybe worked". A store that could not answer says so, and
/// the caller's obligation is to stop rather than to proceed unrecorded.
#[derive(Debug)]
pub enum StoreError {
    /// The compare-and-swap failed: somebody else committed in between.
    Conflict {
        expected: Revision,
        found: Revision,
    },
    /// Another worker holds a live lease.
    LeaseHeld {
        by: WorkerId,
        until_ms: i64,
    },
    /// This lease is no longer the current one — it expired and was taken, or it
    /// was already released.
    LeaseLost {
        fence: u64,
    },
    /// The bytes on the other side are not what this store wrote. Quarantine and
    /// surface; never repair by guessing.
    Corrupt {
        key: ExecutionKey,
        detail: String,
    },
    /// A commit claimed a journal watermark the log does not reach.
    ///
    /// Refused rather than accepted, because every later replay would expect
    /// history that does not exist — and would fail far from the commit that
    /// caused it.
    WatermarkAhead {
        watermark: u64,
        last_seq: u64,
    },
    /// One token accumulated more outstanding resolutions than a bounded read
    /// will take.
    ///
    /// A completer that reports without bound is a bug in the completer, and the
    /// store refusing is how it becomes visible — the alternative is a wake
    /// directory that grows until a listing is the slowest thing in the scan.
    WakeLedgerFull {
        wake_token: String,
        limit: usize,
    },
    Journal(JournalError),
    Effect(EffectError),
    Key(KeyError),
    /// The substrate could not be reached. Fail closed.
    Unavailable {
        detail: String,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Conflict { expected, found } => write!(
                f,
                "a commit expected {expected} but the store is at {found}; another holder \
                 committed in between"
            ),
            StoreError::LeaseHeld { by, until_ms } => {
                write!(f, "worker {by} holds the lease until {until_ms}")
            },
            StoreError::LeaseLost { fence } => write!(
                f,
                "lease fence {fence} is no longer current; this holder was superseded"
            ),
            StoreError::Corrupt { key, detail } => {
                write!(f, "the stored state for {key} is unreadable: {detail}")
            },
            StoreError::WatermarkAhead {
                watermark,
                last_seq,
            } => write!(
                f,
                "a commit claims journal watermark {watermark} but the log reaches only \
                 {last_seq}"
            ),
            StoreError::WakeLedgerFull { wake_token, limit } => write!(
                f,
                "the wake token {wake_token} holds more than {limit} unconsumed resolutions; a \
                 completer is reporting without bound"
            ),
            StoreError::Journal(error) => write!(f, "{error}"),
            StoreError::Effect(error) => write!(f, "{error}"),
            StoreError::Key(error) => write!(f, "{error}"),
            StoreError::Unavailable { detail } => {
                write!(f, "the loop state store is unavailable: {detail}")
            },
        }
    }
}

impl std::error::Error for StoreError {}

impl From<JournalError> for StoreError {
    fn from(error: JournalError) -> Self {
        StoreError::Journal(error)
    }
}

impl From<EffectError> for StoreError {
    fn from(error: EffectError) -> Self {
        StoreError::Effect(error)
    }
}

impl From<KeyError> for StoreError {
    fn from(error: KeyError) -> Self {
        StoreError::Key(error)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// A published receipt that **no further segment will follow this one**.
///
/// # The window it closes, and why nothing already on the run could close it
///
/// A delegated child's work does not stop at one loop-state key.
/// `executor.rs`'s `execute_agentically_with_refinement` may commission a
/// refinement pass, which runs under `{child}-p1` while pass 0's
/// `RunEnded { Success }` already sits committed on the bare key. A reader
/// judging the child from its records alone therefore has to decide what an
/// **empty successor address** means, and the two readings are opposite:
/// *nothing followed* and *the successor has not committed yet*. Between the
/// predecessor's terminal commit and the successor's first commit — seconds of
/// `build_run_setup` and seeding — only the second is true, and a reader that
/// guessed the first proves a working child finished.
///
/// Nothing already on the run can be asked. The journal records a
/// [`TerminalKind`] and the decision to refine is taken *after* that record is
/// committed, by the wrapper around the loop rather than by the loop; and a
/// record appended after a non-resumable terminal is
/// `JournalError::RecordAfterTerminal` on the next read, so the answer cannot
/// be appended where the question is asked.
///
/// So it is published beside the journal instead, once, by the writer that took
/// the decision — and it is the **presence** of the receipt that is evidence,
/// never its absence. A crash before it is written leaves no receipt, a reader
/// refuses to prove anything, and the park it was judging stays exactly as it
/// was. That is the whole safety argument: this value can only ever turn a
/// refusal into a proof, never the other way round.
///
/// # Why it is not on [`LoopState`]
///
/// The same reason [`ProjectorCursor`] is not: it is knowable only *after* the
/// commit that ended the segment, so carrying it on the state would mean a
/// second compare-and-swap against a revision the ended run no longer has a
/// claimant for. It is also written at most once per segment, where a state is
/// written six times an iteration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainClosure {
    /// The loop-state segment id this receipt was published for.
    ///
    /// Carried inside the receipt as well as in its address, so a receipt read
    /// out of the wrong directory — a copied execution tree, a restore that
    /// renamed a run — is refused rather than believed. A reader compares it
    /// against the key it asked about and treats a mismatch as no receipt at
    /// all.
    pub segment: String,
    /// When the closing writer published it. Diagnostic only: nothing compares
    /// it against a clock, because a receipt is evidence by existing and an
    /// old one says exactly what a new one does.
    pub closed_at_ms: i64,
}

impl ChainClosure {
    /// The receipt a writer publishes for the segment it has just finished.
    pub fn for_segment(segment: impl Into<String>, closed_at_ms: i64) -> Self {
        Self {
            segment: segment.into(),
            closed_at_ms,
        }
    }

    /// Whether this receipt is the one `key` asked for.
    ///
    /// A free-standing question rather than an assumption at the call site: the
    /// reader that gets this wrong does not fail, it proves something about the
    /// wrong run.
    pub fn closes(&self, key: &ExecutionKey) -> bool {
        self.segment == key.execution_id()
    }
}

/// Everything a worker needs to advance an execution it does not own.
///
/// # Why the methods are `async` even though the filesystem impl is not
///
/// The substrate the design points at next is object storage or Postgres, and
/// both are genuinely async. A trait that was sync today would have to be
/// rewritten — along with every caller — the first time it was implemented over a
/// network. The filesystem implementation moves each complete transaction onto
/// a bounded blocking executor; network-backed implementations can await their
/// substrate directly.
#[async_trait]
pub trait LoopStateStore: Send + Sync {
    /// The committed state and the revision to commit against, or `None` when
    /// this execution has never committed.
    async fn load(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>>;

    /// Publish a new state, refusing it if the store has moved on.
    ///
    /// `expected` must be the revision the caller loaded. The first commit for an
    /// execution passes [`Revision::INITIAL`].
    ///
    /// # It also publishes the run's ending, and only a commit can
    ///
    /// When the journal prefix this commit vouches for ends on a non-resumable
    /// terminal, the store records that beside the state — see [`EndedRun`] and
    /// [`Self::list_runnable`]. It happens **here**, and deliberately not in
    /// [`Self::append_journal`]: a terminal record that was appended and never
    /// committed is an orphan, the run did not end, and a store that published
    /// the ending at append time would strand a live execution on the strength
    /// of a worker that died mid-boundary.
    ///
    /// Publishing it costs no extra read. Every implementation already has the
    /// journal in hand at this point — it is what the watermark is checked
    /// against.
    ///
    /// Durable implementations must make ending discovery and state publication
    /// one crash-safe transition. The filesystem store prepublishes a bounded
    /// current/proposed revision marker and refuses before snapshot CAS if that
    /// write fails; its readers select only the committed revision binding.
    /// In-memory storage updates both values under one mutex.
    async fn commit(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
    ) -> StoreResult<Revision>;

    /// Commit while proving the caller still holds a non-expired lease. Real
    /// durable stores override this so lease validation and mutation share one
    /// per-execution critical section; the default preserves compatibility for
    /// test/decorator stores while still rejecting an obviously stale token.
    async fn commit_fenced(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        lease: &Lease,
    ) -> StoreResult<Revision> {
        validate_presented_lease(key, lease)?;
        self.commit(key, state, expected).await
    }

    /// Append records, sweeping any orphaned attempt first, and return the seq of
    /// the last record placed.
    ///
    /// Sweeping is not optional — see the module docs. An implementation that
    /// appended after an orphan would let the next commit's watermark bury it.
    ///
    /// # This requires the caller to hold the lease, and nothing here checks it
    ///
    /// Unlike [`Self::commit`], which is a compare-and-swap and rejects a stale
    /// holder on its own, an append has no version to swap on: an append-only
    /// log's whole point is that it takes what it is given. Two holders
    /// appending to one journal interleave their seqs, and the damage surfaces
    /// as `JournalError::SeqRewind` on the next read — loudly, at the point of
    /// reading, naming the lease as the cause.
    ///
    /// That is the deliberate position: **detected, never silent.** Making the
    /// append itself compare-and-swap would mean one durable object per record,
    /// which trades the log for a directory and buys nothing the lease does not
    /// already provide.
    ///
    /// # One append per commit boundary, and this is a constraint not a habit
    ///
    /// The sweep is what makes it one. A second append issued before the commit
    /// that covers the first finds the first sitting above the watermark, cannot
    /// tell it from a dead worker's attempt, and sweeps it — silently, and with
    /// the numbering starting over. So a phase emits **every** record it produces
    /// in one call, which is why this takes a slice, and commits before it
    /// appends again.
    ///
    /// Distinguishing the two cases would need an attempt identifier in this API,
    /// which would put the lease's job in a second place and give a caller two
    /// ways to be wrong instead of one.
    async fn append_journal(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
    ) -> StoreResult<u64>;

    async fn append_journal_fenced(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
        lease: &Lease,
    ) -> StoreResult<u64> {
        validate_presented_lease(key, lease)?;
        self.append_journal(key, appends).await
    }

    /// Records from `from_seq` onward, orphans included.
    ///
    /// Orphans are included deliberately: this is the raw log, and deciding what
    /// is authoritative needs the watermark, which lives on the committed state.
    /// `Journal::authoritative` is where the two meet, and making the caller pass
    /// through it is what stops "read the journal" from quietly meaning "replay
    /// the journal".
    async fn read_journal(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
    ) -> StoreResult<Vec<JournalRecord>>;

    /// The whole log, checked, as a [`Journal`] — without copying it to get there.
    ///
    /// Every caller that wants a complete `Journal` rather than a verified
    /// replay/projection window goes through here. The hot phase-entry and
    /// outbox paths use the narrower methods below, allowing durable stores to
    /// answer from their integrity-checked tail index; the portable defaults
    /// still route those methods through this whole-log verifier.
    ///
    /// # The default is exactly what those callers used to write by hand
    ///
    /// `read_journal(key, 0)` then [`Journal::from_records`]. So a store that
    /// does not override this is checked precisely as it was before this method
    /// existed — which is what [`memory::MemoryLoopStateStore`] needs, because
    /// its `read_journal` hands back whatever is in its slot without checking
    /// anything at all.
    ///
    /// **That is why the check lives in the default rather than being deleted as
    /// redundant.** It is redundant only for a store whose own read already
    /// parses, and this is a `dyn` boundary: the caller cannot know which store
    /// it holds. `fs::FsLoopStateStore` overrides it precisely *because* it can
    /// make that claim about itself, and its override says so at the site.
    ///
    /// # `from_seq` is not a parameter, deliberately
    ///
    /// [`Journal::from_records`] requires a log that starts at seq one — a
    /// partial log cannot be checked for a seq break — so `Journal` is a whole-log
    /// type and a `from_seq` here would be an argument whose only valid value is
    /// zero. Callers that want a suffix take it off the built journal
    /// ([`Journal::authoritative_from`]), where the check has already run.
    async fn read_journal_verified(&self, key: &ExecutionKey) -> StoreResult<Journal> {
        Ok(Journal::from_records(self.read_journal(key, 0).await?)?)
    }

    /// Replay the exact committed prefix. Durable stores may answer from an
    /// integrity-bound journal index when it still names the current file
    /// generation; the portable default retains whole-log verification.
    async fn replay_committed_journal(
        &self,
        key: &ExecutionKey,
        watermark: u64,
    ) -> StoreResult<ReplayedCursor> {
        let journal = self.read_journal_verified(key).await?;
        if journal.last_seq() < watermark {
            return Err(StoreError::WatermarkAhead {
                watermark,
                last_seq: journal.last_seq(),
            });
        }
        Ok(replay_journal(journal.authoritative(watermark))?)
    }

    /// Return a checked authoritative window for outbox projection.
    ///
    /// The default returns the whole prefix. Filesystem storage overrides this
    /// with a bounded tail read when its durable index can prove the byte offset
    /// of the append batch containing `from_seq`.
    async fn read_journal_projection(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
        watermark: u64,
        _require_complete_history: bool,
    ) -> StoreResult<Vec<JournalRecord>> {
        let journal = self.read_journal_verified(key).await?;
        if journal.last_seq() < watermark {
            return Err(StoreError::WatermarkAhead {
                watermark,
                last_seq: journal.last_seq(),
            });
        }
        let mut records = journal.into_records();
        records.truncate(records.partition_point(|record| record.seq <= watermark));
        let _ = from_seq;
        Ok(records)
    }

    /// Record that an effect is about to fire, before it fires.
    async fn record_effect_intent(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
    ) -> StoreResult<()>;

    async fn record_effect_intent_fenced(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
        lease: &Lease,
    ) -> StoreResult<()> {
        validate_presented_lease(key, lease)?;
        self.record_effect_intent(key, entry).await
    }

    /// Record what happened to an effect that already had an intent.
    async fn record_effect_outcome(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> StoreResult<()>;

    async fn record_effect_outcome_fenced(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
        lease: &Lease,
    ) -> StoreResult<()> {
        validate_presented_lease(key, lease)?;
        self.record_effect_outcome(key, effect_id, outcome).await
    }

    /// This run's whole effect status map.
    async fn load_effects(&self, key: &ExecutionKey) -> StoreResult<EffectLedger>;

    /// Take the right to advance this execution for `ttl`.
    ///
    /// Any unreleased, unexpired lease refuses the claim, even when `worker`
    /// equals its holder. A worker id is not a lease capability; [`Self::renew`]
    /// is the only extension path because it requires the current fence.
    async fn claim(
        &self,
        key: &ExecutionKey,
        worker: &WorkerId,
        ttl: Duration,
    ) -> StoreResult<Lease>;

    /// Extend a lease this worker still holds, producing a new fence.
    async fn renew(&self, lease: &Lease, ttl: Duration) -> StoreResult<Lease>;

    /// Give a lease up early.
    async fn release(&self, lease: Lease) -> StoreResult<()>;

    /// Mark a parked execution's wait as satisfied, once, by one completion.
    ///
    /// Called by whoever completes the thing being waited on — a job runner, a
    /// child execution — which does not hold the parent's lease. It writes a
    /// resolution rather than mutating the parent, so waking cannot lose a
    /// compare-and-swap race against the parent's own commit.
    ///
    /// # Why a resolution id, and why the completer has to supply it
    ///
    /// Because only the completer can tell **one event delivered twice** from
    /// **two events**. A retried job runner reporting the same completion again
    /// must be idempotent; a child that reports a second time is a second thing
    /// to wake for. The token cannot distinguish them — it is `job:<id>` or
    /// `children:<sorted ids>` and is identical in both cases — so the resolution
    /// id is the completer's own event identity, and this call is idempotent on
    /// `(wake_token, resolution_id)`.
    ///
    /// # A resolution is consumed, and the order is load-bearing
    ///
    /// A parked execution is runnable when **any** unconsumed resolution names
    /// its token. Leaving a park is three steps, in this order:
    ///
    /// 1. [`Self::wake_resolutions`] — the ids this run is about to act on.
    /// 2. `commit` the state with `wait` cleared.
    /// 3. [`Self::consume_wake`] with exactly those ids.
    ///
    /// **Commit before consuming, never the reverse.** A crash between 1 and 2
    /// removes nothing, so the park is intact and is simply retried. A crash
    /// between 2 and 3 leaves the resolutions outstanding, so the *next* park on
    /// that token is satisfied once without a fresh completion — the run takes
    /// one extra round rather than waiting. Consuming first would invert that
    /// into a lost wake and a run parked forever, which is strictly worse: a
    /// spin is bounded and observable, a hang is neither.
    ///
    /// Passing the exact ids, rather than "clear this token", closes the third
    /// race: a completion landing between steps 1 and 2 is not in the list, so it
    /// survives step 3 and satisfies the next park.
    ///
    /// # What this replaces
    ///
    /// A boolean marker that nothing ever removed. The consequence was that a
    /// second park on one token was never honoured — an execution parking twice
    /// on one job id was runnable immediately the second time and would spin —
    /// and re-parking on the same job is the ordinary shape of a run waiting on a
    /// child that reports more than once. The reasoning for leaving it was that a
    /// completer may resolve **before** the parent commits its park, so any
    /// cleanup of "a resolution nothing is waiting on" would delete exactly the
    /// resolution that arrived early. That is true of a cleanup driven by the
    /// *store*, which cannot know what is waiting. It is not true of one driven
    /// by the parked run itself, which knows precisely what it acted on — and
    /// that run already holds the lease and is already committing, so this costs
    /// no extra read on any commit that is not leaving a park.
    async fn resolve_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_id: &str,
    ) -> StoreResult<()>;

    /// The resolution ids outstanding for one token, in a stable order.
    ///
    /// Empty means the park is not satisfied. Step 1 of the sequence on
    /// [`Self::resolve_wake`].
    async fn wake_resolutions(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<String>>;

    /// Drop exactly these resolutions, returning how many were there to drop.
    ///
    /// Step 3, and only ever after the commit that left the park. An id that is
    /// already gone is not an error — a repeated consume is how a retried
    /// boundary behaves — which is why this returns a count rather than
    /// refusing.
    async fn consume_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
    ) -> StoreResult<usize>;

    async fn consume_wake_fenced(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
        lease: &Lease,
    ) -> StoreResult<usize> {
        validate_presented_lease(key, lease)?;
        self.consume_wake(key, wake_token, resolution_ids).await
    }

    /// Executions this worker may pick up now.
    ///
    /// Portable work plus the pinned work addressed to this worker, minus
    /// anything leased, parked without a resolved wake, not yet at its retry
    /// time, or **already ended for good**. Deadline-expired unparked work is
    /// deliberately included so a worker can durably retire it; otherwise it is
    /// invisible to both this scheduler and the parked-run reconciler.
    ///
    /// # A run whose journal records a non-resumable terminal is not offered
    ///
    /// This is the one filter that is not a property of [`LoopState`], and it is
    /// required of every implementation. A terminal lives in the journal, so
    /// until this rule existed a run that completed an hour ago was still
    /// claimable, unleased, unparked and inside every timer — offered by every
    /// scan forever, refused by every claim, and occupying a slot in a `limit`
    /// that live work was queued behind.
    ///
    /// **It must not cost a journal read per key.** An implementation that
    /// opened each run's log inside this call would turn a directory walk into
    /// an `O(records)` read per key on the poll path, which is the shape the
    /// design rules out. The terminal is instead published by the [`Self::commit`]
    /// that made it authoritative — see [`EndedRun`] — so this call reads a
    /// value, never a log.
    ///
    /// # Receipt ownership also withholds a resumable source segment
    ///
    /// `WaitingForUser`, `WaitingForConfirmation`, `PausedByUser`,
    /// `WaitingForChildren` and `Sleeping` end the invocation and leave the
    /// execution alive. A receipt-backed user/manual pause is nevertheless
    /// withheld: its immutable source segment belongs to the terminal lifecycle,
    /// which first converges runtime + Artifact state and then publishes the
    /// separately staged successor authority. Re-running its ending phase would
    /// duplicate effects. Receipt-less legacy resumables and dedicated
    /// WaitingChildren/Sleeping owners retain the ordinary runnable behavior.
    /// [`non_resumable_terminal`] remains the journal-only ending distinction;
    /// implementations must additionally recognize an exact receipt bound to the
    /// current segment and watermark without opening the journal.
    ///
    /// # What this does NOT remove from the walk
    ///
    /// A quarantined run, one past its iteration ceiling, and one holding an
    /// indeterminate effect are all still offered, because every one of those is
    /// judged against a threshold the store does not hold. They are the reason
    /// [`Self::scan_runnable`] exists.
    ///
    /// Production's exact-key execution path does not use global runnable
    /// discovery. The boot lifecycle enumerates canonical runtime executions,
    /// then composes Artifact roots, integrity-sealed delegated children, or
    /// durable PlanGraph roots through their owning lifecycle. Missing durable
    /// composition fails closed. [`Self::scan_runnable`] remains the bounded
    /// discovery API for callers that already possess exact hosts; this
    /// compatibility listing remains for callers that explicitly accept one
    /// prefix-sized page. A store row alone is never host authority.
    ///
    /// # Pinned work is withheld from other workers for as long as its pin lasts
    ///
    /// [`LoopState::claimable_by`](super::state::LoopState::claimable_by) is one
    /// of the filters, so a run pinned to a worker that never comes back is
    /// offered to nobody — **until its pin lapses**, which it does on its own
    /// within `PIN_TTL_MS` of the last commit that renewed it. Nothing here
    /// sweeps it and nothing surfaces it in the meantime, and `deadline_at_ms`
    /// does not free it either: that deadline is checked at *phase entry*, and
    /// nothing enters a phase it cannot claim. So an operator asking "why is
    /// this run doing nothing" inside that window will find it committed,
    /// unleased, unparked, inside every timer, and still not offered — and then
    /// find it running again without having done anything.
    ///
    /// An earlier version of this paragraph said *permanently*, which was true
    /// of the unbounded pin it was written against and is the defect the expiry
    /// closed. The window is now bounded rather than absent, which is a
    /// different thing from fixed: it is still a period where the honest answer
    /// to "why is this stalled" is not visible from this scan. That is a
    /// property of the placement rule rather than of this listing, and
    /// [`LoopState::claimable_by`](super::state::LoopState::claimable_by) is
    /// where it is argued. It is repeated here because this is where an operator
    /// looks first.
    async fn list_runnable(
        &self,
        worker: &WorkerId,
        limit: usize,
    ) -> StoreResult<Vec<ExecutionKey>>;

    /// One bounded page of [`Self::list_runnable`], resumable past the last
    /// execution it inspected. `limit` bounds matches returned; implementations
    /// additionally cap directories visited so a sparse page cannot become a
    /// whole-store scan.
    ///
    /// # WorkerRunner pages with this; production recovery is lifecycle-owned
    ///
    /// [`super::worker_runner::WorkerRunner`] uses this cursor and bounds pages
    /// per sweep. Production boot does not derive a host from this scan: it
    /// discovers runtime rows in the canonical execution store, validates their
    /// Artifact/PlanGraph/sealed-child composition, and re-enters the ordinary
    /// executor under the exact id. The loop store still supplies dedicated
    /// terminal-debt and parked-run scans. This API is therefore a reusable
    /// scheduler primitive, not an unwired production recovery promise.
    ///
    /// # The gap this closes, stated as the failure it caused
    ///
    /// [`Self::list_runnable`] takes a `limit` and nothing else, so every caller
    /// sees the first `limit` offerable keys of the walk and can never see past
    /// them. Most keys leave that prefix on their own — they get claimed, they
    /// park, they back off. Some never do: a run that is **quarantined**, one
    /// past its **iteration ceiling**, one holding an **indeterminate effect**
    /// answers a permanent refusal at claim time and commits nothing, so it is
    /// offered again, unchanged, by every later scan. `limit` such keys at the
    /// head of the walk starve every live execution behind them, permanently,
    /// while the scan reports a full page and looks healthy.
    ///
    /// The ended-run half of that population is now withheld by
    /// [`Self::list_runnable`] itself. The three above are **not**, and cannot
    /// be: `PhaseAttemptsExhausted` is measured against the driver's
    /// `max_phase_attempts`, the iteration ceiling against the *host's*
    /// `max_iterations`, and an indeterminate effect against a ledger read the
    /// scan does not do. None of those thresholds is knowable from the store, so
    /// no filter here can remove them and paging is the only way past.
    ///
    /// # The contract on the cursor
    ///
    /// - `after: None` starts at the beginning of the walk.
    /// - `after: Some(c)` resumes **strictly after** the position `c` names, and
    ///   `c` must be a [`RunnableScan::resume`] this same store produced.
    /// - [`RunnableScan::resume`] is `Some` whenever the walk stopped short of
    ///   the end for any reason, and `None` only when it truly reached the end.
    /// - Paging with each page's `resume` visits every execution the store holds
    ///   at most once per pass. A run committed *behind* the cursor mid-pass is
    ///   missed by that pass and picked up by the next one, which is the ordinary
    ///   property of a cursor over a live tree and is why a caller pages in a
    ///   loop rather than once.
    ///
    /// # Why the default refuses instead of answering one page
    ///
    /// The same reason [`Self::load_projector_cursor`]'s default refuses. A
    /// default that accepted `after` and ignored it would hand back page one
    /// forever, and a caller paging in a loop would spin on the same prefix it
    /// was trying to escape — the exact defect this method exists to fix, made
    /// invisible. A default that answered `resume: None` after stopping at
    /// `limit` would be worse still: it would claim the store had been walked to
    /// the end when the tail had not been looked at.
    ///
    /// So an implementation that has not built this says so, and a caller that
    /// needs the unpaged listing calls [`Self::list_runnable`], which every
    /// store has.
    ///
    /// # The delegation that keeps the two answers together is a property of
    /// the two REAL stores, not of the trait
    ///
    /// `fs` and `memory` both write [`Self::list_runnable`] as a call into this
    /// one, and `the_paged_scan_and_the_flat_listing_answer_the_same_question`
    /// is what stops a later editor re-forking them. A **wrapper** store gets no
    /// such guarantee: the test decorators in `worker_runner`, `driver_worker`
    /// and `reconciler` forward `list_runnable` to an inner store and inherit
    /// the refusal above for this method, so through one of them the two entry
    /// points answer different things. Harmless while nothing calls this, and
    /// worth knowing before a decorator is put on a path that does — a
    /// decorator has to forward both or it is not the store it is wrapping.
    async fn scan_runnable(
        &self,
        _worker: &WorkerId,
        _limit: usize,
        _after: Option<&ScanCursor>,
    ) -> StoreResult<RunnableScan> {
        Err(StoreError::Unavailable {
            detail: "this store does not implement the cursored scan, so it cannot offer work \
                     past the first page of its walk"
                .to_string(),
        })
    }

    /// Scan, independently of execution placement, for terminal lifecycle debt.
    ///
    /// This is a projection lifecycle call, not scheduling. Implementations
    /// therefore ignore pins, runnable time, waits, phase placement and leases
    /// while discovering candidates. A projector must still claim each exact
    /// key and re-read its committed state and journal under that lease before
    /// emitting anything. Debt is an accepted event above the event cursor, an
    /// runtime-backed CannotProceed watermark, a receipt-backed terminal
    /// (including resumable HITL), or any committed
    /// operator-steer consume receipt.
    /// Receipt-less ordinary endings are a rolling-upgrade legacy shape: their
    /// journal kind cannot reconstruct the old runtime/Artifact outcome, so
    /// implementations project their events without manufacturing settlement
    /// debt that no exact owner could ever discharge.
    /// The last category is intentionally independent of an ending marker: an
    /// external cancellation can close the runtime before the loop publishes a
    /// terminal, while the already-committed receipt still must be ACKed and
    /// fenced-cleared.
    ///
    /// `max_visits` bounds entries examined rather than matches returned. The
    /// cursor is required because a sparse debt scan that restarted at the
    /// beginning on every cadence could starve a terminal in the tail forever.
    async fn scan_terminal_outbox_debt(
        &self,
        _max_visits: NonZeroUsize,
        _after: Option<&ScanCursor>,
    ) -> StoreResult<TerminalOutboxScan> {
        Err(StoreError::Unavailable {
            detail: "this store does not implement the bounded terminal-outbox debt scan"
                .to_string(),
        })
    }

    /// Every execution whose committed state is parked. **Not a scheduling
    /// call.**
    ///
    /// [`Self::list_runnable`] answers *what may I run*, and its whole shape is
    /// built around being cheap enough for a poll — the wake index exists so
    /// that question costs `O(ready)` rather than `O(parked)`. This answers a
    /// different question, *what is stuck*, and it is the counterpart the design
    /// asks for by name: **"a reconciler detects orphaned parks rather than a
    /// per-poll scan"**
    /// (`docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Error handling*).
    ///
    /// # What it deliberately does NOT do
    ///
    /// It does not consult the wake ledger, and it does not judge whether a park
    /// is healthy. Both are the reconciler's job, and both are the expensive
    /// half — a store that filtered here would have to read one wake directory
    /// per parked execution on every call, which is the per-poll scan the design
    /// rules out, merely moved behind a different method name.
    ///
    /// So a park whose wake has already resolved is still listed. That is not an
    /// oversight: a resolved park that nothing has picked up is itself a stall
    /// worth surfacing, and a listing that hid it would make the one case where
    /// the scheduler and the store disagree invisible.
    ///
    /// # Cost, stated rather than implied
    ///
    /// Both implementations visit **every** execution, because neither keeps a
    /// parked index. That is the same walk `list_runnable` already performs, and
    /// it is acceptable here for a reason it would not be on the poll path: this
    /// is called on a reconciliation cadence measured in minutes, not on every
    /// claim. An index would make it `O(parked)`, and the reason there is not one
    /// is written up at [`super::reconciler`] — briefly, an index has to be
    /// written on the commit path, and a commit path that can fail for a reason
    /// the run does not care about is a new way to strand a run.
    ///
    /// `limit` bounds the answer, and filling it sets
    /// [`ParkedListing::incomplete`] — because for this caller, unlike for a
    /// scheduler, an unexamined tail is a coverage hole rather than simply less
    /// work taken.
    ///
    /// # Why this one has a default and the rest do not
    ///
    /// Every other method here is required, because a store that cannot commit
    /// or cannot lease is not a store. This one is different: an implementation
    /// that cannot enumerate parks is still a perfectly good store, it simply
    /// cannot be reconciled — and the honest answer to *what is stuck* from such
    /// a store is **"I did not look"**, not "nothing".
    ///
    /// So the default returns an empty listing with
    /// [`ParkedListing::incomplete`] set, and that flag is load-bearing rather
    /// than decorative: it travels into
    /// [`super::reconciler::ReconcileReport::incomplete`], whose documentation
    /// forbids reading an empty `findings` as a clean bill of health while it is
    /// set. A default returning `incomplete: false` would be the dangerous
    /// version — a new store would report *no orphaned parks* forever and
    /// nothing would ever notice.
    ///
    /// The real implementations override it, and the contract case
    /// `list_parked_offers_parked_work_and_nothing_else` is what stops one of
    /// them from silently falling back here.
    async fn list_parked(&self, _limit: usize) -> StoreResult<ParkedListing> {
        Ok(ParkedListing {
            parked: Vec::new(),
            resume: None,
            incomplete: true,
        })
    }

    /// One ordered page of parked executions, resuming strictly after `after`.
    ///
    /// The conservative default supports only the first page through
    /// [`Self::list_parked`]. Accepting and ignoring a cursor would repeat page
    /// one forever, so wrappers and new stores must opt in explicitly.
    async fn scan_parked(
        &self,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<ParkedListing> {
        if after.is_some() {
            return Err(StoreError::Unavailable {
                detail: "this store does not implement the cursored parked scan".to_string(),
            });
        }
        self.list_parked(limit).await
    }

    /// How far this execution's event outbox has been emitted.
    ///
    /// `Ok(None)` means *nothing has ever projected this run* — a legitimate
    /// answer, and the one a fresh execution gives. It is not the same as an
    /// error, and the two must stay apart: a projector handed `None` starts at
    /// seq zero with an empty dedupe window and re-emits every authoritative
    /// event it finds, which is the right thing to do for a run that has emitted
    /// nothing and exactly the wrong thing to do for a run whose mark could not
    /// be read.
    ///
    /// # Why the mark is here and not on [`LoopState`]
    ///
    /// [`ProjectorCursor`]'s own docs argue it, and the short form is two
    /// things: a full dedupe window is roughly a hundred kilobytes riding a
    /// record that is otherwise about one, committed six times an iteration; and
    /// the mark is only knowable *after* the commit that made its records
    /// authoritative, so riding `LoopState` would mean a second
    /// compare-and-swap per boundary whose refusal would re-emit a boundary's
    /// events for a reason unrelated to the outbox.
    ///
    /// # The default refuses rather than answering `None`
    ///
    /// An implementation that has not built this cannot say *nothing has
    /// projected yet* — it does not know. Answering `Ok(None)` would make a
    /// projector re-emit the whole run's events on every boundary, forever, with
    /// nothing in any log saying why. So the default is an error, the caller
    /// declines to project on it, and the records stay in the journal.
    async fn load_projector_cursor(
        &self,
        _key: &ExecutionKey,
    ) -> StoreResult<Option<ProjectorCursor>> {
        Err(StoreError::Unavailable {
            detail: "this store does not implement the projector mark, so it cannot say \
                     how much of the event outbox has been emitted"
                .to_string(),
        })
    }

    /// Publish the mark, after the events beneath it have been emitted.
    ///
    /// **Last-writer-wins under the lease, deliberately not a compare-and-swap.**
    /// [`Self::commit`] is a CAS because two workers publishing a state must not
    /// both believe they won. This value is different: it is written by whoever
    /// just emitted, it only ever moves forward within one execution, and a
    /// refusal here would be a refusal to record work that has *already reached
    /// the transports* — which converts a bounded duplicate into an unbounded
    /// one on the next pass.
    ///
    /// # A failure here must not fail the boundary
    ///
    /// The commit this follows has already landed and the events have already
    /// been emitted. A caller that turned this error into a refused boundary
    /// would hold a run hostage to its outbox; the correct response is to log
    /// and carry on, and pay for it with one boundary's re-emission after a
    /// restart. `driver_worker::project_outbox` is where that is written down.
    ///
    /// The default refuses for the same reason [`Self::load_projector_cursor`]'s
    /// does: an implementation that stored nothing and answered `Ok(())` would
    /// tell its caller the mark is durable when it is not.
    async fn save_projector_cursor(
        &self,
        _key: &ExecutionKey,
        _cursor: &ProjectorCursor,
    ) -> StoreResult<()> {
        Err(StoreError::Unavailable {
            detail: "this store does not implement the projector mark, so an emitted event cannot \
                     be recorded as emitted"
                .to_string(),
        })
    }

    async fn save_projector_cursor_fenced(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
        lease: &Lease,
    ) -> StoreResult<()> {
        validate_presented_lease(key, lease)?;
        self.save_projector_cursor(key, cursor).await
    }

    /// The receipt saying this segment closed its chain, if one was published.
    ///
    /// `Ok(None)` is the ordinary answer for a run that is still going, for one
    /// that died before it could publish, and for a store that publishes no
    /// receipts at all. Collapsing those three is deliberate and is the reason
    /// this default is `Ok(None)` where [`Self::load_projector_cursor`]'s is an
    /// error: there, answering "nothing yet" for "I cannot say" makes a
    /// projector re-emit a whole run's timeline, and here every one of the three
    /// leads a reader to the same place — **refuse to prove anything**. A
    /// receipt is evidence by existing, so the absent case has only one safe
    /// reading and there is nothing for an error to protect.
    ///
    /// See [`ChainClosure`] for what the presence of one licenses.
    async fn load_chain_closure(&self, _key: &ExecutionKey) -> StoreResult<Option<ChainClosure>> {
        Ok(None)
    }

    /// Publish the receipt, after the segment's terminal has been committed.
    ///
    /// Called once, by the writer that decided no further segment follows. Not
    /// fenced and not a compare-and-swap: the run has ended, so there is no
    /// lease left to present and nothing for two writers to disagree about —
    /// every writer of this value for one segment is writing the same value.
    ///
    /// # A failure here must not fail anything
    ///
    /// The run is over and its outcome is already the caller's to return. The
    /// only thing an unpublished receipt costs is that a reconciler will decline
    /// to prove a park about this child, which is the direction it declines in
    /// anyway when it has no receipt.
    ///
    /// The default is an **error**, not a silent `Ok(())`, for the reason
    /// [`Self::save_projector_cursor`]'s is: a store that recorded nothing and
    /// answered success would tell its caller the receipt is durable when it is
    /// not, and the caller logs an error it can act on rather than believing a
    /// proof exists somewhere it does not.
    async fn record_chain_closure(
        &self,
        _key: &ExecutionKey,
        _closure: &ChainClosure,
    ) -> StoreResult<()> {
        Err(StoreError::Unavailable {
            detail: "this store does not implement chain-closure receipts, so a finished \
                     segment cannot record that nothing follows it"
                .to_string(),
        })
    }
}

fn validate_presented_lease(key: &ExecutionKey, lease: &Lease) -> StoreResult<()> {
    if &lease.key != key || lease.is_expired_at(Utc::now().timestamp_millis()) {
        return Err(StoreError::LeaseLost { fence: lease.fence });
    }
    Ok(())
}

// ============================================================================
// The contract suite
// ============================================================================

/// One suite, run against every implementation.
///
/// The design asks for this so *"the object-store impl is proven against the
/// same contract as the filesystem one"*. Adding an implementation is one line:
/// write a harness and invoke [`loop_state_store_contract`].
///
/// Every case below is a property a **driver** depends on, written from the
/// failure it prevents rather than from the method it calls. A suite that only
/// asserted "what I put in comes out" would pass on an implementation with no
/// compare-and-swap, no lease exclusion and no orphan sweep — which is to say on
/// an implementation that cannot support a second worker at all.
///
/// # What this suite does not prove, and where that is proven instead
///
/// - **Durability.** Every case runs against the in-memory store as well, and
///   that store has none, so by construction no case here can be a durability
///   test — *including the ones named for a crash*. "Crash" here means the only
///   thing a store can observe: a worker appended and never committed. Surviving
///   a process is what [`ContractHarness::reopen`] reaches, and what `fs`'s own
///   tests check against real bytes.
/// - **Atomicity under real concurrency.** The cases are single-threaded, so an
///   implementation whose compare-and-swap was a read followed by a write, with a
///   window in between, would pass all of them. The filesystem store closes that
///   window with `link(2)` and asserts the primitive directly; a contract written
///   over a trait can only assert the outcome a caller is entitled to see.
/// - **The limits that exist because a file has to be read back.** Byte ceilings
///   on a state, a lease or a key record are filesystem concerns and are not
///   asserted here — see `memory`'s module docs, which say which rules the two
///   implementations deliberately do not share.
#[cfg(test)]
pub mod contract {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::effects::{
        BatchMode, CommittedActRef, EffectDisposition, PendingBatch, PendingEffect, RetrySafety,
    };
    use crate::magician_v2::execution::agentic::run_loop::journal::{
        replay, EmitRefused, EventKey, Journal, JournalBody, ProjectedEventSink, RecordedBoundary,
        RecordedCanonicalScope, RecordedEventRouting, RecordedStep, TerminalKind,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
    use crate::magician_v2::execution::agentic::run_loop::state::{
        LoopCursor, Placement, RunIdentity, WaitReason,
    };

    /// What a contract run needs: a store, and whatever must outlive it.
    ///
    /// The associated `Store` rather than a bare constructor because the
    /// filesystem harness owns a temporary directory that the store only borrows
    /// a path into; a factory returning the store alone would drop the directory
    /// first and every case would fail on a path that no longer exists.
    pub trait ContractHarness: Send + Sync {
        type Store: LoopStateStore;

        /// Deliberately not `async`. Every harness builds synchronously — a
        /// temporary directory, a map — and an async associated function with no
        /// receiver is a shape `async_trait` supports awkwardly for no gain here.
        fn create() -> Self;

        fn store(&self) -> &Self::Store;

        /// A second, independently constructed handle onto the same substrate.
        ///
        /// This is the one thing a single handle cannot check. Every other case
        /// would pass on an implementation that answered from a process-local
        /// cache the substrate never saw — which is precisely what the filesystem
        /// store claims not to have ("there is no in-memory index") and precisely
        /// what a second worker, or the same worker after a restart, would find
        /// missing.
        fn reopen(&self) -> Self::Store;
    }

    pub fn key(execution_id: &str) -> ExecutionKey {
        ExecutionKey::new("owner", "default", execution_id).expect("a fixture key is well-formed")
    }

    pub fn fresh_state(key: &ExecutionKey) -> LoopState {
        LoopState::new(RunIdentity {
            execution_id: Some(key.execution_id().to_string()),
            principal: Some(key.principal().to_string()),
            workspace: Some(key.workspace().to_string()),
            // Cdp-only, because `LoopState::new` starts a run `Portable` and a
            // portable run with an empty (therefore UNRESTRICTED) ceiling is a
            // state `LoopState::for_commit` refuses to project. The store does
            // not know that invariant and must not, but a suite whose every
            // fixture was a state no driver could publish would be exercising
            // shapes the system never produces.
            browser_transports: vec!["cdp".to_string()],
            ..RunIdentity::default()
        })
    }

    fn unsafe_effect(id: &str) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id is well-formed"),
            tool: "gmail__send".to_string(),
            arguments_fingerprint: "fp-1".to_string(),
            retry_safety: RetrySafety::NotRetrySafe,
            // A send is outward, so it carries the act ref a worker reconciles
            // against instead of re-deriving one — which is what stops a
            // byte-different re-serialisation reading as "nothing left" — and
            // the scope that ref was derived under, without which the pickup
            // addresses a directory the ref could never name.
            reconcile_ref: Some(
                CommittedActRef::new(
                    format!("act-{}", "0123456789abcdef".repeat(2)),
                    "anonymous",
                    "default",
                )
                .expect("the fixture ref must be the shape derive_act_ref mints"),
            ),
            // A send is not reattachable, so nothing names a job to resume.
            reattach_ref: None,
        }
    }

    pub async fn an_execution_that_never_committed_loads_as_nothing<S: LoopStateStore>(store: &S) {
        let key = key("never-committed");
        assert!(store
            .load(&key)
            .await
            .expect("a load must answer")
            .is_none());
        assert!(store
            .read_journal(&key, 0)
            .await
            .expect("an absent journal is empty, not an error")
            .is_empty());
        assert!(store
            .load_effects(&key)
            .await
            .expect("an absent ledger is empty, not an error")
            .is_empty());
    }

    pub async fn a_commit_publishes_a_revision_a_load_reads_back<S: LoopStateStore>(store: &S) {
        let key = key("commit-roundtrip");
        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 3,
            phase: Phase::Decide,
        };
        state.work_budget_consumed_ms = 4_200;

        let revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("the first commit uses the initial revision");
        assert_eq!(revision, Revision::INITIAL.next());

        let loaded = store
            .load(&key)
            .await
            .expect("load")
            .expect("a committed execution loads");
        assert_eq!(loaded.revision, revision);
        assert_eq!(loaded.state, state);
    }

    pub async fn a_stale_holder_cannot_clobber_a_newer_commit<S: LoopStateStore>(store: &S) {
        // The failure: a worker whose lease expired finishes its phase and
        // commits over the work of the worker that took over. Compare-and-swap
        // is what stops it, and it has to stop it on a value the stale worker
        // cannot refresh.
        let key = key("cas");
        let state = fresh_state(&key);
        let first = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("first commit");
        let second = store
            .commit(&key, &state, first)
            .await
            .expect("second commit");

        let error = store
            .commit(&key, &state, first)
            .await
            .expect_err("a commit against a superseded revision must be refused");
        match error {
            StoreError::Conflict { expected, found } => {
                assert_eq!(expected, first);
                assert_eq!(found, second);
            },
            other => panic!("expected a conflict, got {other}"),
        }

        // And the refusal left the store where it was.
        let loaded = store.load(&key).await.expect("load").expect("present");
        assert_eq!(loaded.revision, second);
    }

    pub async fn a_second_first_commit_is_a_conflict_not_an_overwrite<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("double-first");
        let state = fresh_state(&key);
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("first commit");
        let error = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("two workers both believing they are first must not both win");
        assert!(matches!(error, StoreError::Conflict { .. }), "got {error}");
    }

    pub async fn the_store_numbers_journal_records_gaplessly<S: LoopStateStore>(store: &S) {
        // The commit between the two appends is load-bearing, and its absence is
        // what this case originally got wrong: with the watermark still at zero,
        // the second append finds the first attempt's records above it, cannot
        // tell them from a dead worker's, and sweeps them — so the numbering it
        // meant to assert restarted at one. That is the sweep rule working, and
        // it is asserted next door. Here the watermark keeps up, which is the
        // shape a driver is required to use: see `append_journal`'s contract on
        // one append per commit boundary.
        let key = key("journal-seq");
        let mut state = fresh_state(&key);
        let last = store
            .append_journal(
                &key,
                &[
                    JournalAppend::phase_completed(1, Phase::Prepare, RecordedStep::Continued),
                    JournalAppend::phase_completed(1, Phase::Observe, RecordedStep::Continued),
                ],
            )
            .await
            .expect("append");
        assert_eq!(last, 2);

        state.journal_seq = 2;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Decide,
        };
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("the boundary commit the next append is numbered from");

        let last = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Decide,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        assert_eq!(
            last, 3,
            "the next attempt continues the numbering it committed"
        );

        let records = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        // Gaplessness is the invariant `Journal` checks, so building one from
        // what the store returned is the assertion.
        Journal::from_records(records.clone()).expect("the store's records must be well-formed");

        let tail = store.read_journal(&key, 3).await.expect("read");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 3);
    }

    pub async fn an_orphaned_attempt_is_swept_before_the_next_one_appends<S: LoopStateStore>(
        store: &S,
    ) {
        // The bug this prevents is subtle and would be invisible without it.
        // Worker A appends records 3 and 4 and dies before committing; the
        // watermark is still 2. Worker B re-runs the phase and appends. If its
        // records land at 5 and 6, the commit that follows sets the watermark to
        // 6 — which now covers the orphans at 3 and 4, and replay applies work
        // nobody committed.
        let key = key("orphan-sweep");
        let mut state = fresh_state(&key);

        store
            .append_journal(
                &key,
                &[
                    JournalAppend::phase_completed(1, Phase::Prepare, RecordedStep::Continued),
                    JournalAppend::phase_completed(1, Phase::Observe, RecordedStep::Continued),
                ],
            )
            .await
            .expect("append");
        state.journal_seq = 2;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Decide,
        };
        let revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit the watermark");

        // Worker A's attempt: appended, never committed.
        store
            .append_journal(
                &key,
                &[
                    JournalAppend::phase_completed(1, Phase::Decide, RecordedStep::Continued),
                    JournalAppend::phase_completed(1, Phase::Resolve, RecordedStep::Continued),
                ],
            )
            .await
            .expect("append");

        // Worker B re-runs the same phase.
        let last = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Decide,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        assert_eq!(
            last, 3,
            "the re-run's record must take the orphan's seq, not sit after it"
        );

        let records = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(
            records.len(),
            3,
            "the orphaned attempt is gone from the log"
        );

        state.journal_seq = 3;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Resolve,
        };
        store
            .commit(&key, &state, revision)
            .await
            .expect("commit after the re-run");

        let records = store.read_journal(&key, 0).await.expect("read");
        let journal = Journal::from_records(records).expect("well-formed");
        let cursor = crate::magician_v2::execution::agentic::run_loop::journal::replay(
            journal.authoritative(3),
        )
        .expect("replay");
        assert_eq!(cursor.phase, Phase::Resolve);
        assert_eq!(cursor.iteration, 1);
    }

    pub async fn replay_of_the_committed_prefix_reaches_the_committed_cursor<S: LoopStateStore>(
        store: &S,
    ) {
        // Replay determinism, asserted through a store rather than over a
        // hand-built vector: the cursor the journal reconstructs must equal the
        // cursor the commit recorded, at every seq.
        let key = key("replay");
        let mut state = fresh_state(&key);
        let mut revision = Revision::INITIAL;

        let steps = [
            (Phase::Prepare, RecordedStep::Continued, Phase::Observe),
            (Phase::Observe, RecordedStep::Continued, Phase::Decide),
            (Phase::Decide, RecordedStep::Continued, Phase::Resolve),
            (Phase::Resolve, RecordedStep::Continued, Phase::Apply),
            (
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
                Phase::Epilogue,
            ),
        ];

        for (phase, step, next) in steps {
            let seq = store
                .append_journal(&key, &[JournalAppend::phase_completed(1, phase, step)])
                .await
                .expect("append");
            state.journal_seq = seq;
            state.cursor = LoopCursor {
                iteration: 1,
                phase: next,
            };
            revision = store.commit(&key, &state, revision).await.expect("commit");

            let loaded = store.load(&key).await.expect("load").expect("present");
            let records = store.read_journal(&key, 0).await.expect("read");
            let journal = Journal::from_records(records).expect("well-formed");
            let replayed = crate::magician_v2::execution::agentic::run_loop::journal::replay(
                journal.authoritative(loaded.state.journal_seq),
            )
            .expect("replay");
            assert_eq!(
                (replayed.iteration, replayed.phase),
                (loaded.state.cursor.iteration, loaded.state.cursor.phase),
                "the journal must reproduce the committed cursor at seq {seq}"
            );
        }
    }

    pub async fn a_crash_at_every_commit_boundary_resumes_at_the_same_cursor<S: LoopStateStore>(
        store: &S,
    ) {
        // The crash-injection case the design asks for, at the grain it asks for
        // it: *"kill at each commit boundary; assert resume produces the same
        // terminal outcome"*. Every one of the six phases is killed at, not a
        // representative one — the boundaries differ (the epilogue is the only
        // place the iteration counter steps; an exit lands on the epilogue
        // rather than on the next phase) and a bug that shows at only one of
        // them is exactly what a sampled test misses.
        //
        // "Killing" a worker here means what it means in production: it appended
        // records and never committed. The store keeps them, the watermark does
        // not cover them, and the next attempt sweeps them.
        let key = key("crash-boundaries");
        let mut state = fresh_state(&key);
        let mut revision = Revision::INITIAL;

        let walk = [
            (Phase::Prepare, RecordedStep::Continued, 1, Phase::Observe),
            (Phase::Observe, RecordedStep::Continued, 1, Phase::Decide),
            (Phase::Decide, RecordedStep::Continued, 1, Phase::Resolve),
            (Phase::Resolve, RecordedStep::Continued, 1, Phase::Apply),
            (
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
                1,
                Phase::Epilogue,
            ),
            (Phase::Epilogue, RecordedStep::Continued, 2, Phase::Prepare),
        ];

        for (phase, step, next_iteration, next_phase) in walk {
            let seq = store
                .append_journal(&key, &[JournalAppend::phase_completed(1, phase, step)])
                .await
                .expect("the phase records what it did");
            state.journal_seq = seq;
            state.cursor = LoopCursor {
                iteration: next_iteration,
                phase: next_phase,
            };
            revision = store
                .commit(&key, &state, revision)
                .await
                .expect("the boundary commit");

            // ...and here the worker dies, having appended the NEXT phase's
            // record without committing it.
            store
                .append_journal(
                    &key,
                    &[JournalAppend::phase_completed(
                        next_iteration,
                        next_phase,
                        RecordedStep::Continued,
                    )],
                )
                .await
                .expect("the attempt that never commits");

            let resumed = store
                .load(&key)
                .await
                .expect("load")
                .expect("the committed state survived the crash");
            assert_eq!(resumed.revision, revision);
            assert_eq!(resumed.state.cursor.phase, next_phase, "after {phase}");
            assert_eq!(resumed.state.cursor.iteration, next_iteration);

            let records = store.read_journal(&key, 0).await.expect("read");
            let journal = Journal::from_records(records).expect("well-formed");
            assert!(
                !journal.orphaned(resumed.state.journal_seq).is_empty(),
                "the dead worker's record is in the log after {phase}"
            );
            let replayed = replay(journal.authoritative(resumed.state.journal_seq))
                .expect("the committed prefix must replay");
            assert_eq!(
                (replayed.iteration, replayed.phase),
                (resumed.state.cursor.iteration, resumed.state.cursor.phase),
                "replay of the committed prefix must land where the commit did, after {phase}"
            );
        }

        // One iteration completed, and the orphan from the last crash is still
        // uncommitted, so the watermark never covered it.
        assert_eq!(state.cursor.iteration, 2);
        let final_state = store.load(&key).await.expect("load").expect("present");
        let records = store.read_journal(&key, 0).await.expect("read");
        let journal = Journal::from_records(records).expect("well-formed");
        assert_eq!(
            journal.authoritative(final_state.state.journal_seq).len(),
            6,
            "exactly the six committed phase records are authoritative"
        );
    }

    pub async fn a_re_run_after_a_crash_does_not_disturb_a_settled_effect<S: LoopStateStore>(
        store: &S,
    ) {
        // The second half of "no non-retry-safe effect fires twice". The first
        // half is that a missing result routes to reconciliation; this is the
        // case where the result DID land — the dispatch settled, then the worker
        // died before committing — and the re-running phase re-commits its
        // intent. If that intent commit overwrote the row, the resumed worker
        // would see no result and reconcile a send that had already been
        // answered.
        let key = key("crash-resettle");
        let pending = unsafe_effect("llm-1:tool:call-1");
        let intent = EffectLedgerEntry::intent(&pending, 2, Phase::Apply, 1_000);

        store
            .record_effect_intent(&key, &intent)
            .await
            .expect("intent");
        store
            .record_effect_outcome(
                &key,
                &pending.effect_id,
                EffectOutcome::Succeeded { at_ms: 1_100 },
            )
            .await
            .expect("the dispatch settled");

        // ...the worker dies here, and the phase re-runs from the last commit.
        let mut re_run = intent.clone();
        re_run.intent_at_ms = 2_000;
        store
            .record_effect_intent(&key, &re_run)
            .await
            .expect("a re-running phase re-commits its intent");

        let ledger = store.load_effects(&key).await.expect("load");
        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .and_then(|entry| entry.outcome.clone()),
            Some(EffectOutcome::Succeeded { at_ms: 1_100 }),
            "the result that landed must survive the re-run's intent commit"
        );
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);
    }

    pub async fn an_effect_with_an_intent_and_no_outcome_reads_as_no_answer<S: LoopStateStore>(
        store: &S,
    ) {
        // The whole point of the ledger. An intent with no outcome is a worker
        // that died between committing and settling — which is exactly the case
        // where "nothing recorded" must NOT be read as "nothing happened".
        let key = key("effect-intent");
        let pending = unsafe_effect("llm-1:tool:call-1");
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&pending, 2, Phase::Apply, 1_000),
            )
            .await
            .expect("intent");

        let ledger = store.load_effects(&key).await.expect("load");
        let entry = ledger.get(&pending.effect_id).expect("the row is there");
        assert!(entry.outcome.is_none());
        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reconcile,
            "a non-retry-safe effect with no result must be reconciled, never re-fired"
        );
    }

    pub async fn a_settled_effect_is_adopted_rather_than_fired_again<S: LoopStateStore>(store: &S) {
        let key = key("effect-settled");
        let pending = unsafe_effect("llm-1:tool:call-1");
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&pending, 2, Phase::Apply, 1_000),
            )
            .await
            .expect("intent");
        store
            .record_effect_outcome(
                &key,
                &pending.effect_id,
                EffectOutcome::Succeeded { at_ms: 1_200 },
            )
            .await
            .expect("outcome");

        let ledger = store.load_effects(&key).await.expect("load");
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);

        // And a second, different settlement is refused rather than accepted.
        let error = store
            .record_effect_outcome(
                &key,
                &pending.effect_id,
                EffectOutcome::Failed {
                    at_ms: 1_300,
                    reason: "no".to_string(),
                },
            )
            .await
            .expect_err("two holders settling one effect differently is a defect");
        assert!(
            matches!(
                error,
                StoreError::Effect(EffectError::OutcomeConflict { .. })
            ),
            "got {error}"
        );
    }

    pub async fn a_crash_between_intent_and_fire_does_not_re_fire_a_live_send<S: LoopStateStore>(
        store: &S,
    ) {
        // The crash-injection case the design names: kill at the commit boundary
        // and assert that no non-retry-safe effect fires twice. The store's job
        // is to hold the intent across the crash; the disposition is what proves
        // the resumed worker does not blind-fire.
        let key = key("crash-intent");
        let batch = PendingBatch {
            iteration: 2,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects: vec![unsafe_effect("llm-1:tool:call-1")],
        };
        batch.validate().expect("well-formed");

        let mut state = fresh_state(&key);
        state.cursor = LoopCursor {
            iteration: 2,
            phase: Phase::Apply,
        };
        state.pending = Some(batch.clone());
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("the intent commit lands before anything fires");
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&batch.effects[0], 2, Phase::Apply, 1_000),
            )
            .await
            .expect("intent");

        // …the worker dies here, having possibly fired…

        let resumed = store.load(&key).await.expect("load").expect("present");
        let pending = resumed.state.pending.expect("the batch survived");
        pending.validate().expect("and is still well-formed");
        let ledger = store.load_effects(&key).await.expect("load");
        let plan = ledger.plan(&pending);
        assert_eq!(plan.len(), 1);
        assert_eq!(
            plan[0].1,
            EffectDisposition::Reconcile,
            "the resumed worker must ask the outward record, not fire again"
        );
    }

    pub async fn a_live_lease_excludes_every_claimant_until_it_expires<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("lease");
        let mine = WorkerId::new("worker-a");
        let theirs = WorkerId::new("worker-b");

        let lease = store
            .claim(&key, &mine, Duration::from_secs(300))
            .await
            .expect("claim");
        assert_eq!(lease.worker, mine);

        let error = store
            .claim(&key, &theirs, Duration::from_secs(300))
            .await
            .expect_err("a live lease excludes another worker");
        match error {
            StoreError::LeaseHeld { by, .. } => assert_eq!(by, mine),
            other => panic!("expected the lease to be held, got {other}"),
        }

        let error = store
            .claim(&key, &mine, Duration::from_secs(300))
            .await
            .expect_err("the holder's id is not proof it still possesses this lease");
        match error {
            StoreError::LeaseHeld { by, .. } => assert_eq!(by, mine),
            other => panic!("expected the same worker's lease to be held, got {other}"),
        }

        let renewed = store
            .renew(&lease, Duration::from_secs(300))
            .await
            .expect("the current fenced lease is the one extension capability");
        assert!(renewed.fence > lease.fence, "a renewal must fence forward");
    }

    pub async fn an_expired_lease_may_be_taken_and_its_holder_is_then_refused<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("lease-expiry");
        let mine = WorkerId::new("worker-a");
        let theirs = WorkerId::new("worker-b");

        // A zero TTL is already expired, which makes this deterministic rather
        // than a sleep.
        let stale = store
            .claim(&key, &mine, Duration::ZERO)
            .await
            .expect("claim");
        let taken = store
            .claim(&key, &theirs, Duration::from_secs(300))
            .await
            .expect("an expired lease may be taken");
        assert!(taken.fence > stale.fence);

        let error = store
            .renew(&stale, Duration::from_secs(300))
            .await
            .expect_err("the superseded holder must not be able to renew");
        assert!(matches!(error, StoreError::LeaseLost { .. }), "got {error}");

        let error = store
            .release(stale)
            .await
            .expect_err("nor to release the lease it no longer holds");
        assert!(matches!(error, StoreError::LeaseLost { .. }), "got {error}");
    }

    pub async fn a_superseded_fence_cannot_mutate_state_or_journal<S: LoopStateStore>(store: &S) {
        let key = key("fenced-mutations");
        let original = fresh_state(&key);
        let revision = store
            .commit(&key, &original, Revision::INITIAL)
            .await
            .expect("seed");
        let stale = store
            .claim(&key, &WorkerId::new("worker-a"), Duration::ZERO)
            .await
            .expect("claim");
        let current = store
            .claim(&key, &WorkerId::new("worker-b"), Duration::from_secs(300))
            .await
            .expect("take over expired lease");

        let mut forbidden = original.clone();
        forbidden.phase_attempts = 99;
        let error = store
            .commit_fenced(&key, &forbidden, revision, &stale)
            .await
            .expect_err("a superseded fence cannot commit");
        assert!(matches!(error, StoreError::LeaseLost { .. }), "got {error}");

        let error = store
            .append_journal_fenced(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
                &stale,
            )
            .await
            .expect_err("a superseded fence cannot append");
        assert!(matches!(error, StoreError::LeaseLost { .. }), "got {error}");

        let loaded = store.load(&key).await.expect("load").expect("present");
        assert_eq!(loaded.state.phase_attempts, 0);
        assert!(
            store.read_journal(&key, 0).await.expect("read").is_empty(),
            "lease validation and mutation must be one critical section"
        );
        store
            .release(current)
            .await
            .expect("release current holder");
    }

    pub async fn releasing_a_lease_lets_the_next_worker_in<S: LoopStateStore>(store: &S) {
        let key = key("lease-release");
        let mine = WorkerId::new("worker-a");
        let theirs = WorkerId::new("worker-b");

        let lease = store
            .claim(&key, &mine, Duration::from_secs(300))
            .await
            .expect("claim");
        let renewed = store
            .renew(&lease, Duration::from_secs(300))
            .await
            .expect("renew");
        assert!(renewed.fence > lease.fence);

        store.release(renewed).await.expect("release");
        let next = store
            .claim(&key, &theirs, Duration::from_secs(300))
            .await
            .expect("a released lease is free");
        assert_eq!(next.worker, theirs);
    }

    pub async fn list_runnable_offers_a_committed_execution<S: LoopStateStore>(store: &S) {
        let key = key("runnable");
        let state = fresh_state(&key);
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let runnable = store
            .list_runnable(&WorkerId::new("worker-a"), 10)
            .await
            .expect("list");
        assert!(runnable.contains(&key), "got {runnable:?}");
    }

    pub async fn list_runnable_withholds_work_that_is_not_ready_and_offers_expired_work<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        let worker = WorkerId::new("worker-a");
        let now_ms = chrono::Utc::now().timestamp_millis();

        let parked = key("parked");
        let mut state = fresh_state(&parked);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&parked, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let backing_off = key("backing-off");
        let mut state = fresh_state(&backing_off);
        state.runnable_at_ms = now_ms + 3_600_000;
        store
            .commit(&backing_off, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let expired = key("expired");
        let mut state = fresh_state(&expired);
        state.deadline_at_ms = Some(now_ms - 1);
        // Deadline retirement outranks retry backoff. Otherwise an execution
        // whose last Retry scheduled far into the future stays invisible after
        // its wall-clock deadline and cannot be durably timed out until that
        // unrelated backoff expires.
        state.runnable_at_ms = now_ms + 3_600_000;
        store
            .commit(&expired, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let pinned_elsewhere = key("pinned-elsewhere");
        let mut state = fresh_state(&pinned_elsewhere);
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-b"),
            // Far in the future, so this asserts the pin WITHHOLDS the work. A
            // lapsed pin is claimable by anyone and would make this case pass
            // for the opposite reason.
            pinned_until_ms: now_ms + 3_600_000,
        };
        store
            .commit(&pinned_elsewhere, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        for withheld in [&parked, &backing_off, &pinned_elsewhere] {
            assert!(
                !runnable.contains(withheld),
                "{withheld} must not be offered: {runnable:?}"
            );
        }
        assert!(
            runnable.contains(&expired),
            "an expired unparked execution must be offered for durable retirement: {runnable:?}"
        );
    }

    /// End a run in `store`'s journal and commit a watermark that covers it.
    ///
    /// Through `append_journal` and `commit`, never by writing a marker: the
    /// property under test is that the STORE derives the ending, and a fixture
    /// that stamped it would pass against a store that derived nothing.
    async fn end_run_as<S: LoopStateStore>(
        store: &S,
        key: &ExecutionKey,
        terminal: TerminalKind,
        covered_by_the_watermark: bool,
    ) {
        let seq = store
            .append_journal(
                key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::RunEnded { terminal },
                )],
            )
            .await
            .expect("the phase records how the run ended");
        let mut state = fresh_state(key);
        // Zero is the watermark of a boundary whose append landed and whose
        // commit did not — an orphan, which is a run that has NOT ended.
        state.journal_seq = if covered_by_the_watermark { seq } else { 0 };
        store
            .commit(key, &state, Revision::INITIAL)
            .await
            .expect("the boundary commit");
    }

    pub async fn list_runnable_withholds_a_run_that_ended_for_good<S: LoopStateStore>(store: &S) {
        // The starvation this closes: a terminal lives in the JOURNAL and
        // `LoopState` carries none, so a run that finished an hour ago is
        // claimable, unleased, unparked and inside every timer. It was offered
        // by every scan forever, refused by every claim, and — because the scan
        // takes a `limit` and returns a prefix — it held its slot while live
        // work queued behind it.
        //
        // Three fixtures, built the same way and differing only in the thing
        // under test. Two of them are the ones that matter: an assertion that a
        // finished run is withheld passes just as well on a store that withheld
        // everything, and the shape a first cut of this really does produce is a
        // store that also withholds every PAUSED run — which is worse than the
        // bug, because a run nobody offers looks exactly like a run nobody has
        // got to yet.
        let worker = WorkerId::new("worker-a");

        let for_good = key("ended-for-good");
        end_run_as(store, &for_good, TerminalKind::Failed, true).await;

        let resumably = key("ended-resumably");
        end_run_as(store, &resumably, TerminalKind::PausedByUser, true).await;

        let uncommitted = key("ending-uncommitted");
        end_run_as(store, &uncommitted, TerminalKind::Failed, false).await;

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            !runnable.contains(&for_good),
            "a run whose committed journal ends on a non-resumable terminal must not be \
             offered: {runnable:?}"
        );
        assert!(
            runnable.contains(&resumably),
            "a PAUSE is not an ending. `PausedByUser`, `WaitingForUser`, \
             `WaitingForConfirmation`, `WaitingForChildren` and `Sleeping` end the invocation \
             and leave the execution alive, and withholding them makes every paused run \
             unresumable: {runnable:?}"
        );
        assert!(
            runnable.contains(&uncommitted),
            "the terminal record is above the watermark, so it is an orphaned attempt by a \
             worker that appended and died — the run did NOT end. A store that published the \
             ending at APPEND time strands this live execution forever: {runnable:?}"
        );

        // And the same run, once a commit does cover the record, is withheld.
        // Without this the case above would also pass on a store that never
        // publishes an ending for a key whose first commit had none.
        let mut state = fresh_state(&uncommitted);
        state.journal_seq = 1;
        store
            .commit(&uncommitted, &state, Revision::INITIAL.next())
            .await
            .expect("the boundary commit that was missing");
        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            !runnable.contains(&uncommitted),
            "the ending is published by the commit that vouches for the record, not only by \
             the commit that follows the append: {runnable:?}"
        );
    }

    pub async fn terminal_outbox_debt_stays_runnable_until_its_cursor_advances<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        let key = key("terminal-outbox-debt");
        let appends = vec![
            JournalAppend::named_event(
                1,
                Phase::Prepare,
                "plan.step.finished",
                "agent-1",
                None,
                None,
                serde_json::json!({ "step_id": "last" }),
            )
            .expect("bounded named event"),
            JournalAppend::phase_completed(
                1,
                Phase::Prepare,
                RecordedStep::RunEnded {
                    terminal: TerminalKind::Success,
                },
            ),
        ];
        let watermark = store
            .append_journal(&key, &appends)
            .await
            .expect("terminal append");
        let mut state = fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("terminal commit");

        let worker = WorkerId::new("worker-a");
        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            runnable.contains(&key),
            "a committed terminal must remain discoverable while its accepted event is owed"
        );

        // Projection discovery is not scheduling: an active execution lease
        // must not make durable terminal debt invisible. The projector will
        // contend for the claim and re-check the exact key before it emits.
        let lease = store
            .claim(
                &key,
                &WorkerId::new("another-holder"),
                Duration::from_secs(300),
            )
            .await
            .expect("claim terminal debt");
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero scan budget"), None)
            .await
            .expect("scan terminal debt");
        assert!(
            debt.keys.contains(&key),
            "placement and lease state do not erase committed projection debt"
        );
        store.release(lease).await.expect("release terminal debt");

        let journal = store.read_journal_verified(&key).await.expect("journal");
        let mut cursor = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projection = cursor.project(&journal, key.execution_id(), watermark, &mut sink);
        assert_eq!(projection.emitted, 1);
        store
            .save_projector_cursor(&key, &cursor)
            .await
            .expect("save cursor");

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            !runnable.contains(&key),
            "once the terminal event is projected, final executions leave discovery"
        );
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero scan budget"), None)
            .await
            .expect("scan terminal debt");
        assert!(
            !debt.keys.contains(&key),
            "the dedicated lifecycle scan also retires debt only after the durable cursor moves"
        );
    }

    /// Reconciler retirement may append only a terminal record after every
    /// earlier event was already acknowledged. Runtime settlement therefore
    /// needs its own exact durable receipt; event projection cannot stand in
    /// for it and an eventless ending must remain discoverable.
    pub async fn eventless_cannot_proceed_remains_debt_until_runtime_settlement<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        let key = key("eventless-runtime-settlement-debt");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::CannotProceed,
                    },
                )],
            )
            .await
            .expect("append eventless CannotProceed terminal");
        let mut state = fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit eventless terminal");

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero scan budget"), None)
            .await
            .expect("scan runtime settlement debt");
        assert_eq!(debt.keys, vec![key.clone()]);

        let mut cursor = ProjectorCursor::new();
        cursor.mark_runtime_terminal_settled(watermark);
        store
            .save_projector_cursor(&key, &cursor)
            .await
            .expect("save independent runtime settlement receipt");
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero scan budget"), None)
            .await
            .expect("rescan settled terminal");
        assert!(debt.keys.is_empty());
    }

    pub async fn a_scan_pages_past_work_no_claim_can_change<S: LoopStateStore>(store: &S) {
        // What a cursor is FOR, and it is not the ended runs above.
        //
        // A quarantined run, one past its iteration ceiling, and one holding an
        // indeterminate effect all answer a permanent refusal at claim time and
        // commit nothing — so they are offered again, unchanged, by every later
        // scan. None of the three is visible from here: the quarantine threshold
        // is the driver's `max_phase_attempts`, the ceiling is the HOST's
        // `max_iterations`, and the effect answer is a ledger read the scan does
        // not do. No filter in this store can remove them, so paging is the only
        // way past, and a scan that offered a prefix and no way to ask for more
        // let `limit` such keys hide every live run behind them permanently.
        //
        // The keys are named so the ones standing in for that population sort
        // FIRST, which is what makes the fixture about a prefix rather than
        // about a set.
        let worker = WorkerId::new("worker-a");
        let stuck_one = key("aaa-stuck-one");
        let stuck_two = key("bbb-stuck-two");
        let live = key("ccc-live");
        for each in [&stuck_one, &stuck_two, &live] {
            store
                .commit(each, &fresh_state(each), Revision::INITIAL)
                .await
                .expect("commit");
        }

        // The uncursored call is the defect, stated as an assertion so the rest
        // is about a fix rather than about a preference.
        let unpaged = store.list_runnable(&worker, 2).await.expect("list");
        assert_eq!(
            unpaged,
            vec![stuck_one.clone(), stuck_two.clone()],
            "the whole of `list_runnable` is the first `limit` offerable keys, and the live run \
             is not among them"
        );

        let mut seen: Vec<ExecutionKey> = Vec::new();
        let mut after: Option<ScanCursor> = None;
        let mut pages = 0;
        loop {
            let page = store
                .scan_runnable(&worker, 2, after.as_ref())
                .await
                .expect("a page");
            seen.extend(page.keys.iter().cloned());
            pages += 1;
            assert!(pages <= 8, "paging must terminate; got {seen:?}");
            match page.resume {
                Some(resume) => after = Some(resume),
                None => break,
            }
        }
        assert!(
            seen.contains(&live),
            "the live run behind the prefix must be reachable: {seen:?}"
        );
        assert_eq!(
            seen,
            vec![stuck_one, stuck_two, live],
            "one pass must visit every execution exactly once and in the walk's order — a \
             cursor that re-offered what it had already handed out would spin, and one that \
             skipped ahead would lose work with nothing saying so"
        );
    }

    pub async fn a_scan_that_reached_the_end_says_so_and_a_short_one_does_not<S: LoopStateStore>(
        store: &S,
    ) {
        // `resume: None` is a CLAIM — *there is nothing after this* — and a
        // caller stops paging on it. A store that answered `None` whenever its
        // page happened to fill would make the tail of itself unreachable, and
        // the symptom would be indistinguishable from a store with less work in
        // it than it has.
        let worker = WorkerId::new("worker-a");
        let only = key("only-one");
        store
            .commit(&only, &fresh_state(&only), Revision::INITIAL)
            .await
            .expect("commit");

        let whole = store
            .scan_runnable(&worker, 50, None)
            .await
            .expect("a page");
        assert_eq!(whole.keys, vec![only.clone()]);
        assert!(
            whole.resume.is_none(),
            "the walk reached the end of the store, so there is nothing to resume after"
        );

        // A page that stopped because it was full must name a resume point even
        // though the entry it stopped on is the last one — the walk does not
        // know that, and guessing would be the failure above.
        let second = key("only-two");
        store
            .commit(&second, &fresh_state(&second), Revision::INITIAL)
            .await
            .expect("commit");
        let short = store.scan_runnable(&worker, 1, None).await.expect("a page");
        assert_eq!(short.keys.len(), 1);
        assert!(
            short.resume.is_some(),
            "a page that filled has not reached the end and must say where to carry on"
        );

        // A limit of zero examines nothing. Answering `None` there would say the
        // store had been walked to the end before the walk started.
        let none_at_all = store.scan_runnable(&worker, 0, None).await.expect("a page");
        assert!(none_at_all.keys.is_empty());
        let mut after = none_at_all
            .resume
            .expect("a scan that examined nothing has not reached the end of anything");

        // And that is only half the property. `resume.is_some()` is satisfied by
        // a page that hands back the cursor it was given — which is what both
        // implementations used to do here, through an `or_else` that read as
        // prudence — and such a page is not a resume point at all: the next call
        // asks the same question, and the loop below never ends. So this pages
        // for real and asserts the two things that make paging safe, that each
        // page moves and that the walk finishes.
        let mut reached_the_end = false;
        for _ in 0..8 {
            let page = store
                .scan_runnable(&worker, 0, Some(&after))
                .await
                .expect("a page");
            assert!(
                page.keys.is_empty(),
                "a scan asked for zero keys offers none, wherever it stopped"
            );
            match page.resume {
                Some(resume) => {
                    assert!(
                        resume > after,
                        "each page must resume STRICTLY past the one before it, or a caller \
                         paging in a loop is asking the same question forever"
                    );
                    after = resume;
                },
                None => {
                    reached_the_end = true;
                    break;
                },
            }
        }
        assert!(
            reached_the_end,
            "paging with a limit of zero must still walk to the end of the store and stop"
        );
    }

    pub async fn an_ending_is_retracted_when_the_prefix_no_longer_ends_on_one<S: LoopStateStore>(
        store: &S,
    ) {
        // The ending is a PUBLISHED value, and a published value that is only
        // ever written is a claim that outlives what it was derived from.
        //
        // The shape is the one `non_resumable_terminal`'s docs describe as safe:
        // a record after a committed non-resumable terminal, which is a journal
        // a replay refuses. That function answers `None` for it — correctly, its
        // last record is not a `RunEnded` — and the documented consequence is
        // that the run stays offered, is claimed, and is quarantined by
        // `driver_worker::verify_journal` where the corruption is legible.
        //
        // It did not. The marker published by the earlier commit was left in
        // place and was once treated as valid below a later watermark, so the
        // run was withheld from every scan. Exact equality and retraction now
        // both prevent that. Nothing in the store forbids the shape, either:
        // `append_journal` accepts a record after a committed terminal without
        // complaint, so the rule is the driver's discipline and not a
        // protection in force.
        let worker = WorkerId::new("worker-a");
        let key = key("ended-then-appended");
        end_run_as(store, &key, TerminalKind::Failed, true).await;

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            !runnable.contains(&key),
            "the fixture has to be withheld first, or the assertion below is satisfied by a \
             run that was never withheld: {runnable:?}"
        );

        // A record after the terminal, and a commit that vouches for it. The
        // committed prefix now ends on something that is not an ending.
        let seq = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    2,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the store accepts it, which is the point");
        let mut state = fresh_state(&key);
        state.journal_seq = seq;
        store
            .commit(&key, &state, Revision::INITIAL.next())
            .await
            .expect("the commit that makes the record authoritative");

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            runnable.contains(&key),
            "a commit publishes the ending its prefix ends on and RETRACTS the one it does \
             not. A marker left behind withholds the run from every scan, and a run nothing \
             offers is a run nothing claims — so the journal that would have quarantined it \
             is never read: {runnable:?}"
        );
    }

    pub async fn the_paged_scan_and_the_flat_listing_answer_the_same_question<S: LoopStateStore>(
        store: &S,
    ) {
        // Two entry points, one filter. They are written as a delegation in both
        // implementations precisely so this cannot drift; the case exists
        // because a later editor re-forking them would produce a key one caller
        // can see and another cannot, and nothing else here would notice.
        let worker = WorkerId::new("worker-a");
        let now_ms = chrono::Utc::now().timestamp_millis();

        let offered = key("agree-offered");
        store
            .commit(&offered, &fresh_state(&offered), Revision::INITIAL)
            .await
            .expect("commit");

        let ended = key("agree-ended");
        end_run_as(store, &ended, TerminalKind::Success, true).await;

        let parked = key("agree-parked");
        let mut state = fresh_state(&parked);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&parked, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let backing_off = key("agree-backing-off");
        let mut state = fresh_state(&backing_off);
        state.runnable_at_ms = now_ms + 3_600_000;
        store
            .commit(&backing_off, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let flat = store.list_runnable(&worker, 50).await.expect("list");
        let paged = store
            .scan_runnable(&worker, 50, None)
            .await
            .expect("a page");
        assert_eq!(flat, paged.keys);
        assert_eq!(
            flat,
            vec![offered],
            "and the shared answer is the right one, or the two would only have to agree about \
             being wrong"
        );
    }

    pub async fn a_resolved_wake_makes_a_parked_execution_runnable_again<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("wake");
        let worker = WorkerId::new("worker-a");
        let wait = WaitReason::children(vec!["child-1".to_string()]).expect("one live child");
        let mut state = fresh_state(&key);
        state.wait = Some(wait.clone());
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(!runnable.contains(&key), "a parked execution waits");

        store
            .resolve_wake(&key, &wait.wake_token(), "child-1-completed")
            .await
            .expect("the completer resolves the token without holding the lease");

        let runnable = store.list_runnable(&worker, 50).await.expect("list");
        assert!(runnable.contains(&key), "got {runnable:?}");

        // The same completion delivered twice is one resolution. A retried job
        // runner must not make the parent wake for something that happened once.
        store
            .resolve_wake(&key, &wait.wake_token(), "child-1-completed")
            .await
            .expect("a re-delivered completion is idempotent");
        assert_eq!(
            store
                .wake_resolutions(&key, &wait.wake_token())
                .await
                .expect("read the outstanding resolutions"),
            vec!["child-1-completed".to_string()],
            "at-least-once delivery must not turn one completion into two"
        );
    }

    pub async fn a_second_park_on_one_token_waits_for_a_second_completion<S: LoopStateStore>(
        store: &S,
    ) {
        // The defect: nothing removed a resolution, so a run that parked twice on
        // one token was runnable immediately the second time and spun. Re-parking
        // on the same job is the ORDINARY shape of a run waiting on a child that
        // reports more than once, so "vary the token" is not a rule a driver can
        // keep.
        let key = key("wake-twice");
        let worker = WorkerId::new("worker-a");
        let wait = WaitReason::Job {
            job_id: "coding-7".to_string(),
        };
        let token = wait.wake_token();

        let mut state = fresh_state(&key);
        state.wait = Some(wait.clone());
        let mut revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit the first park");

        store
            .resolve_wake(&key, &token, "report-1")
            .await
            .expect("the first report");
        assert!(
            store
                .list_runnable(&worker, 50)
                .await
                .expect("list")
                .contains(&key),
            "the first report wakes the run"
        );

        // Leaving the park: read what we are acting on, commit, then consume.
        let acted_on = store
            .wake_resolutions(&key, &token)
            .await
            .expect("read the outstanding resolutions");
        assert_eq!(acted_on, vec!["report-1".to_string()]);
        state.wait = None;
        revision = store
            .commit(&key, &state, revision)
            .await
            .expect("commit the wake before consuming it");
        assert_eq!(
            store
                .consume_wake(&key, &token, &acted_on)
                .await
                .expect("consume"),
            1
        );

        // Now park again on the SAME token. Nothing has reported since, so this
        // park must hold.
        state.wait = Some(wait.clone());
        revision = store
            .commit(&key, &state, revision)
            .await
            .expect("commit the second park");
        assert!(
            !store
                .list_runnable(&worker, 50)
                .await
                .expect("list")
                .contains(&key),
            "the second park on one token must wait for a second report, not \
             inherit the first one's resolution"
        );

        // And a genuinely new report still wakes it, or the fix would be "never
        // wake twice" rather than "wake once per completion".
        store
            .resolve_wake(&key, &token, "report-2")
            .await
            .expect("the second report");
        assert!(
            store
                .list_runnable(&worker, 50)
                .await
                .expect("list")
                .contains(&key),
            "a second completion must still wake the second park"
        );
        let _ = revision;
    }

    pub async fn a_resolution_that_arrives_before_the_park_still_satisfies_it<S: LoopStateStore>(
        store: &S,
    ) {
        // The race that made the old marker unremovable, and the one the
        // consume-what-you-acted-on rule has to keep satisfying: a completer may
        // resolve BEFORE the parent commits its park. That resolution belongs to
        // the park that has not been written yet.
        let key = key("wake-early");
        let worker = WorkerId::new("worker-a");
        let wait = WaitReason::Job {
            job_id: "coding-9".to_string(),
        };
        let token = wait.wake_token();

        store
            .resolve_wake(&key, &token, "report-1")
            .await
            .expect("the job finishes before the parent has parked");

        let mut state = fresh_state(&key);
        state.wait = Some(wait);
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit the park afterwards");

        assert!(
            store
                .list_runnable(&worker, 50)
                .await
                .expect("list")
                .contains(&key),
            "an early resolution must satisfy the park it arrived before, or a \
             run parks forever on a job that already finished"
        );
    }

    pub async fn a_completion_landing_during_a_wake_is_not_consumed_with_it<S: LoopStateStore>(
        store: &S,
    ) {
        // Why the consume takes the exact ids rather than clearing the token. A
        // report can land between the run reading what it is acting on and the
        // consume that follows its commit; clearing the token would swallow a
        // completion nobody has seen, and the run would park next time with
        // nothing left to wake it.
        let key = key("wake-interleaved");
        let worker = WorkerId::new("worker-a");
        let wait = WaitReason::Job {
            job_id: "coding-11".to_string(),
        };
        let token = wait.wake_token();

        let mut state = fresh_state(&key);
        state.wait = Some(wait.clone());
        let revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit the park");

        store
            .resolve_wake(&key, &token, "report-1")
            .await
            .expect("the first report");
        let acted_on = store
            .wake_resolutions(&key, &token)
            .await
            .expect("read what this wake is acting on");

        // The second report lands after that read and before the consume.
        store
            .resolve_wake(&key, &token, "report-2")
            .await
            .expect("the second report");

        state.wait = None;
        let revision = store
            .commit(&key, &state, revision)
            .await
            .expect("commit the wake");
        assert_eq!(
            store
                .consume_wake(&key, &token, &acted_on)
                .await
                .expect("consume only what was acted on"),
            1
        );
        assert_eq!(
            store
                .wake_resolutions(&key, &token)
                .await
                .expect("read again"),
            vec!["report-2".to_string()],
            "the completion that landed mid-wake must survive the consume"
        );

        // So the next park on that token is satisfied by it rather than hanging.
        state.wait = Some(wait);
        store
            .commit(&key, &state, revision)
            .await
            .expect("commit the second park");
        assert!(
            store
                .list_runnable(&worker, 50)
                .await
                .expect("list")
                .contains(&key),
            "a report nobody consumed must satisfy the next park"
        );
    }

    /// The reconciler's listing offers what is parked and nothing else.
    ///
    /// The negative half is what makes it a test: a `list_parked` that returned
    /// every committed execution would pass an assertion that only looked for the
    /// parked one, and a reconciler over that listing would diagnose every run in
    /// the store on every pass.
    pub async fn list_parked_offers_parked_work_and_nothing_else<S: LoopStateStore>(store: &S) {
        let parked = key("parked-run");
        let wait = WaitReason::children(vec!["child-1".to_string()]).expect("one live child");
        let mut state = fresh_state(&parked);
        state.wait = Some(wait.clone());
        store
            .commit(&parked, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let running = key("running-run");
        store
            .commit(&running, &fresh_state(&running), Revision::INITIAL)
            .await
            .expect("commit");

        // Committed, not parked, and NOT runnable either — a backoff. A listing
        // that answered "everything `list_runnable` withholds" rather than
        // "everything parked" would pick this up, and the reconciler would report
        // a run that is simply waiting out a retry delay.
        let backing_off = key("backing-off-run");
        let mut state = fresh_state(&backing_off);
        state.runnable_at_ms = chrono::Utc::now().timestamp_millis() + 3_600_000;
        store
            .commit(&backing_off, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let listing = store.list_parked(50).await.expect("list");
        assert!(!listing.incomplete, "the whole store fits in the limit");
        let keys: Vec<&ExecutionKey> = listing.parked.iter().map(|row| &row.key).collect();
        assert_eq!(keys, vec![&parked], "got {keys:?}");
        assert_eq!(listing.parked[0].wait, wait);
    }

    /// A park whose wake has already resolved is still listed.
    ///
    /// Deliberate, and asserted so a later "optimisation" has to argue with a
    /// test: a resolved park nothing has picked up is a stall of its own, and a
    /// listing that filtered it would make the one case where the scheduler and
    /// the store disagree invisible. Filtering would also put a wake-ledger read
    /// per parked execution inside a listing, which is the per-poll scan the
    /// design rules out, moved behind a different method name.
    pub async fn list_parked_still_offers_a_park_whose_wake_resolved<S: LoopStateStore>(store: &S) {
        let key = key("resolved-park");
        let wait = WaitReason::children(vec!["child-1".to_string()]).expect("one live child");
        let mut state = fresh_state(&key);
        state.wait = Some(wait.clone());
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");
        store
            .resolve_wake(&key, &wait.wake_token(), "child-1-done")
            .await
            .expect("the completer reports");

        let listing = store.list_parked(50).await.expect("list");
        assert_eq!(
            listing.parked.len(),
            1,
            "a satisfied park is still a park until a commit clears the wait: {:?}",
            listing.parked
        );
    }

    /// A pass that stopped short says so rather than reading as a full sweep.
    pub async fn list_parked_reports_a_pass_that_stopped_short<S: LoopStateStore>(store: &S) {
        for id in ["park-a", "park-b"] {
            let key = key(id);
            let mut state = fresh_state(&key);
            state.wait = Some(WaitReason::Job {
                job_id: format!("job-for-{id}"),
            });
            store
                .commit(&key, &state, Revision::INITIAL)
                .await
                .expect("commit");
        }

        let full = store.list_parked(50).await.expect("list");
        assert_eq!(full.parked.len(), 2);
        assert!(!full.incomplete, "the whole store fits in this limit");

        let capped = store.list_parked(1).await.expect("list");
        assert_eq!(capped.parked.len(), 1);
        assert!(
            capped.incomplete,
            "a listing that filled its limit has not seen the tail, and a caller reading its \
             emptiness as 'nothing is stuck' would be wrong"
        );
    }

    /// A parked run carries when it parked and what lease it last had.
    ///
    /// Neither is ever an input to a **retirement** — that follows a proof taken
    /// from the run's own state and its wake ledger, and `parked_since_ms` only
    /// decides whether a park is looked at at all. Both have a plausible-looking
    /// wrong answer: a `parked_since_ms` of zero reads as a park from 1970, and a
    /// lease reported as live when it was released reads as a worker still
    /// holding the run.
    ///
    /// **What this does NOT pin:** an implementation answering `parked_since_ms`
    /// with the time of the *read* rather than of the commit passes here, because
    /// at this resolution the two are the same instant. The filesystem store's
    /// own `a_park_is_dated_by_the_commit_that_made_it_rather_than_by_the_read`
    /// backdates a snapshot to separate them; the in-memory store has no
    /// equivalent, and this case is deliberately not written as though it did.
    pub async fn a_parked_run_carries_when_it_parked_and_its_last_lease<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("dated-park");
        let worker = WorkerId::new("worker-a");
        let lease = store
            .claim(&key, &worker, Duration::from_secs(300))
            .await
            .expect("claim");

        let before_ms = chrono::Utc::now().timestamp_millis();
        let mut state = fresh_state(&key);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");
        let after_ms = chrono::Utc::now().timestamp_millis();
        store.release(lease).await.expect("release");

        let listing = store.list_parked(50).await.expect("list");
        let row = listing
            .parked
            .iter()
            .find(|row| row.key == key)
            .expect("the park is listed");

        let parked_since_ms = row
            .parked_since_ms
            .expect("a store that just wrote the snapshot can date it");
        assert!(
            // Widened either way by a couple of seconds, because a filesystem
            // mtime is not the clock this process read and the two need not agree
            // to the millisecond. Zero, or a value from a different era, still
            // fails.
            (before_ms - 2_000..=after_ms + 2_000).contains(&parked_since_ms),
            "the park is dated {parked_since_ms}, which is not near the commit that made it \
             ({before_ms}..={after_ms})"
        );

        let last_lease = row.last_lease.as_ref().expect("this run was claimed once");
        assert_eq!(last_lease.worker, worker);
        assert!(
            last_lease.released,
            "the lease was given back, and a reader that thought a worker still held it would \
             conclude the run was being worked on"
        );
    }

    pub async fn a_live_lease_hides_the_work_from_every_worker<S: LoopStateStore>(store: &S) {
        let key = key("leased-out");
        let state = fresh_state(&key);
        let holder = WorkerId::new("worker-b");
        let other = WorkerId::new("worker-a");
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");
        store
            .claim(&key, &holder, Duration::from_secs(300))
            .await
            .expect("claim");

        for worker in [&holder, &other] {
            let runnable = store.list_runnable(worker, 50).await.expect("list");
            assert!(
                !runnable.contains(&key),
                "a live lease must hide work even when the scanner presents the holder's id: \
                 {runnable:?}"
            );
        }
    }

    pub async fn a_batch_that_would_not_validate_is_refused_at_commit<S: LoopStateStore>(
        store: &S,
    ) {
        // The bound has to hold on the way IN as well as on the way out. A store
        // that accepted an over-wide parallel slice would hand a worker a batch
        // it fans out past its own membership.
        let key = key("bad-batch");
        let mut state = fresh_state(&key);
        state.pending = Some(PendingBatch {
            iteration: 1,
            phase: Phase::Apply,
            mode: BatchMode::Parallel { admitted: 9 },
            effects: vec![unsafe_effect("llm-1:tool:call-1")],
        });

        let error = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("an invalid batch must not be committed");
        // The specific refusal, not any effect error: a fixture that drifted into
        // failing for some other reason would otherwise still pass here while
        // testing nothing about the bound it names.
        assert!(
            matches!(
                error,
                StoreError::Effect(EffectError::ParallelSliceOverruns {
                    admitted: 9,
                    effects: 1
                })
            ),
            "got {error}"
        );
    }

    pub async fn a_watermark_beyond_the_log_is_refused<S: LoopStateStore>(store: &S) {
        // A commit claiming records that were never appended would make every
        // later replay expect history that does not exist — and the failure
        // would surface far from the commit that caused it.
        let key = key("watermark-ahead");
        let mut state = fresh_state(&key);
        state.journal_seq = 7;

        let error = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("a watermark cannot exceed the log it points into");
        match error {
            StoreError::WatermarkAhead {
                watermark,
                last_seq,
            } => {
                assert_eq!(watermark, 7);
                assert_eq!(last_seq, 0);
            },
            other => panic!("expected the watermark to be refused, got {other}"),
        }
    }

    pub async fn an_event_record_survives_the_store_intact<S: LoopStateStore>(store: &S) {
        let key = key("event");
        store
            .append_journal(
                &key,
                &[JournalAppend {
                    iteration: 4,
                    phase: Phase::Prepare,
                    ordinal: 2,
                    body: JournalBody::Event {
                        event_type: "iteration_started".to_string(),
                        payload: serde_json::json!({ "iteration": 4, "note": "line\nbreak" }),
                        // NOT `Unrecorded`. That is the field's `Default` and its
                        // `#[serde(default)]` value at once, so a store that
                        // dropped `routing` on the way out and a store that kept
                        // it would both read back as `Unrecorded` and this case
                        // would pass either way. A variant that carries data is
                        // the only shape whose survival is evidence.
                        routing: RecordedEventRouting::CanonicalRuntimeFact {
                            scope: RecordedCanonicalScope {
                                principal: "p".to_string(),
                                workspace: "w".to_string(),
                                task_id: "t".to_string(),
                                ui_thread_id: "thread-9".to_string(),
                            },
                        },
                    },
                }],
            )
            .await
            .expect("append");

        let records = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].event_key(key.execution_id()).to_string(),
            "event:4:prepare:2"
        );
        let JournalBody::Event {
            event_type,
            payload,
            routing,
        } = &records[0].body
        else {
            panic!("the body must survive as an event");
        };
        assert_eq!(event_type, "iteration_started");
        assert_eq!(payload["note"], "line\nbreak");
        // The third field is part of "intact". A projector reads it to decide
        // whether an event is a canonical fact or a transport-only send, and a
        // store that lost it would hand back `Unrecorded` — an absence of a
        // decision that reads exactly like a record written before the field
        // existed, with nothing in the log saying which it was.
        assert_eq!(
            routing,
            &RecordedEventRouting::CanonicalRuntimeFact {
                scope: RecordedCanonicalScope {
                    principal: "p".to_string(),
                    workspace: "w".to_string(),
                    task_id: "t".to_string(),
                    ui_thread_id: "thread-9".to_string(),
                },
            },
            "the routing decision must survive the round trip, or a replay \
             re-decides it from nothing"
        );
    }

    /// A `JournalBody::NamedEvent` survives the store, beside an `Event` in the
    /// same batch.
    ///
    /// # Why this case exists, stated as the gap it closes
    ///
    /// Until it did, **no `NamedEvent` traversed either store in any test**. The
    /// variant's bytes were exercised only by `journal.rs` calling `to_line` /
    /// `from_str` directly, and the driver's end-to-end case
    /// (`driver_worker::a_named_record_reaches_the_host_through_its_own_rail`)
    /// runs on [`MemoryLoopStateStore`](super::memory::MemoryLoopStateStore),
    /// which holds `JournalRecord` structs with no serde step at all. So
    /// `FsLoopStateStore`'s real write/read path — line append, `Journal::parse`,
    /// the unterminated-tail repair, `MAX_JOURNAL_BYTES` — had never seen the
    /// variant, and a store-side regression that dropped a field would have been
    /// caught by nothing. An unstated gap reads as coverage.
    ///
    /// # The fixture carries nothing that is also a default
    ///
    /// `principal` and `workspace` are both
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`. A `None` is
    /// **omitted from the wire entirely**, so a store that dropped the pair and
    /// a store that kept it would both read back `None` and a fixture built on
    /// `None` would pass either way. Both are `Some` here, and neither value is
    /// the key's own scope, so a store that re-derived them from the path rather
    /// than reading them from the record fails too.
    ///
    /// The payload carries an embedded newline for the same reason the event
    /// case does: `FsLoopStateStore` writes one record per LINE, so a value that
    /// contains one is what says the encode escaped it rather than tearing the
    /// record in half.
    ///
    /// # Both rails in ONE batch, on purpose
    ///
    /// `Journal::check_batch_addresses` runs over the batch **regardless of
    /// body**, and `read_journal` hands back one list. A batch with one body kind
    /// would pass a store that had a separate path per rail and lost the
    /// ordering between them; two bodies at two ordinals in one append is what
    /// says the store numbers and returns them as one sequence.
    ///
    /// **What production change leaves this green?** One that changes neither the
    /// wire shape nor the ordering. Dropping `principal`/`workspace` on the way
    /// out fails the scope assertions; renaming the `kind` tag fails the parse;
    /// numbering the two records independently per rail fails the seq assertion;
    /// returning them out of order fails the body match.
    /// The checked door and the raw door describe the same log.
    ///
    /// [`LoopStateStore::read_journal_verified`] has a default that is literally
    /// `read_journal(key, 0)` piped through [`Journal::from_records`], and
    /// `fs::FsLoopStateStore` overrides it to answer with the journal its parse
    /// already built — skipping a copy of every record and a second walk. An
    /// override is a place for the two answers to drift, and a drift here is not
    /// a slow read: `driver_worker::verify_journal` replays the checked answer to
    /// decide whether the committed cursor is real, and
    /// `driver_worker::project_outbox` emits from it. A door that answered with
    /// one record fewer would quarantine live runs; one that answered with a
    /// record more would emit an orphaned attempt's events.
    ///
    /// # Why the records are compared and not counted
    ///
    /// A length check passes on a store that returned the right number of the
    /// wrong records — a suffix, a re-ordering, a stale read. `JournalRecord`
    /// is `PartialEq`, so the whole log is the assertion, and the count below is
    /// only there to stop the comparison being satisfied by two empty answers.
    ///
    /// # Orphans, deliberately
    ///
    /// Nothing is committed here, so every record sits above the watermark. Both
    /// doors promise the raw log — [`Journal::authoritative`] is where the
    /// watermark is applied, and it is applied by the CALLER — so a door that
    /// helpfully filtered orphans out would answer with nothing and fail.
    pub async fn the_checked_read_answers_with_the_log_the_raw_read_does<S: LoopStateStore>(
        store: &S,
    ) {
        let key = key("checked-read");

        // Before anything is written. An execution with no log has an EMPTY one,
        // not a missing one: `verify_journal` runs at the very first phase entry
        // of every run, and a door that reported absence as an error would
        // quarantine every execution on its first claim.
        assert!(
            store
                .read_journal_verified(&key)
                .await
                .expect("an execution that has written nothing still reads")
                .all_records()
                .is_empty(),
            "an unwritten log is empty, not an error"
        );

        // Three, and distinguishable by payload. With one record a door that
        // dropped the first and a door that dropped the last are the same
        // failure; with identical records a re-ordering is invisible.
        store
            .append_journal(
                &key,
                &[
                    iteration_started(1),
                    iteration_started(2),
                    iteration_started(3),
                ],
            )
            .await
            .expect("the store places the records");

        let raw = store
            .read_journal(&key, 0)
            .await
            .expect("the raw door reads");
        let checked = store
            .read_journal_verified(&key)
            .await
            .expect("and the checked door reads");
        assert_eq!(
            raw.len(),
            3,
            "the fixture has to put three records in the log, or the comparison below is two \
             empty answers agreeing"
        );
        assert_eq!(
            checked.all_records(),
            raw.as_slice(),
            "the checked door must answer with the log the raw door answers with, record for \
             record and in order"
        );
    }

    pub async fn a_named_record_survives_the_store_intact<S: LoopStateStore>(store: &S) {
        let key = key("named");
        store
            .append_journal(
                &key,
                &[
                    JournalAppend {
                        iteration: 6,
                        phase: Phase::Apply,
                        ordinal: 0,
                        body: JournalBody::Event {
                            event_type: "AgenticActionExecuted".to_string(),
                            payload: serde_json::json!({ "tool": "gmail__send" }),
                            routing: RecordedEventRouting::Unrecorded,
                        },
                    },
                    JournalAppend {
                        iteration: 6,
                        phase: Phase::Apply,
                        ordinal: 1,
                        body: JournalBody::NamedEvent {
                            name: "plan.step.finished".to_string(),
                            agent_id: "agent-named-rail".to_string(),
                            // NOT the key's own principal/workspace, so a store
                            // that answered from the path instead of the record
                            // is a failure and not a coincidence.
                            principal: Some("principal-on-the-record".to_string()),
                            workspace: Some("workspace-on-the-record".to_string()),
                            payload: serde_json::json!({
                                "step_id": "s-1",
                                "status": "completed",
                                "note": "line\nbreak",
                            }),
                        },
                    },
                ],
            )
            .await
            .expect("append");

        let records = store.read_journal(&key, 0).await.expect("read");
        assert_eq!(records.len(), 2, "both rails, one batch, one sequence");
        assert_eq!(
            (records[0].seq, records[1].seq),
            (1, 2),
            "the store numbers a mixed batch gaplessly and in the order it was given, or a \
             projector's dedupe window sees a different list than the phase produced"
        );
        assert_eq!(
            records[1].event_key(key.execution_id()).to_string(),
            "named:6:apply:1",
            "the named record's address is the same shape an event's is; the shared address \
             space is what lets one dedupe window cover both rails"
        );

        let JournalBody::NamedEvent {
            name,
            agent_id,
            principal,
            workspace,
            payload,
        } = &records[1].body
        else {
            panic!(
                "the body must survive as a named event, got {:?}",
                records[1].body
            );
        };
        assert_eq!(name, "plan.step.finished");
        assert_eq!(agent_id, "agent-named-rail");
        assert_eq!(
            principal.as_deref(),
            Some("principal-on-the-record"),
            "the scope halves are omitted from the wire when they are `None`, so a store that \
             lost them round-trips `None` into `None` and nothing says a scoped envelope \
             replayed as an unscoped one"
        );
        assert_eq!(workspace.as_deref(), Some("workspace-on-the-record"));
        assert_eq!(payload["step_id"], "s-1");
        assert_eq!(
            payload["note"], "line\nbreak",
            "the filesystem store writes one record per line, so an unescaped newline tears \
             this record in half"
        );

        assert!(
            matches!(&records[0].body, JournalBody::Event { event_type, .. }
                if event_type == "AgenticActionExecuted"),
            "and the event beside it is unchanged by the rail added next to it, got {:?}",
            records[0].body
        );
    }

    /// A sink that keeps what it was handed, in order.
    ///
    /// Not a fixture shaped for an assertion: the cases below assert on how many
    /// DISTINCT addresses reached a transport, and a sink that counted calls
    /// rather than recording addresses would report a re-run's duplicate and its
    /// original as the same evidence.
    #[derive(Default)]
    struct RecordingSink {
        emitted: Vec<EventKey>,
    }

    impl ProjectedEventSink for RecordingSink {
        fn emit(
            &mut self,
            key: &EventKey,
            _event_type: &str,
            _payload: &serde_json::Value,
        ) -> Result<(), EmitRefused> {
            self.emitted.push(key.clone());
            Ok(())
        }

        /// Same address bookkeeping as [`Self::emit`], because the cases here
        /// are about the STORE — that a mark saved and reloaded still suppresses
        /// a re-run's duplicate — and that property is the same one whichever
        /// rail the record is on. Recording the two into one vector is what lets
        /// a case mix the rails and still assert on distinct addresses; keeping
        /// them apart would let a store that dropped one rail's records look
        /// identical to a run that produced none of them.
        fn emit_named(
            &mut self,
            key: &EventKey,
            _name: &str,
            _agent_id: &str,
            _principal: Option<&str>,
            _workspace: Option<&str>,
            _payload: &serde_json::Value,
        ) -> Result<(), EmitRefused> {
            self.emitted.push(key.clone());
            Ok(())
        }
    }

    /// The one event a phase would journal, addressed the way a phase addresses
    /// it.
    ///
    /// A re-run of the same phase produces this same value at a NEW seq, which is
    /// exactly the shape the dedupe window exists to recognise — so the two cases
    /// below build their re-run by appending this a second time rather than by
    /// hand-editing an address.
    fn iteration_started(iteration: usize) -> JournalAppend {
        JournalAppend {
            iteration,
            phase: Phase::Prepare,
            ordinal: 0,
            body: JournalBody::Event {
                event_type: "AgenticIterationStarted".to_string(),
                payload: serde_json::json!({ "iteration": iteration }),
                // Typed out rather than defaulted, which is what the field being
                // a required parameter on `JournalAppend::event` is for: these
                // cases are about the dedupe ADDRESS, not about routing, and
                // `Unrecorded` is the honest answer for a fixture that never had
                // an `ActionExecutors` to ask.
                routing: RecordedEventRouting::Unrecorded,
            },
        }
    }

    /// Append one batch and commit the watermark that vouches for it, the way
    /// `driver_worker::commit_boundary` does.
    ///
    /// Committing matters rather than being ceremony: `append_journal` numbers
    /// from the COMMITTED watermark and sweeps everything above it, so a second
    /// append with no commit in between would sweep the first record and start
    /// the numbering again — and a case that skipped the commit would be
    /// asserting about a log that no longer holds what it appended.
    async fn append_and_commit<S: LoopStateStore>(
        store: &S,
        key: &ExecutionKey,
        revision: Revision,
        appends: &[JournalAppend],
    ) -> (u64, Revision) {
        let seq = store
            .append_journal(key, appends)
            .await
            .expect("the store places the records");
        let mut state = fresh_state(key);
        state.journal_seq = seq;
        let revision = store
            .commit(key, &state, revision)
            .await
            .expect("the watermark commits");
        (seq, revision)
    }

    pub async fn an_execution_that_never_projected_has_no_mark_rather_than_a_mark_at_zero<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        // `None` and an error are different answers and a driver acts on the
        // difference: `None` means nothing has projected, so start at zero and
        // emit; an error means the mark could not be read, so emit NOTHING,
        // because starting at zero would re-deliver the whole run's timeline.
        // A store that answered `Ok(Some(ProjectorCursor::new()))` here would be
        // indistinguishable from one that had genuinely projected nothing — until
        // the first time it lost a mark, when it would silently look like a fresh
        // run instead of like a store that could not answer.
        let key = key("never-projected");
        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("a store that implements the mark can always be asked")
                .is_none(),
            "an execution nothing has projected must answer `None`"
        );
    }

    pub async fn a_projector_mark_survives_the_store_with_the_window_that_recognises_a_re_run<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        // The property is NOT "what I saved comes back". A store that persisted
        // `emitted_through_seq` alone — the obvious field, and the only one a
        // hand-rolled row would carry — passes an equality check on the mark and
        // then re-emits every duplicate a crash produces, because a re-run
        // appends the same address at a NEW seq and the high-water mark cannot
        // recognise it. So this case restores the mark and asks it about a real
        // re-run.
        let key = key("projector-mark");

        let (first_seq, revision) =
            append_and_commit(store, &key, Revision::INITIAL, &[iteration_started(1)]).await;
        let journal = Journal::from_records(
            store
                .read_journal(&key, 0)
                .await
                .expect("the log reads back"),
        )
        .expect("and parses");

        let mut cursor = ProjectorCursor::new();
        let mut sink = RecordingSink::default();
        let projected = cursor.project(&journal, key.execution_id(), first_seq, &mut sink);
        assert_eq!(projected.emitted, 1, "the one event is emitted once");
        store
            .save_projector_cursor(&key, &cursor)
            .await
            .expect("the mark saves");

        // A second worker picks the run up. It has no cursor of its own.
        let mut restored = store
            .load_projector_cursor(&key)
            .await
            .expect("the mark reads back")
            .expect("a saved mark is present");
        let again = restored.project(&journal, key.execution_id(), first_seq, &mut sink);
        assert_eq!(
            (again.emitted, again.deduped),
            (0, 0),
            "a restored mark must not re-walk what it already passed"
        );

        // Now the crash case the window exists for: the phase re-runs and appends
        // the SAME address at a NEW seq.
        let (second_seq, _) =
            append_and_commit(store, &key, revision, &[iteration_started(1)]).await;
        assert!(
            second_seq > first_seq,
            "a re-run appends rather than rewriting: {second_seq} must be past {first_seq}"
        );
        let journal = Journal::from_records(
            store
                .read_journal(&key, 0)
                .await
                .expect("the log reads back"),
        )
        .expect("and parses");
        let rerun = restored.project(&journal, key.execution_id(), second_seq, &mut sink);
        assert_eq!(
            (rerun.emitted, rerun.deduped),
            (0, 1),
            "the re-run's duplicate must be recognised by the restored window, not emitted again"
        );

        assert_eq!(
            sink.emitted.len(),
            1,
            "one address reached the transport across a restore and a re-run, got {:?}",
            sink.emitted
        );
    }

    pub async fn a_chain_closure_is_absent_until_it_is_published_and_names_its_own_segment<
        S: LoopStateStore,
    >(
        store: &S,
    ) {
        // A receipt is evidence BY EXISTING, so the two halves this pins are the
        // two a reader acts on: nothing has published one yet, and the one that
        // comes back is the one that was written — for the segment it was
        // written for.
        //
        // The second half is not paranoia about serialisation. `ChainClosure`
        // is read by the reconciler to license a permanent, non-resumable
        // terminal on a DIFFERENT run, so a receipt recovered from the wrong
        // directory — a copied execution tree, a restore that renamed a run —
        // would license it about a child nobody asked about. `closes` is the
        // check that stops that, and it is worth nothing unless the store round
        // trips the field it reads.
        let never_closed = key("chain-never-closed");
        assert!(
            store
                .load_chain_closure(&never_closed)
                .await
                .expect("a store can always be asked whether a receipt exists")
                .is_none(),
            "a segment nothing has closed must answer `None`, never a fabricated receipt"
        );

        let closed = key("chain-closed");
        let receipt = ChainClosure::for_segment(closed.execution_id(), 1_700_000_000_000);
        store
            .record_chain_closure(&closed, &receipt)
            .await
            .expect("the receipt publishes");

        let read_back = store
            .load_chain_closure(&closed)
            .await
            .expect("and reads back")
            .expect("a published receipt is present");
        assert_eq!(
            read_back, receipt,
            "the receipt must survive the store intact"
        );
        assert!(
            read_back.closes(&closed),
            "the receipt must name the segment it was published for"
        );
        assert!(
            !read_back.closes(&never_closed),
            "and must not answer for any other segment"
        );

        // Publishing again is the crash-retry path, not a conflict: the writer
        // is the run that already ended, and every writer of this value for one
        // segment writes the same value.
        store
            .record_chain_closure(&closed, &receipt)
            .await
            .expect("a second publication is not a conflict");
        assert_eq!(
            store
                .load_chain_closure(&closed)
                .await
                .expect("and reads back")
                .expect("still present"),
            receipt
        );
    }

    pub async fn a_second_handle_sees_everything_the_first_one_committed<H: ContractHarness>(
        harness: &H,
    ) {
        // These stores are addressed by a substrate, not by a handle. An
        // implementation that kept any of this where only the writing handle
        // could see it would pass every other case in the suite and then lose the
        // execution the moment a second worker asked — which is the entire
        // premise of a loop no process owns.
        let key = key("second-handle");
        let store = harness.store();
        let pending = unsafe_effect("llm-1:tool:call-1");

        let seq = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        let mut state = fresh_state(&key);
        state.journal_seq = seq;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Observe,
        };
        let revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");
        store
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 1_000),
            )
            .await
            .expect("intent");
        store
            .claim(&key, &WorkerId::new("worker-a"), Duration::from_secs(300))
            .await
            .expect("claim");

        let reopened = harness.reopen();

        let loaded = reopened
            .load(&key)
            .await
            .expect("load")
            .expect("the committed state belongs to the substrate, not to the handle");
        assert_eq!(loaded.revision, revision);
        assert_eq!(loaded.state, state);
        assert_eq!(
            reopened.read_journal(&key, 0).await.expect("read").len(),
            1,
            "the log is not the writing handle's private memory"
        );
        assert!(
            reopened
                .load_effects(&key)
                .await
                .expect("effects")
                .get(&pending.effect_id)
                .is_some(),
            "an intent a second handle cannot see is an effect that fires twice"
        );

        let error = reopened
            .claim(&key, &WorkerId::new("worker-b"), Duration::from_secs(300))
            .await
            .expect_err("a lease taken through one handle must exclude a worker at another");
        assert!(matches!(error, StoreError::LeaseHeld { .. }), "got {error}");

        let error = reopened
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("the second handle must see a committed revision, not an empty store");
        assert!(matches!(error, StoreError::Conflict { .. }), "got {error}");
    }
}

/// Run the whole [`contract`] suite against one implementation.
///
/// One line per implementation, which is the property the design asks for. Each
/// case becomes its own `#[tokio::test]` so a failure names the property that
/// broke rather than "the contract suite failed".
#[cfg(test)]
macro_rules! loop_state_store_contract {
    ($harness:ty) => {
        loop_state_store_contract!(
            @cases $harness,
            an_execution_that_never_committed_loads_as_nothing,
            a_commit_publishes_a_revision_a_load_reads_back,
            a_stale_holder_cannot_clobber_a_newer_commit,
            a_second_first_commit_is_a_conflict_not_an_overwrite,
            the_store_numbers_journal_records_gaplessly,
            an_orphaned_attempt_is_swept_before_the_next_one_appends,
            replay_of_the_committed_prefix_reaches_the_committed_cursor,
            a_crash_at_every_commit_boundary_resumes_at_the_same_cursor,
            a_re_run_after_a_crash_does_not_disturb_a_settled_effect,
            an_effect_with_an_intent_and_no_outcome_reads_as_no_answer,
            a_settled_effect_is_adopted_rather_than_fired_again,
            a_crash_between_intent_and_fire_does_not_re_fire_a_live_send,
            a_live_lease_excludes_every_claimant_until_it_expires,
            an_expired_lease_may_be_taken_and_its_holder_is_then_refused,
            a_superseded_fence_cannot_mutate_state_or_journal,
            releasing_a_lease_lets_the_next_worker_in,
            list_runnable_offers_a_committed_execution,
            list_runnable_withholds_work_that_is_not_ready_and_offers_expired_work,
            list_runnable_withholds_a_run_that_ended_for_good,
            terminal_outbox_debt_stays_runnable_until_its_cursor_advances,
            eventless_cannot_proceed_remains_debt_until_runtime_settlement,
            a_scan_pages_past_work_no_claim_can_change,
            a_scan_that_reached_the_end_says_so_and_a_short_one_does_not,
            an_ending_is_retracted_when_the_prefix_no_longer_ends_on_one,
            the_paged_scan_and_the_flat_listing_answer_the_same_question,
            a_resolved_wake_makes_a_parked_execution_runnable_again,
            a_second_park_on_one_token_waits_for_a_second_completion,
            a_resolution_that_arrives_before_the_park_still_satisfies_it,
            a_completion_landing_during_a_wake_is_not_consumed_with_it,
            a_live_lease_hides_the_work_from_every_worker,
            list_parked_offers_parked_work_and_nothing_else,
            list_parked_still_offers_a_park_whose_wake_resolved,
            list_parked_reports_a_pass_that_stopped_short,
            a_parked_run_carries_when_it_parked_and_its_last_lease,
            a_batch_that_would_not_validate_is_refused_at_commit,
            a_watermark_beyond_the_log_is_refused,
            an_event_record_survives_the_store_intact,
            a_named_record_survives_the_store_intact,
            the_checked_read_answers_with_the_log_the_raw_read_does,
            an_execution_that_never_projected_has_no_mark_rather_than_a_mark_at_zero,
            a_projector_mark_survives_the_store_with_the_window_that_recognises_a_re_run,
            a_chain_closure_is_absent_until_it_is_published_and_names_its_own_segment,
        );
        // Cases that need the harness rather than one store, because the
        // property is about a second handle onto the same substrate.
        loop_state_store_contract!(
            @harness_cases $harness,
            a_second_handle_sees_everything_the_first_one_committed,
        );
    };
    (@harness_cases $harness:ty, $($case:ident,)+) => {
        $(
            #[tokio::test]
            async fn $case() {
                use $crate::magician_v2::execution::agentic::run_loop::store::contract::
                    ContractHarness;
                let harness = <$harness as ContractHarness>::create();
                $crate::magician_v2::execution::agentic::run_loop::store::contract::$case(
                    &harness,
                )
                .await;
            }
        )+
    };
    (@cases $harness:ty, $($case:ident,)+) => {
        $(
            #[tokio::test]
            async fn $case() {
                use $crate::magician_v2::execution::agentic::run_loop::store::contract::
                    ContractHarness;
                let harness = <$harness as ContractHarness>::create();
                $crate::magician_v2::execution::agentic::run_loop::store::contract::$case(
                    harness.store(),
                )
                .await;
            }
        )+
    };
}

#[cfg(test)]
pub(crate) use loop_state_store_contract;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_execution_id_that_could_escape_its_directory_is_refused_not_sanitized() {
        // Sanitizing would map two distinct executions onto one directory, where
        // they would share a journal and a lease. A rejected id is a bug report;
        // a merged pair of runs is a silent corruption.
        for hostile in ["../secrets", "a/b", ".", "..", ".hidden", "a\0b", ""] {
            assert!(
                ExecutionKey::new("owner", "default", hostile).is_err(),
                "{hostile:?} must be refused"
            );
        }
        assert!(ExecutionKey::new("owner", "default", "exec-1_ok.2").is_ok());
    }

    #[test]
    fn a_key_that_arrives_as_json_meets_the_same_refusal_as_one_built_in_code() {
        // The store writes a key record beside every execution and reads it back
        // on every scan, then joins the recovered id into a path. A derived
        // `Deserialize` would have rebuilt the struct field by field, so the
        // refusal `ExecutionKey::new` performs would have applied to code and not
        // to the disk it reads.
        for hostile in ["../secrets", "a/b", ".", "..", ".hidden", ""] {
            let raw = serde_json::json!({
                "principal": "owner",
                "workspace": "default",
                "execution_id": hostile,
            });
            assert!(
                serde_json::from_value::<ExecutionKey>(raw).is_err(),
                "{hostile:?} must be refused when it arrives as JSON too"
            );
        }

        let key = ExecutionKey::new("owner", "default", "exec-1").expect("well-formed");
        let encoded = serde_json::to_string(&key).expect("a key encodes");
        assert_eq!(
            serde_json::from_str::<ExecutionKey>(&encoded).expect("and decodes"),
            key,
            "the refusal must not cost the round trip the store depends on"
        );
    }

    #[test]
    fn every_contract_case_is_registered_with_the_macro() {
        // A case the macro does not name never runs — against any implementation
        // — and nothing else notices: the function still compiles and its `pub`
        // in a public module keeps dead-code quiet. Adding an implementation
        // being one line is what makes a forgotten line invisible.
        let source = include_str!("mod.rs");
        let after_macro = source
            .split_once("macro_rules! loop_state_store_contract")
            .expect("the macro is declared in this file")
            .1;
        let macro_body = after_macro
            .split_once("pub(crate) use loop_state_store_contract")
            .map_or(after_macro, |(body, _)| body);
        let registered: std::collections::HashSet<&str> = macro_body
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_suffix(','))
            .collect();

        let declared: Vec<&str> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub async fn "))
            .map(|rest| {
                rest.split(|character| character == '<' || character == '(')
                    .next()
                    .unwrap_or(rest)
            })
            .collect();
        assert!(
            declared.len() >= 20,
            "the extractor found only {} cases, so it has stopped matching the declarations it \
             is supposed to police",
            declared.len()
        );

        for case in declared {
            assert!(
                registered.contains(case),
                "the contract case {case} is declared but never registered with \
                 loop_state_store_contract!, so it runs against no implementation at all"
            );
        }
    }

    #[test]
    fn a_key_needs_a_scope() {
        assert_eq!(
            ExecutionKey::new("", "default", "exec-1"),
            Err(KeyError::EmptyScope { field: "principal" })
        );
        assert_eq!(
            ExecutionKey::new("owner", "   ", "exec-1"),
            Err(KeyError::EmptyScope { field: "workspace" })
        );
    }

    #[test]
    fn an_execution_id_is_length_bounded() {
        let long = "e".repeat(MAX_EXECUTION_ID_BYTES + 1);
        assert!(matches!(
            ExecutionKey::new("owner", "default", long),
            Err(KeyError::ExecutionIdTooLong { .. })
        ));
    }

    #[test]
    fn the_initial_revision_is_nothing_committed_rather_than_a_committed_zero() {
        assert_eq!(Revision::INITIAL.as_u64(), 0);
        assert_eq!(Revision::INITIAL.next(), Revision::from_u64(1));
        assert_eq!(Revision::INITIAL.to_string(), "r0");
    }

    #[test]
    fn a_lease_expires_at_its_deadline_rather_than_after_it() {
        let lease = Lease {
            key: ExecutionKey::new("owner", "default", "exec-1").expect("well-formed"),
            worker: WorkerId::new("worker-a"),
            fence: 4,
            expires_at_ms: 1_000,
        };
        assert!(!lease.is_expired_at(999));
        assert!(
            lease.is_expired_at(1_000),
            "a lease whose deadline is now is over; treating it as live would let a zero TTL \
             hold forever"
        );
    }
}
