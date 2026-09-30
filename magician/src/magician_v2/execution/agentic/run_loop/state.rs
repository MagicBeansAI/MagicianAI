//! The run's own state, as a value rather than as ambient process memory.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`. The goal that document
//! serves is that **any worker process can advance any execution** by loading
//! state from a store, running one bounded unit of work, and committing. That is
//! only possible if the state a run depends on is a value someone can be handed.
//! Today much of it is ambient: interior-mutable fields on `ActionExecutors` and
//! `AgenticContext`, plus locals on the stack of a resident tokio task.
//!
//! This module is where that state moves to, one landable increment at a time.
//! The design is explicit about the order — *"the scratch extraction lands first
//! and is verifiable on its own (the in-process loop keeps working, state just
//! lives somewhere honest)"* — so nothing here changes behaviour. It changes
//! **where the truth lives**.

use serde::{Deserialize, Serialize};

use crate::magician_v2::work_context::WorkAuthorityRef;

/// Who this run is, and on whose behalf it acts.
///
/// The identity block of the design's `LoopState`, extracted first because it is
/// the part a second holder must be **told** rather than allowed to inherit. A
/// worker that picks up an execution has no ambient process memory to read it
/// from; handing it a `RunIdentity` is the whole point.
///
/// # What this replaced
///
/// Twelve separate `Arc<Mutex<…>>` fields on `ActionExecutors`, each set by hand
/// at every entry point that starts or resumes a run. They were never
/// independent state: the seeding blocks copied nine of them straight out of
/// `AgenticContext`, so the executors held a **second copy** of identity that
/// already existed, kept in step by hand. Any entry point that forgot one left
/// the two disagreeing, with no type able to notice.
///
/// # Runtime-owned, never model-supplied
///
/// Several of these carry that property individually today —
/// `chat_session_id` and `work_authority` say so at their declarations — and it
/// survives the move: this value is built from a context the runtime stamped, and
/// there is no path that constructs one from tool arguments. Keeping them in one
/// type makes that easier to hold, not harder, because there is now a single
/// place a reviewer must check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    /// Task this run serves, for provenance in durable artifact frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,

    /// The execution itself, for the same provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,

    /// The agent acting. Backstopped from the owner snapshot when the context's
    /// own `agent_id` is absent — see [`RunIdentity::apply_context`], which is
    /// where that rule now lives instead of at the call site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Principal for execution-scoped V3 storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,

    /// Workspace for execution-scoped V3 storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// The originating chat session, authoritative for task and tool lineage.
    /// Runtime-owned; never accepted from model arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,

    /// UI thread for pack capability provenance.
    ///
    /// The one member not derivable from the context: it is resolved by an async
    /// lookup against the artifact service. [`RunIdentity::apply_context`]
    /// therefore leaves it `None` and the caller sets it once resolved, which is
    /// the same two-step the seeding block already performed — now visible in the
    /// type rather than implied by statement order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread: Option<String>,

    /// Work authority of this execution (§4.2c carrier). Stamped from the
    /// context, never accepted from model arguments. Absent means **no
    /// authority**, never "unrestricted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_authority: Option<WorkAuthorityRef>,

    /// User goal, for cache-stable primitive seed-goal rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_goal: Option<String>,

    /// Success criteria, for the same rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_success_criteria: Option<String>,

    /// Per-execution task spawn cap for autonomous cycles.
    ///
    /// **Required on the wire — no `default`, no `skip_serializing_if`.** This is
    /// a CEILING, and `create_task` enforces it only under `if let Some(..)`, so
    /// an absent field is an *unenforced* cap rather than an unset one. With
    /// `#[serde(default)]` a record that simply lacked this key would resume with
    /// no cap at all, and no reader could tell "declared nothing" from "field
    /// dropped".
    ///
    /// `grants.rs` states the rule this obeys: *"A ceiling narrows. Losing one
    /// widens the run, so it has to be carried."* Making it required turns a lost
    /// ceiling into a parse failure — the run refuses to load rather than loading
    /// unrestricted, which is the fail-closed direction.
    ///
    /// # Removing `#[serde(default)]` was NOT enough, and this is why
    ///
    /// serde's derive answers a **missing `Option<T>` field with `None`** whether
    /// or not `#[serde(default)]` is present: the generated code routes a missing
    /// field through `serde::__private::de::missing_field`, whose deserializer
    /// implements `deserialize_option` and answers `visit_none`. So this field
    /// went on loading as "no cap" from a record that simply lacked the key, and
    /// `a_ceiling_cannot_go_missing_from_the_wire` — which asserts by deleting
    /// the key from a real encoding — caught it.
    ///
    /// `deserialize_with` is what fixes it, and it fixes it structurally rather
    /// than by adding a check: with a custom deserializer serde cannot use
    /// `missing_field` (which needs `Deserialize`), so it emits a hard
    /// `Error::missing_field` instead. The function below deserializes exactly
    /// what the derive would; its only job is to change that codegen branch.
    ///
    /// An explicit `null` still means *declared nothing*, which is the
    /// distinction the whole rule is about: absent is a dropped key and refuses,
    /// `null` is a run that genuinely has no cap and loads.
    #[serde(deserialize_with = "deserialize_present_optional_cap")]
    pub max_spawned_tasks: Option<u32>,

    /// Browser-transport ceiling of the CURRENT owner agent, so the flat
    /// `PrimitiveExecCtx` builder — which sees only the executors, never the
    /// context — can carry it into browser dispatch.
    ///
    /// Empty means unrestricted. **Replaced, never merged**: a delegate must not
    /// inherit its parent's wider ceiling.
    ///
    /// **Required on the wire**, for the same reason as `max_spawned_tasks` and
    /// with a sharper edge: `BrowserTransportCeiling::parse(&[])` yields
    /// `allowed: None` and permits every transport. A dropped key here does not
    /// merely fail to narrow — it hands a run pinned to one transport the full
    /// set, which is precisely the widening
    /// `set_executor_browser_transports` recovers from a poisoned lock to
    /// prevent. Reintroducing it at the serialization boundary would undo that.
    pub browser_transports: Vec<String>,
}

/// Deserialize an `Option` field that must nevertheless be **present**.
///
/// Delegates to the derive's own behaviour for the value; its whole purpose is
/// the branch it forces in the generated code. See
/// [`RunIdentity::max_spawned_tasks`] for why the attribute is load-bearing and
/// what removing it re-opens.
fn deserialize_present_optional_cap<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u32>::deserialize(deserializer)
}

impl RunIdentity {
    /// Overwrite exactly the fields the context owns, leaving the rest.
    ///
    /// Separate from [`Self::from_context`] because three members are **not**
    /// the context's to answer for, and a wholesale replace would erase them:
    ///
    /// - `ui_thread` is resolved by an async lookup against the artifact service.
    /// - `primitive_goal` and `primitive_success_criteria` are written later in
    ///   the same setup, by the primitive prompt-context refresh.
    ///
    /// The seeding path runs before all three, so replacing the whole value there
    /// would blank a field that a previous segment had correctly set and that
    /// nothing on this path sets again. Applying only what the context knows is
    /// what the twelve separate writes did, one field at a time; this keeps that
    /// exact meaning while making it a single atomic swap.
    pub fn apply_context(&mut self, ctx: &super::super::types::AgenticContext) {
        self.task_id = ctx.task_id.clone();
        self.execution_id =
            super::super::executor::runtime_execution_id_opt(ctx).map(str::to_string);
        // The agent backstop: a delegated child can have `ctx.agent_id == None`
        // while its owner snapshot knows the active agent. Both the flat
        // `build_primitive_exec_ctx` path and the legacy enriched-params dispatch
        // read this when injecting `__agent_id` for bare compiled handlers, so it
        // must be `Some` wherever an owner is known.
        self.agent_id = ctx
            .agent_id
            .clone()
            .or_else(|| ctx.active_owner_agent_id.clone());
        self.principal = ctx.principal.clone();
        self.workspace = ctx.workspace.clone();
        self.chat_session_id = ctx.chat_session_id.clone();
        self.work_authority = ctx.work_authority.clone();
        self.max_spawned_tasks = ctx.max_spawned_tasks;
        // Replaced, never merged — a delegate must not inherit its parent's
        // wider ceiling.
        self.browser_transports = ctx.browser_transports.clone();
    }
}

/// Explicit, unambiguous ownership of one generated loop-state address.
///
/// Generated execution ids may themselves contain every suffix token used by
/// the stateless address grammar, so parsing `-r`, `-n`, `-p`, or `-s...-a...`
/// can never prove which runtime execution owns a segment. The producer writes
/// both values while it still has them separately; lifecycle consumers compare
/// them verbatim and never reverse the spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopSegmentBinding {
    pub base_execution_id: String,
    pub exact_segment_id: String,
    /// Opaque durable admission for the narrow restart-before-first-snapshot
    /// window. It is consumed by the first snapshot commit and never grants
    /// authority on later revisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preseed_admission_token: Option<String>,
}

/// The identity as the executors hold it: shared, swappable, cheap to read.
///
/// # Why `Arc<RunIdentity>` inside the lock rather than `RunIdentity`
///
/// This is read on essentially every dispatch and written only at the handful of
/// entry points that start, resume, or hand over a run. Holding the value
/// directly would make each read deep-clone up to twelve `String`s while holding
/// the lock; holding an `Arc` makes a read a refcount bump and a fast unlock.
///
/// Cheaper in LOCKS than what it replaced: code needing principal *and*
/// workspace *and* task id took three locks; it now takes one.
///
/// Not cheaper in allocations, and an earlier version of this comment said
/// otherwise. A caller that needs owned `String`s still clones them out of the
/// snapshot — what it no longer does is take a separate lock for each. The
/// refcount bump replaces the LOCKING, not the copying.
///
/// # Why one lock is safe where twelve were
///
/// Collapsing twelve mutexes into one means a single poisoning now affects every
/// field rather than one. That is why [`ExecutorRunIdentity::update`] **recovers**
/// from poisoning and writes, rather than skipping the write.
///
/// The distinction is load-bearing and was already inconsistent before this
/// type: `set_executor_browser_transports` recovered on purpose — *"leaving a
/// previous owner's ceiling in place across a transition would let a narrow
/// delegate keep a wide predecessor's transports"* — while
/// `set_executor_current_agent` beside it silently skipped. Under one lock the
/// skipping policy would extend that widening to the ceiling, so the recovering
/// policy is the only correct one: for a ceiling a stale value is a WIDENING, and
/// for the rest it is provenance attributed to the wrong run. Both are worse than
/// proceeding with a lock whose previous holder panicked.
#[derive(Debug, Clone, Default)]
pub struct ExecutorRunIdentity {
    inner: std::sync::Arc<std::sync::Mutex<std::sync::Arc<RunIdentity>>>,
}

impl ExecutorRunIdentity {
    /// Read the current identity. One lock, no string allocation.
    pub fn get(&self) -> std::sync::Arc<RunIdentity> {
        std::sync::Arc::clone(
            &self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    // `set` — a wholesale replace — lived here until the review that landed this
    // type. Nothing called it: every production write goes through `update`, and
    // `update` can express a replace anyway. A worker installing an identity
    // loaded from a store is the caller it was written for, and that worker does
    // not exist yet; it can come back then, with its caller. The same goes for
    // `RunIdentity::from_context`, which was `default()` plus `apply_context`
    // spelled twice.

    /// Apply a narrow edit to the identity in place.
    ///
    /// For the genuine single-field transitions — an owner handover that changes
    /// only the agent and its ceiling, or the `ui_thread` that resolves after the
    /// rest is known. Copy-on-write: readers holding an older `Arc` keep a
    /// consistent snapshot rather than observing a half-applied change.
    pub fn update(&self, edit: impl FnOnce(&mut RunIdentity)) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = (**guard).clone();
        edit(&mut next);
        *guard = std::sync::Arc::new(next);
    }
}

// ============================================================================
// The committed loop state
// ============================================================================

/// Where an execution is, precisely enough for a different process to take over.
///
/// # This is `LoopState` as far as it exists, and no further
///
/// The design's `LoopState` has five blocks: identity, cursor, goal and
/// conversation, budgets and authority, and the extracted scratch. Three of them
/// are here. The conversation, the environment and the authority ceilings are
/// **not**, and inventing fields for them now would be worse than leaving them
/// out: `messages` and `environment` are specified as *references* into their own
/// logs precisely because embedding them makes every commit O(n) in the run's
/// history, and the authority block cannot land until the scratch extraction has
/// made it a value.
///
/// So what this type is, exactly: the part of the loop's state that a **store**
/// must understand — to schedule the execution, to lease it, to bound it, to
/// know what it was in the middle of. Everything else can join it later without
/// changing any of the store machinery built on this, because the store never
/// interprets those fields.
///
/// # What may never appear here
///
/// Three shapes wear the `RUN` label in
/// `docs/archive/plans/2026-08-26-scratch-extraction-field-audit.md` and only one of them
/// belongs in a boundary record. The other two are named here so a later edit has
/// to argue with a comment rather than only with a reviewer:
///
/// - **Grants**, not ceilings. [`super::grants::RunGrants`] derives neither
///   `Serialize` nor `Deserialize`, deliberately: a lost grant re-prompts, which
///   fails closed, and a forged one hands a run filesystem access nobody
///   approved. Embedding it would not compile, and that is the design working.
/// - **In-flight handoffs.** Server-labeled tool results and the shell stream
///   context never outlive the operation that creates them. Persisting one would
///   carry bytes the loop's own contract keeps out of every sink and restore a
///   handoff nobody is waiting to take.
/// - **Inbound channels.** The manual pause signal and the steer queue are how
///   something *outside* reaches a run in flight. The queue's *contents* are
///   state and deserve a store-backed inbox; the `Arc` is a delivery mechanism
///   and a worker cannot be handed one as a value.
/// Versioned, bounded payload required to resume the Resolve/Apply half of one
/// iteration in a different process.
///
/// The state store treats the payload as opaque on purpose.  Phase-owned Rust
/// types evolve independently of the scheduler, while the version lets the
/// host refuse an incompatible payload instead of guessing.  Capability
/// grants and ephemeral credentials are excluded by the producer. Primitive
/// decisions are not checkpointed because their runtime proof object has no
/// stable serialized identity; other governed implementations are re-resolved
/// from the live registry and matched against an exact contract digest before
/// the payload is admitted back into execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveCheckpoint {
    pub version: u16,
    pub payload: serde_json::Value,
}

impl ResolveCheckpoint {
    /// Version 2 makes checkpoint admission fail closed at the authority
    /// boundary: a producer records exact governed capability contracts and
    /// refuses to publish a capsule that depends on a non-durable credential.
    /// Version 1 capsules cannot prove either property and are therefore not
    /// recoverable by this runtime.
    pub const VERSION: u16 = 2;
    pub const MAX_BYTES: usize = 4 * 1024 * 1024;

    pub fn try_new(payload: serde_json::Value) -> Result<Self, String> {
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("resolve checkpoint could not be encoded: {error}"))?;
        if bytes.len() > Self::MAX_BYTES {
            return Err(format!(
                "resolve checkpoint is {} bytes, above the {} byte boundary",
                bytes.len(),
                Self::MAX_BYTES
            ));
        }
        Ok(Self {
            version: Self::VERSION,
            payload,
        })
    }

    pub fn require_current(&self) -> Result<&serde_json::Value, String> {
        if self.version != Self::VERSION {
            return Err(format!(
                "resolve checkpoint version {} is not supported by version {}",
                self.version,
                Self::VERSION
            ));
        }
        // `try_new` is only a producer-side admission check. Deserialization
        // constructs this type without calling it, so trusting that earlier
        // check would let a hand-written, migrated or corrupted state bypass the
        // boundary and make `restore_resolve_checkpoint` clone/decode an
        // arbitrarily large value. Re-measure the value that was actually read
        // before returning any part of it to the executor.
        let bytes = serde_json::to_vec(&self.payload)
            .map_err(|error| format!("resolve checkpoint could not be encoded: {error}"))?;
        if bytes.len() > Self::MAX_BYTES {
            return Err(format!(
                "resolve checkpoint is {} bytes, above the {} byte boundary",
                bytes.len(),
                Self::MAX_BYTES
            ));
        }
        Ok(&self.payload)
    }
}

/// Bounded Artifact/runtime projection captured before a receipt-owned loop
/// ending becomes authoritative. That includes final terminals plus
/// receipt-backed user/manual pauses whose exact continuation is published by
/// the terminal lifecycle. The full `AgenticOutcome` is intentionally not
/// retained in LoopState: browser snapshots and recursive pause bodies can be
/// large and may contain protected app data. This is the exact, already
/// disclosure-filtered lifecycle projection that the ordinary outcome owner
/// would have persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalOutcomeProjection {
    pub execution_status: String,
    pub task_status: String,
    pub outcome_type: String,
    pub outcome_summary: String,
    pub iterations_used: Option<usize>,
    pub is_terminal: bool,
    /// Full or partial delivery, carried through the sealed receipt so the
    /// settlement that replays it does not write `None` over the kind the
    /// first terminal flip recorded. Skipped when absent so receipts sealed
    /// before the field existed re-serialize byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<crate::magician_v2::execution::agentic::types::CompletionKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
}

/// Exact identity and generation covered by a terminal-settlement seal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSettlementDescriptor {
    pub schema_version: u32,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub base_execution_id: String,
    pub exact_segment_id: String,
    pub terminal_kind: String,
    pub terminal_seq: u64,
    pub outcome: TerminalOutcomeProjection,
    /// HMAC-bound live delegated-child policy bit: a WaitingForUser the
    /// delegated child may suspend on — a staged DiffApproval id or any
    /// owner-answerable input (`delegated_waiting_for_user_should_suspend`
    /// is the one predicate every stamping and reading site uses). The field
    /// keeps its original name for receipt compatibility. It remains
    /// available after the exact pause body is legitimately consumed into
    /// its successor segment.
    #[serde(default, skip_serializing_if = "pause_bool_is_false")]
    pub waiting_for_user_diff_approval: bool,
    /// Fixed-roster address axis carried by the exact pause body. The resume
    /// segment is sealed beside it so adoption cannot confuse two stages of the
    /// same root that paused at the same iteration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_loop_state_segment:
        Option<crate::magician_v2::execution::agentic::PipelineLoopStateSegment>,
    /// Protected, bounded stage-deliverable sidecar captured before a
    /// fixed-roster stage's inner terminal CAS. The outer receipt HMAC binds
    /// this small content-addressed reference; the independently scope-HMACed
    /// sidecar owns the inner-terminal -> outer-progress crash gap without
    /// retaining deliverable bytes in LoopState.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_stage_settlement:
        Option<crate::magician_v2::artifact_v2::pipeline_terminal_settlement::PipelineTerminalSettlementRef>,
    /// Exact sealed FullPauseStore authority prepared before committing a
    /// resumable max-iteration/budget boundary. All three fields are absent for
    /// a true terminal and present together for a continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_body_sha256: Option<String>,
    /// Exact successor segment encoded by the prepared pause. Fixed-roster
    /// pipeline recovery uses this value for its outer paused-stage CAS rather
    /// than reconstructing an address from naming conventions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_resume_segment: Option<String>,
}

fn pause_bool_is_false(value: &bool) -> bool {
    !*value
}

/// Integrity-sealed terminal outcome receipt committed in the same LoopState
/// CAS that publishes the `RunEnded` watermark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSettlementReceipt {
    pub schema_version: u32,
    pub hmac_sha256: String,
    pub descriptor: TerminalSettlementDescriptor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
    /// Who this run is and on whose behalf it acts.
    ///
    /// **Required**, not `#[serde(default)]`. The identity carries two ceilings,
    /// and `RunIdentity::default()` is unrestricted in both: empty
    /// `browser_transports` permits every transport and an absent
    /// `max_spawned_tasks` is an unenforced cap. A record that lost the whole
    /// block would therefore have resumed a run with no browser ceiling and no
    /// spawn cap — a total widening from a single missing key, and one nothing
    /// downstream could detect.
    pub identity: RunIdentity,

    /// Runtime execution + exact segment binding captured at seed time.
    /// Compatibility reads leave this absent, but security-bearing lifecycle
    /// settlement and exact wake recovery fail closed until a current producer
    /// has written it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_binding: Option<LoopSegmentBinding>,

    /// Bounded, exact continuation captured at the most recent successful
    /// phase boundary. A cold claimant can encounter a still-live placement
    /// pin before it runs any phase, so process-local history is not recovery
    /// evidence; this checkpoint is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_checkpoint: Option<IterationContinuationCheckpoint>,

    /// Exact bounded outcome proof for the receipt-owned ending at
    /// `journal_seq`. This includes final terminals and user/manual pauses whose
    /// continuation is staged outside this immutable source segment. It is
    /// produced by the Artifact owner after the terminal journal append receives
    /// its seq and before the fenced LoopState commit. Compatibility states may
    /// omit it; ordinary terminal settlement then fails closed instead of
    /// inventing an outcome from `TerminalKind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_settlement_receipt: Option<TerminalSettlementReceipt>,

    /// Which iteration and which phase.
    ///
    /// **Required on the wire**, and for the same reason as every other required
    /// field here: [`LoopCursor::default`] is iteration one, first phase, so a
    /// record that merely lost this key would resume an execution nine
    /// iterations in **from the beginning** — re-spending every LLM call it had
    /// already paid for. The effect ledger stops the re-run from re-firing a
    /// settled effect; nothing stops it from re-deciding.
    pub cursor: LoopCursor,

    /// The committed journal watermark.
    ///
    /// Records at or below this are authoritative; records beyond it are an
    /// orphaned attempt by a worker that appended and then failed to commit. See
    /// [`super::journal`] — this field is the reason that module exists in the
    /// shape it does.
    ///
    /// **Required on the wire, and this is the field where absence costs most.**
    /// A dropped key reads as zero, zero means *nothing committed*, and
    /// `Journal::orphaned(0)` is therefore **the entire log** — so the next
    /// `append_journal` sweeps every record the run ever committed into
    /// `journal.swept.jsonl` and starts the numbering again. That is not a
    /// widening, it is history loss, and it is triggered by one missing key on a
    /// field whose default is indistinguishable from a legitimate value.
    pub journal_seq: u64,

    /// Whether any worker may take this execution, or only one.
    ///
    /// **Required on the wire — no `default`, no `skip_serializing_if`**, for
    /// the same reason as [`RunIdentity::max_spawned_tasks`] and
    /// [`RunIdentity::browser_transports`]: absence widens. A record that lost
    /// this key would deserialize as [`Placement::Portable`] — the class any
    /// worker may claim — so a single dropped key would offer an execution that
    /// holds a process-local resource to a worker that cannot reach it.
    ///
    /// `claimable_by` is already wired into both stores' `list_runnable`
    /// (`store/fs.rs`, `store/memory.rs`), so the scheduling half reads this
    /// field on every scan. Making it required turns a lost placement into a
    /// parse failure — the run refuses to load rather than loading claimable by
    /// everyone.
    pub placement: Placement,

    /// Why this execution is not runnable, when it is not.
    ///
    /// `None` is the common case and means "runnable now, subject to
    /// [`Self::runnable_at_ms`]".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<WaitReason>,

    /// The earliest wall-clock time this execution should be picked up again.
    ///
    /// This is where [`super::outcome::BoundaryOutcome::Retry`] lands. The
    /// resident driver sleeps for the backoff; a worker must not, because
    /// sleeping holds a worker for the whole delay — the resident-task cost this
    /// whole design exists to remove. Committing the deadline turns the wait into
    /// a value the scheduler honours instead.
    ///
    /// **Required on the wire.** Zero means *runnable now*, so a record that
    /// lost this key would be offered immediately however long a backoff it had
    /// just committed — which turns the one mechanism that bounds a retry loop
    /// into a no-op, silently.
    pub runnable_at_ms: i64,

    /// The wall-clock deadline, checked at phase entry.
    ///
    /// Replaces the spawned watchdog, which is a `tokio::spawn` plus a sleep
    /// racing a finished-signal — and which **dies with its worker**, so under a
    /// stateless driver it would silently stop enforcing. A stored deadline
    /// survives a handoff.
    ///
    /// `None` means no wall-clock deadline, which is different from a deadline in
    /// the past. Every reader must keep them apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,

    /// Active time charged so far, in milliseconds.
    ///
    /// Committed at every boundary rather than derived from an `Instant`, and the
    /// reason is a settled finding rather than a preference:
    /// `context_work_budget_elapsed_ms` is `consumed_ms` plus the elapsed time of
    /// an open segment, and that segment's `Instant` is process-local monotonic —
    /// it cannot cross a worker boundary. A driver that failed to close the open
    /// segment into this field at every commit would break budgets silently on
    /// handoff.
    ///
    /// Parked and queued time is free: this accumulates only while a phase is
    /// executing.
    ///
    /// **[`LoopState::for_commit`] is the only thing that writes this.** See
    /// that function for why the fold lives there rather than in
    /// `LoopStateStore::commit` or in each driver, and for what breaks if a
    /// later commit site skips it.
    ///
    /// # The obligation this places on whoever LOADS a state
    ///
    /// `for_commit` publishes the **context's** total and *replaces* this value
    /// rather than adding to it, so a driver that loaded a state and did not
    /// seed `ctx.work_budget_consumed_ms` from this field would publish a total
    /// starting at its own zero — refunding the whole run's budget on every
    /// handoff. That obligation is stated here, on the field a loader reads,
    /// and not only on the function a committer calls.
    ///
    /// It is also **enforced**: this value only ever grows, so `for_commit`
    /// refuses a projection that would publish less than the state already
    /// carries ([`LoopStateRefusal::WorkBudgetWentBackwards`]). An unseeded
    /// driver therefore fails its first commit loudly instead of overrunning
    /// its budget silently.
    ///
    /// # And required on the wire, for the third time on the same field
    ///
    /// The enforcement above catches a driver that did not seed. It cannot catch
    /// a **record that lost this key**, because the state would then arrive
    /// carrying zero and the projection would see nothing going backwards. Zero
    /// is a legitimate value — a run that has done no work yet — so no reader
    /// can tell it from a dropped key. That is the same argument as
    /// [`RunIdentity::max_spawned_tasks`], and the consequence is the same
    /// shape: one missing key refunds the run's entire work budget.
    pub work_budget_consumed_ms: u64,

    /// Consecutive failed attempts at the current phase.
    ///
    /// After N, the execution is quarantined and surfaced rather than spun on.
    /// Reset on any successful phase, which is what makes it *consecutive* rather
    /// than a lifetime count.
    ///
    /// **Required on the wire.** Zero is the reset value, so a record that lost
    /// this key would have its quarantine counter cleared on every load — and a
    /// run failing the same phase forever would never be quarantined, which is
    /// the one thing this counter exists to do.
    pub phase_attempts: u32,

    /// The complete batch this iteration committed before firing any of it.
    ///
    /// A batch, not an effect: `Decision::Execute` fires a primary candidate,
    /// then a parallel slice of admitted follow-ups, then the rest in sequence.
    /// Present means a worker picking this execution up must consult the effect
    /// ledger per member before doing anything. It is also the atomic activation
    /// marker for new `prepared_only` intent rows: member rows are written one at
    /// a time, then this whole value is fenced into state before dispatch. A
    /// prepared row not named here is an inert prefix of an aborted write, not
    /// an effect that may have fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<super::effects::PendingBatch>,

    /// Durable iteration-entry values used by Epilogue after a handoff.
    ///
    /// Prepare publishes this in the same fenced commit that moves the cursor
    /// to Observe. It is retained throughout that iteration and cleared only
    /// when Epilogue commits. The iteration binding prevents an old baseline
    /// from corrupting a later turn's stuck counter or duration telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_checkpoint: Option<IterationCheckpoint>,

    /// A cold holder is reconstructing the inputs for this iteration before it
    /// may consume the committed Apply batch.
    ///
    /// This bit is durable because the reconstruction spans ordinary phase
    /// commits. A crash after the fresh Observe commits but before Decide must
    /// not turn the next cold holder loose at Decide with no observed state;
    /// that holder journals back to Observe again. While this is set, any
    /// [`Self::pending`] value remains the atomic activation marker for the
    /// original Apply batch and is carried unchanged through
    /// Observe -> Decide -> Resolve -> Apply. It is cleared only by a
    /// successful Apply boundary.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cold_reobserve: bool,

    /// The pre-Resolve recovery capsule for the current iteration. It is
    /// published with the Decide boundary only when all phase inputs are durable
    /// and authority-bound, retained while Resolve advances to Apply, and
    /// cleared only when Apply settles (or the turn ends). Absence at Resolve or
    /// Apply means the run relies on its live, pinned host and is intentionally
    /// not eligible for cold pickup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_checkpoint: Option<ResolveCheckpoint>,

    /// Content-free receipt for operator steers consumed by the last committed
    /// Decide boundary. The next phase must acknowledge the matching sealed
    /// inbox rows before it performs work, then clear this field with its own
    /// successful commit. Keeping the receipt in the same fenced state update
    /// as Decide closes both crash windows: before that update the messages are
    /// re-offered; after it they are retired without being prompted again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_consume_receipt: Option<super::steer_inbox::SteerConsumeReceipt>,
}

impl LoopState {
    /// A fresh execution, at iteration one, phase one, with nothing recorded.
    pub fn new(identity: RunIdentity) -> Self {
        Self {
            identity,
            segment_binding: None,
            continuation_checkpoint: None,
            terminal_settlement_receipt: None,
            cursor: LoopCursor::default(),
            journal_seq: 0,
            placement: Placement::default(),
            wait: None,
            runnable_at_ms: 0,
            deadline_at_ms: None,
            work_budget_consumed_ms: 0,
            phase_attempts: 0,
            pending: None,
            iteration_checkpoint: None,
            cold_reobserve: false,
            resolve_checkpoint: None,
            steer_consume_receipt: None,
        }
    }

    /// Whether a worker may pick this execution up at `now_ms`.
    ///
    /// Three independent reasons not to, and they are checked in the order that
    /// costs least. A parked execution is **not** runnable regardless of its
    /// timers: something outside has to resolve the wait first.
    pub fn is_runnable_at(&self, now_ms: i64) -> bool {
        self.wait.is_none() && self.runnable_at_ms <= now_ms && !self.deadline_passed(now_ms)
    }

    /// Whether the stored wall-clock deadline has passed.
    ///
    /// An absent deadline is not a passed one. Written as an explicit `map_or`
    /// rather than a comparison against a sentinel so the two cannot be
    /// conflated by a later edit.
    pub fn deadline_passed(&self, now_ms: i64) -> bool {
        self.deadline_at_ms.map_or(false, |at| at <= now_ms)
    }

    /// Whether this worker may claim this execution.
    ///
    /// # Placement narrows mid-flight, and only ever narrows
    ///
    /// An earlier version of this comment said *"placement is chosen at
    /// execution start and never mid-flight"*. That rule was written before the
    /// ephemeral-secret finding and does not survive it: `SecretStore.ephemeral`
    /// (`secrets/store.rs`) is a plain in-memory `HashMap` with no persistence
    /// path — the store persists only the provisioned, captured and mcp_oauth
    /// partitions — so a run that took a user-typed password or OTP holds it in
    /// **one process**. `resolve_invocation_secrets`
    /// (`primitive_dispatch/browser/dispatch.rs`) substitutes placeholders at the
    /// transport and bails if any remain, so moving that run to another worker
    /// fails closed and re-prompts for the same OTP every time it is
    /// rescheduled.
    ///
    /// So the amended rule: **a run may become [`Placement::Pinned`] mid-flight,
    /// and nothing may decide to widen it again — it only ever lapses.** Said
    /// exactly, because the two halves are different: no caller has a method
    /// that hands a pinned run back to everybody, and the only thing that ever
    /// does is the clock passing an expiry the pin itself carries.
    ///
    /// An earlier cut of this sentence said a pinned run "may never become
    /// [`Placement::Portable`] again", which stopped being true the moment the
    /// expiry landed — a lapsed pin admits every worker, which is the same
    /// observable outcome under a different variant name. It is kept here as a
    /// correction rather than deleted, because the distinction it got wrong is
    /// the one the rest of this comment turns on.
    ///
    /// The property the original rule protected was that scheduling never
    /// becomes *less* predictable — a run offered to a worker that cannot run
    /// it. Narrowing cannot do that: `Pinned` is a strict subset of the workers
    /// `Portable` admits, so every claim that succeeds after a pin would have
    /// succeeded before it.
    ///
    /// # Why that argument needed a BOUND, and what the bound is
    ///
    /// On its own it is an argument about the claims that **succeed**. It says
    /// nothing about the ones that stop happening, and the subset a pin narrows
    /// to can become **empty**: a worker is a process, and processes end. An
    /// unbounded pin to a worker that never comes back would be filtered out of
    /// `list_runnable` by this very predicate, forever — nothing sweeps it,
    /// nothing surfaces it, and [`Self::deadline_at_ms`] does not save it,
    /// because that deadline is checked at phase entry and no phase is ever
    /// entered. The run would simply stop, silently and permanently.
    ///
    /// So a pin **expires**. [`Placement::Pinned`] carries `pinned_until_ms`,
    /// and past it this predicate admits every worker again — see
    /// [`PIN_TTL_MS`] for the value and for what each direction of getting it
    /// wrong costs. A stranded run therefore self-heals within one TTL instead
    /// of never.
    ///
    /// The expiry is **renewed on every commit** ([`LoopState::for_commit`]
    /// re-pins through the same constructor), so a live worker's pin does not
    /// lapse under it and the bound only has to exceed the gap between one
    /// worker's commits — not the length of the run.
    ///
    /// # The pin outlives the secret on purpose, since 2026-08-28
    ///
    /// The scope guard clears the whole ephemeral scope when the loop invocation
    /// ends (`clear_ephemeral`), and the pause store blanks secret-named values
    /// out of `resolved_inputs`, so the re-registration at the next loop entry
    /// finds nothing. This doc used to say that from that moment nothing renewed
    /// the pin, so a placement pointing at a worker holding nothing decayed
    /// within one TTL. **It decays within one TTL of the last COMMIT now, not of
    /// the moment the secret cleared**, because gating renewal on the secret is
    /// what let an ordinary run — which never holds one — go claimable while it
    /// was still running. The pin is now the liveness marker rather than a
    /// statement about a credential; see [`LoopState::for_commit`] and
    /// `docs/archive/plans/2026-08-28-stateless-loop-open-questions-design.md` §1.
    ///
    /// # Why a bound and not an inverse
    ///
    /// An `unpin` would be a *decision* to widen, taken by whichever caller
    /// reached it, on an event it would have to invent — and every event
    /// available is wrong. Clearing the pin when the secret is cleared hooks
    /// `EphemeralSecretScopeGuard::drop`, which runs **in the dying worker's own
    /// process** and therefore does not run at all in the crash case, which is
    /// precisely the case that strands a run. An expiry needs no event: it is
    /// already true at the moment it matters, and any reader with a clock can
    /// see it.
    ///
    /// Widening by any other route is still the direction that breaks the rule,
    /// and there is deliberately no method here that takes it:
    /// [`LoopState::pin_to`] is the only writer of this field outside
    /// construction, and it has no inverse.
    ///
    /// **That is a convention, not a compiler guarantee, and the difference is
    /// worth being exact about** — [`super::grants::RunGrants`] earns the
    /// stronger claim a few paragraphs up by not deriving `Serialize`, and this
    /// does not have an equivalent. [`LoopState::placement`] is a `pub` field, so
    /// `state.placement = Placement::Portable` compiles anywhere and would widen
    /// a pinned run in one line. Test fixtures assign it directly today, which is
    /// why it is still `pub`. Any *production* writer added outside `pin_to` and
    /// [`LoopState::for_commit`] is a review finding, not a compile error.
    ///
    /// # Why not a narrower "pinned until consumed" state
    ///
    /// Considered and rejected, because the runtime has no consumption signal to
    /// key it on. `resolve_invocation_secrets` substitutes a placeholder without
    /// removing the entry, and the only removal is `clear_ephemeral`
    /// (`secrets/store.rs`), which the scope guard fires for the **whole scope**
    /// when the loop invocation ends. A `PinnedUntilConsumed` variant would
    /// therefore have to invent the event it releases on.
    ///
    /// The expiry above is **not** that, and the distinction is the whole reason
    /// one was acceptable and the other was not. An invented release event
    /// claims something about the secret — *it has been used, so the pin may
    /// go* — that nothing in the runtime actually knows. An expiry claims
    /// nothing about the secret at all. It says only that this placement's
    /// evidence is stale, which is true by construction once no commit has
    /// renewed it, and is checkable by any reader holding a clock rather than
    /// requiring a signal somebody has to remember to send.
    ///
    /// When the ephemeral partition gets a durable home the pin stops being
    /// needed at all, which is the real fix and a larger one.
    pub fn claimable_by(&self, worker: &WorkerId, now_ms: i64) -> bool {
        match &self.placement {
            Placement::Portable => true,
            Placement::Pinned {
                worker: owner,
                pinned_until_ms,
            } => owner == worker || pin_has_lapsed(*pinned_until_ms, now_ms),
        }
    }

    /// Bind this execution to one worker, for the life of a process-local
    /// resource it has acquired.
    ///
    /// There is no `unpin`, and adding one would break the narrowing rule
    /// [`Self::claimable_by`] states. What there is instead is an **expiry**:
    /// every pin this takes runs to `now_ms + `[`PIN_TTL_MS`], and past that the
    /// run is claimable by anyone again.
    ///
    /// # Three outcomes, and the middle one is not what an earlier doc said
    ///
    /// - **Unpinned, or pinned to this same worker** → pinned, or **renewed**.
    ///   Re-pinning to the same worker is not a no-op: it pushes the expiry out,
    ///   which is what keeps a live worker's pin from lapsing under it and is why
    ///   [`PIN_TTL_MS`] only has to cover the gap between commits.
    /// - **Pinned to a different worker, expiry still in the future** →
    ///   **refused**. A run pinned to worker A precisely because A holds its
    ///   secrets cannot be made correct by re-labelling it worker B's; B does not
    ///   have them.
    /// - **Pinned to a different worker, expiry passed** → **granted**. This is
    ///   the whole point of the bound, and it is the sentence an earlier version
    ///   of this doc contradicted by calling the refusal unconditional. An
    ///   expired pin does not bind, so refusing here would leave the run exactly
    ///   as stranded as it was before the expiry existed.
    ///
    /// A pin taken over this way still fails closed where it matters: the new
    /// worker does not have the secret, so `resolve_invocation_secrets` refuses
    /// the dispatch and the model is told to ask the user again. That is the
    /// already-accepted re-prompt, and it happens once rather than every time
    /// the run is rescheduled.
    pub fn pin_to(&mut self, worker: &WorkerId, now_ms: i64) -> Result<(), LoopStateRefusal> {
        self.placement = pinned_to(&self.placement, worker, now_ms)?;
        Ok(())
    }

    /// The one projection that turns live loop state into a state fit to commit.
    ///
    /// # What it does
    ///
    /// 1. Checks the placement invariant — Decision 2: [`Placement::Portable`] ⇒
    ///    the identity's browser ceiling permits `Cdp` and nothing else. FIRST,
    ///    and on the INCOMING placement; step 2 is why the order matters.
    /// 2. Pins the run to the committing worker. On **every** commit since
    ///    2026-08-28 — this used to read "when it holds a user-typed ephemeral
    ///    secret", and that gate is what let a live run go claimable under
    ///    itself. The pin is the liveness marker. Decision 4.
    /// 3. Folds the open work-budget segment into
    ///    [`Self::work_budget_consumed_ms`] — Decision 3. **This is the only
    ///    writer of that field** — refusing a fold that would publish less than
    ///    the state already carries, which is the one signature an unseeded
    ///    driver leaves.
    ///
    /// The budget it publishes is the **context's** total —
    /// `ctx.work_budget_consumed_ms` plus the open segment's elapsed — and it
    /// *replaces* what the state was carrying rather than adding to it. The
    /// context is the source of truth while a phase runs; the state's value is
    /// the previous commit's answer. That places one obligation on a driver: on
    /// load it must seed `ctx.work_budget_consumed_ms` from the loaded state, or
    /// the first commit on the new worker will publish a total that starts at
    /// that worker's zero. This is the same seeding the resume path already does
    /// when it restores a frame.
    ///
    /// **That obligation is checked, not merely written down.** Active time only
    /// ever grows, so a projection that would publish *less* than the state
    /// already carries is refused with
    /// [`LoopStateRefusal::WorkBudgetWentBackwards`], which names the seeding
    /// step. An earlier cut left this to the driver's own discipline, and the
    /// failure it invites is the worst shape there is: a run that quietly
    /// refunds its whole budget on every handoff, reports nothing, and overruns.
    ///
    /// Nothing is written until every check has passed, so a refused projection
    /// leaves the state exactly as it found it and the caller may report the
    /// refusal without also having half-charged the budget.
    ///
    /// # Why the fold lives here and in exactly one place
    ///
    /// **Not in `LoopStateStore::commit`.** That method takes `&LoopState` and
    /// knows nothing of `AgenticContext`. Putting the fold there would drag the
    /// loop's live types into the store trait, into both implementations, and
    /// into the 22-case contract suite — which would then be testing the loop
    /// rather than the store.
    ///
    /// **Not open-coded per driver.** The in-process and worker drivers must
    /// charge *identically*, or the phase-differential flip gate compares two
    /// runs with different budgets and reports the difference as a driver bug.
    /// One shared projection is what makes that parity claim mean anything.
    ///
    /// # Why a read-side fold rather than a mutating close
    ///
    /// A mutating close (`consumed_ms += elapsed; started_at = Some(now)`) banks
    /// time for work that is then discarded: a phase that runs 40s and loses its
    /// lease commits nothing, and under a mutating close would still have charged
    /// 40s. Repeated, that burns a run's whole budget without advancing an
    /// iteration. This fold never touches the live context, so an attempt that
    /// fails to commit costs zero — which is what the turn-boundary contract's §3
    /// requires ("work done by a holder that never reported is discarded, not
    /// replayed"). It is the same shape the pause path already uses
    /// (`executor.rs`, where `pause_state.work_budget_consumed_ms =
    /// context_work_budget_elapsed_ms(ctx)` also folds without mutating).
    ///
    /// It is also idempotent, which a mutating close is not: a commit rejected by
    /// the store's compare-and-swap and retried recomputes the same base plus a
    /// slightly larger elapsed. Nothing double-counts.
    ///
    /// # Crash between the close and the commit
    ///
    /// There is no window. Nothing was mutated, so a crash after projecting and
    /// before committing loses only the uncommitted attempt's elapsed time —
    /// which is the correct accounting for work that is being discarded. The
    /// residual case is a long phase that dies late (a `Decide` on a slow
    /// provider); that time is genuinely lost from the budget, on purpose. A
    /// provider without a durable job/result API may bill that interrupted call
    /// again on retry; the loop does not claim a timestamp marker can adopt a
    /// response it cannot retrieve.
    ///
    /// # What breaks if a later commit site skips this
    ///
    /// Silently, and only on handoff. `context_work_budget_elapsed_ms` is
    /// `consumed_ms` plus an **`Instant`** elapsed, and an `Instant` is
    /// process-local monotonic: it cannot cross a worker boundary and there is no
    /// value another process could substitute. A commit that skipped the fold
    /// would publish the last-folded total, so every phase a run executed since
    /// its previous commit would be free. Budgets come out low, nothing errors,
    /// and the run overruns its work budget by however much time it spent on the
    /// worker that forgot. The realistic regression is not deleting this
    /// function — it is a second commit site added later that does not call it,
    /// which is what `a_commit_charges_the_open_segment` and the
    /// single-assignment scan below are there to catch.
    pub fn for_commit(&mut self, at: CommitPoint<'_>) -> Result<(), LoopStateRefusal> {
        // Everything that can refuse is computed before anything is written. A
        // projection that charged the budget and then refused would leave a
        // caller reporting the refusal from a state it had already modified.
        //
        // ── RENEWED ON EVERY COMMIT, since 2026-08-28 ───────────────────────
        //
        // It used to renew only on a commit that still held a user-typed
        // ephemeral secret, and carry the placement forward untouched otherwise,
        // "so the run frees itself within one [`PIN_TTL_MS`] instead of staying
        // bound to a worker that has nothing left to offer it."
        //
        // **That freed a LIVE run to be taken from under itself.** An ordinary
        // run holds no such secret, so nothing renewed the pin it was given at
        // creation; one [`PIN_TTL_MS`] later `claimable_by` answered `true` while
        // the run was still going. The lease does not cover the exposure,
        // because `advance_once` claims per phase and releases between them —
        // deliberately, since that is what makes a worker short-lived rather than
        // resident — so between two phases a live run holds no lease and no pin.
        // A scan then offers the key, a second worker claims and advances it, and
        // the original arm's next claim answers `LeaseHeld`, which
        // `StatelessArm` turns into an error that FAILS a run that was healthy.
        //
        // # Why this is the pin's job and not a new mechanism
        //
        // [`PIN_TTL_MS`]'s own doc says it "only has to cover the gap between
        // commits" — which is a description of renew-on-commit, and is only true
        // if every commit renews. The secret gate is what made the TTL cover the
        // whole remainder of a run instead of a gap, and the sizing has been
        // wrong-footed by it rather than by the TTL being short.
        //
        // A separate liveness marker was considered and is not needed: the pin
        // already carries the two things liveness wants, a worker and an expiry,
        // and [`pinned_to`] already renews for the holder, grants after a lapse,
        // and refuses a different worker whose predecessor is still live.
        //
        // Two more were refused, named here so the decision site is the one
        // place that does not need a cross-reference hunt. **Cooperative
        // handoff** — a migration the current driver agrees to rather than one a
        // scanner takes — is the identical change to this line today plus a
        // named future mechanism, refused as YAGNI while nothing needs to move a
        // live run. **Two TTLs**, a short renewal without a secret and a long one
        // with, satisfies both properties and needs a constant nobody has a
        // reason for; picking that number silently is how a constant becomes
        // folklore. All three, with the cost below, are recorded in
        // `docs/archive/plans/2026-08-28-stateless-loop-open-questions-design.md` §1.
        //
        // # What it costs, stated because it is a real trade
        //
        // A crashed run is now recoverable one [`PIN_TTL_MS`] after its LAST
        // COMMIT rather than one after its creation. That is the ordinary price
        // of lease-based liveness, and it is the right direction: the old
        // behaviour made an ordinary run recoverable — meaning stealable — while
        // it was still running.
        //
        // # `holds_user_typed_ephemeral_secret` is no longer read here
        //
        // It is deliberately still carried and still produced. The distinction it
        // marks is real and STRONGER than the one this line used to make: a run
        // pinned because its worker holds a secret should arguably not be granted
        // to another worker even after a lapse, since [`pinned_to`]'s own
        // reasoning — "B does not have them" — does not stop being true when the
        // expiry passes. Acting on that means deciding what happens to a run
        // whose secret died with its worker, which is a separate question and an
        // unbuilt answer; the flag is the hook for it.
        //
        // # The portable-ceiling guard runs FIRST, and that also changed here
        //
        // BEFORE the pin, and on the INCOMING placement. Order matters here, and
        // it changed with the renewal above.
        //
        // This guards a PAIRING — a run that calls itself portable while its
        // browser ceiling permits a process-local transport — and the point is to
        // surface that contradiction rather than resolve it. Checking the pinned
        // OUTGOING placement would skip the guard entirely now that every commit
        // pins, so a run declaring the contradiction would be quietly pinned into
        // consistency instead of refused. The declaration would still be wrong,
        // and nothing would say so.
        portable_ceiling_is_cdp_only(&self.placement, &self.identity.browser_transports)?;
        let placement = pinned_to(&self.placement, at.worker, at.now_ms)?;

        let charged = super::super::executor::context_work_budget_elapsed_ms(at.ctx);
        // Active time does not un-elapse. A projection that would publish less
        // than the state already carries is a context that was never seeded from
        // the state it was loaded from — see the load-side obligation above.
        if charged < self.work_budget_consumed_ms {
            return Err(LoopStateRefusal::WorkBudgetWentBackwards {
                committed_ms: self.work_budget_consumed_ms,
                projected_ms: charged,
            });
        }
        self.placement = placement;
        self.work_budget_consumed_ms = charged;
        Ok(())
    }
}

/// What a commit boundary knows that [`LoopState`] does not.
///
/// A named struct rather than positional arguments, because two of the four
/// would be unreadable at a call site: a bare `bool` whose sense nothing shows —
/// `for_commit(ctx, worker, now, true)` reads as nothing — and a bare `i64` clock
/// sitting next to the `i64` budget values this projection also deals in.
///
/// # The bool's argument for being named got WEAKER and more urgent at once
///
/// This doc used to justify the field name by saying a caller that inverted the
/// bool "would compile and pin every run that holds no secret while releasing
/// every run that does". That stopped being true on 2026-08-28:
/// [`LoopState::for_commit`] does not read the flag at all, so inverting it now
/// changes nothing about placement and no test anywhere would go red.
///
/// Which is the worse failure mode, not the milder one. A wrong value is
/// **inert today and latent**, because the flag is carried as the hook for a
/// decision that is not built — what happens to a run whose secret died with its
/// worker. Whoever builds that reads the field expecting it to mean what it
/// says. A name at the call site is what stops a bug being written now and
/// discovered by the feature that first depends on it.
pub struct CommitPoint<'a> {
    /// The live context whose open work-budget segment is being charged.
    pub ctx: &'a super::super::types::AgenticContext,
    /// The worker running this phase, and the pin target on **every** commit.
    ///
    /// It read "the pin target when the run holds a user-typed ephemeral
    /// secret" until 2026-08-28, which described the gate
    /// [`LoopState::for_commit`] no longer applies: the pin is the liveness
    /// marker now, so this worker is re-pinned whether or not a secret is held.
    pub worker: &'a WorkerId,
    /// Wall clock at this boundary, in milliseconds.
    ///
    /// A parameter rather than an `Utc::now()` inside the projection, because
    /// [`LoopState`] reads no ambient clock anywhere — [`LoopState::is_runnable_at`],
    /// [`LoopState::deadline_passed`] and [`LoopState::claimable_by`] all take
    /// the time they judge against. A type whose correctness a test cannot pin
    /// to an instant is a type whose expiry logic can only be tested by
    /// sleeping.
    pub now_ms: i64,
    /// Whether this run's ephemeral secret scope currently holds a user-typed
    /// secret, per
    /// [`SecretStore::ephemeral_scope_holds_user_typed_secret`](crate::magician_v2::secrets::SecretStore::ephemeral_scope_holds_user_typed_secret).
    ///
    /// Passed in rather than looked up here because `LoopState` is a value with
    /// no handle on the runtime, and giving it one to answer a boolean would put
    /// the secret store inside the type the store layer serializes.
    pub holds_user_typed_ephemeral_secret: bool,
}

/// Why a projection or a placement change was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopStateRefusal {
    /// A portable execution was permitted a transport other than `Cdp`.
    PortableRunMayReachAProcessLocalBrowser {
        /// The ceiling as an operator would read it.
        ceiling: String,
    },
    /// The declared ceiling did not parse, so no claim can be made about it.
    UnreadableCeiling { detail: String },
    /// A pinned run was asked to move to another worker while its pin was
    /// still live.
    ///
    /// Carries `until_ms` because the refusal is now temporary and an operator's
    /// first question is *how long*. A refusal that cannot answer that reads as
    /// permanent, which is what this whole bound exists to stop being true.
    PinnedElsewhere {
        owner: WorkerId,
        asked: WorkerId,
        until_ms: i64,
    },
    /// An owner handover would have left a portable run with no browser
    /// transport at all.
    HandoverLeavesPortableRunNoTransport {
        to_agent_id: String,
        incoming: Vec<String>,
    },
    /// A projection would have published less active time than the state
    /// already carries.
    ///
    /// Only one thing produces this: a driver that loaded a state and did not
    /// seed `ctx.work_budget_consumed_ms` from it, so the context is counting
    /// from this worker's zero. Named as a refusal rather than repaired with a
    /// `max`, because repairing it would hide the missing seed and leave the
    /// *next* handoff to lose the time again.
    WorkBudgetWentBackwards {
        committed_ms: u64,
        projected_ms: u64,
    },
}

impl std::fmt::Display for LoopStateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopStateRefusal::PortableRunMayReachAProcessLocalBrowser { ceiling } => write!(
                f,
                "a portable execution may be claimed by any worker, so its browser ceiling must \
                 permit cdp and nothing else; this one permits {ceiling}"
            ),
            LoopStateRefusal::UnreadableCeiling { detail } => {
                write!(f, "this run's browser ceiling could not be read: {detail}")
            },
            LoopStateRefusal::PinnedElsewhere {
                owner,
                asked,
                until_ms,
            } => write!(
                f,
                "this execution is pinned to worker {owner} until {until_ms} and cannot be \
                 re-pinned to {asked} before then; the pin exists because {owner} holds a \
                 resource {asked} does not, and it lapses on its own rather than needing anyone \
                 to release it"
            ),
            LoopStateRefusal::HandoverLeavesPortableRunNoTransport {
                to_agent_id,
                incoming,
            } => write!(
                f,
                "agent {to_agent_id} declares the browser ceiling {incoming:?}, which shares no \
                 transport with the cdp-only ceiling a portable execution is held to; the \
                 intersection permits nothing and an empty ceiling would read as unrestricted"
            ),
            LoopStateRefusal::WorkBudgetWentBackwards {
                committed_ms,
                projected_ms,
            } => write!(
                f,
                "this projection would publish {projected_ms}ms of active time where \
                 {committed_ms}ms is already committed, and active time does not un-elapse; seed \
                 `ctx.work_budget_consumed_ms` from the loaded state before running a phase"
            ),
        }
    }
}

impl std::error::Error for LoopStateRefusal {}

/// The placement a run holding a process-local resource must have.
///
/// A free function so [`LoopState::pin_to`] and [`LoopState::for_commit`] cannot
/// disagree about what pinning means — the projection has to answer "would this
/// pin be refused" before it writes anything, and a method taking `&mut self`
/// cannot be asked that question without already having applied it.
fn pinned_to(
    current: &Placement,
    worker: &WorkerId,
    now_ms: i64,
) -> Result<Placement, LoopStateRefusal> {
    // `saturating_add`, not `+`: `now_ms` is wall clock off a machine this type
    // does not control, and a clock far enough in the future to overflow an i64
    // should produce a pin that never lapses rather than a panic — or, worse
    // under release arithmetic, a wrapped negative expiry that reads as already
    // lapsed and silently unpins the run.
    let until_ms = now_ms.saturating_add(PIN_TTL_MS);
    match current {
        Placement::Portable => Ok(Placement::Pinned {
            worker: worker.clone(),
            pinned_until_ms: until_ms,
        }),
        // The holder re-pinning. RENEWS rather than returning `current`
        // unchanged: this is the call EVERY commit makes — not only one that
        // still holds a secret, since 2026-08-28 — and it is the only thing that
        // stops a live worker's own pin lapsing under it.
        Placement::Pinned { worker: owner, .. } if owner == worker => Ok(Placement::Pinned {
            worker: worker.clone(),
            pinned_until_ms: until_ms,
        }),
        // A different worker, and the previous holder's pin has lapsed. Granting
        // it is the point of the bound; refusing here would leave a run pinned to
        // a dead worker exactly as stranded as an unbounded pin did.
        Placement::Pinned {
            pinned_until_ms, ..
        } if pin_has_lapsed(*pinned_until_ms, now_ms) => Ok(Placement::Pinned {
            worker: worker.clone(),
            pinned_until_ms: until_ms,
        }),
        Placement::Pinned {
            worker: owner,
            pinned_until_ms,
        } => Err(LoopStateRefusal::PinnedElsewhere {
            owner: owner.clone(),
            asked: worker.clone(),
            until_ms: *pinned_until_ms,
        }),
    }
}

/// How long a pin binds before the run is offered to everyone again.
///
/// **Fifteen minutes**, and the number is anchored to the gap between one
/// worker's commits rather than to how long a run lasts. A pin is renewed by
/// **every** commit ([`pinned_to`]), so a live worker only has to reach one
/// commit boundary inside this window; a run does not have to finish inside it.
/// That "every" is load-bearing and used to read "every commit that still holds
/// the secret", which made this window cover the whole remainder of an ordinary
/// run rather than a gap — see [`LoopState::for_commit`]. For scale,
/// `WorkerConfig::lease_ttl` defaults to five
/// minutes, so this is three lease periods — a worker whose lease lapses
/// mid-phase still keeps its pin.
///
/// # Both directions of getting it wrong
///
/// **Too short** — a LIVE worker's pin lapses between two of its own commits,
/// another worker claims the run, and it fails closed at the transport:
/// `resolve_invocation_secrets` finds an unresolved placeholder and tells the
/// model to ask the user again. One such re-prompt is the handoff behaviour this
/// design already accepted. The danger is that a value below the real commit gap
/// does it *every* time the run is rescheduled — which is the re-prompt loop
/// Decision 4 exists to stop, reintroduced by a constant.
///
/// **Too long** — a run whose worker died stays unschedulable for this long.
/// Nothing is lost and it still self-heals, but the user watches a stalled run
/// with nothing to explain it.
///
/// So the two directions do not trade off against a single midpoint; they set a
/// floor and a ceiling. The **floor** is the commit gap with real margin, and it
/// is the harder of the two, because undershooting it is not one re-prompt but a
/// loop. The **ceiling** is how long a visibly dead run is tolerable. Fifteen
/// minutes is chosen as close to the floor as the margin allows rather than
/// anywhere near the ceiling: three lease periods is enough that a worker has to
/// fail to commit for three consecutive lease lifetimes to lose its own pin, and
/// short enough that a stranded run is back in fifteen minutes rather than an
/// hour.
pub const PIN_TTL_MS: i64 = 15 * 60 * 1_000;

/// Whether a pin taken until `pinned_until_ms` no longer binds at `now_ms`.
///
/// **`<=`, so a pin whose expiry is exactly now is over.** The same edge, and
/// the same choice, as `Lease::is_expired_at` and [`LoopState::deadline_passed`]
/// beside it — one spelling of "expired" in this subsystem rather than two, and
/// the direction that cannot leave a lapsed pin binding for one more
/// millisecond.
///
/// A free function rather than a method on [`Placement`] so
/// [`LoopState::claimable_by`] and [`pinned_to`] cannot answer the question
/// differently: one of them deciding a pin is live while the other decides it is
/// lapsed is a run that no worker can claim and no worker can take over.
fn pin_has_lapsed(pinned_until_ms: i64, now_ms: i64) -> bool {
    pinned_until_ms <= now_ms
}

/// Decision 2's invariant, as one predicate.
///
/// > [`Placement::Portable`] ⇒ `BrowserTransportCeiling::parse(
/// > identity.browser_transports)` permits `Cdp` and nothing else.
///
/// Spelled as `permits` calls rather than as an equality against a parsed
/// `["cdp"]` so it does not depend on how `parse` orders or deduplicates its
/// output. The unrestricted case falls out of it for free: an unrestricted
/// ceiling permits every transport, so it fails the second clause — which is the
/// case that matters most, because `BrowserTransportCeiling::parse(&[])` is
/// `allowed: None` and permits every transport there is.
///
/// The "and nothing else" half is derived from `BrowserTransport::ALL` rather
/// than written out as the two transports that exist today. An earlier cut named
/// `Headed` and `Headless` by hand, which meant a fourth transport added to that
/// enum would have been **silently permitted on a portable run** — the widening
/// this predicate exists to refuse, arriving through a file that never mentions
/// placement.
///
/// A [`Placement::Pinned`] run is unconstrained here: it holds a worker, so a
/// process-local transport is exactly what it is for.
///
/// `pub(super)` and taking the declared list rather than a whole
/// [`RunIdentity`], so [`super::journal::apply_owner_transition`] can check the
/// ceiling it is about to install against *this* predicate instead of against a
/// value it believes satisfies it. Two spellings of one invariant is how the two
/// halves drift.
pub(super) fn portable_ceiling_is_cdp_only(
    placement: &Placement,
    declared: &[String],
) -> Result<(), LoopStateRefusal> {
    use crate::magician_v2::execution::primitive_dispatch::browser::session::{
        BrowserTransport, BrowserTransportCeiling,
    };

    if !matches!(placement, Placement::Portable) {
        return Ok(());
    }
    let ceiling = BrowserTransportCeiling::parse(declared).map_err(|error| {
        LoopStateRefusal::UnreadableCeiling {
            detail: error.to_string(),
        }
    })?;
    let cdp_only = ceiling.permits(BrowserTransport::Cdp)
        && BrowserTransport::ALL
            .iter()
            .filter(|transport| **transport != BrowserTransport::Cdp)
            .all(|transport| !ceiling.permits(*transport));
    if cdp_only {
        Ok(())
    } else {
        Err(LoopStateRefusal::PortableRunMayReachAProcessLocalBrowser {
            ceiling: ceiling.label(),
        })
    }
}

/// Which iteration and which phase — the cursor a worker resumes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCursor {
    /// One-based, and the number the iteration ceiling is checked against.
    pub iteration: usize,
    pub phase: super::outcome::Phase,
}

/// Durable inputs captured at iteration entry for the final Epilogue phase.
///
/// A monotonic [`std::time::Instant`] cannot cross a process boundary. This
/// stores the equivalent wall-clock start plus the history baseline, bound to
/// the exact iteration that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IterationCheckpoint {
    pub iteration: usize,
    pub history_iterations_len_at_start: usize,
    pub started_at_ms: i64,
}

/// Versioned bounded payload for cross-process exact continuation.
///
/// Stored as canonical JSON rather than a second handwritten copy of
/// `AgenticPauseState`: pause-state additions then become recoverable by
/// default, while the explicit byte ceiling and the writer's bounded live-message
/// projection prevent state size from growing with the complete execution history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IterationContinuationCheckpoint {
    pub version: u16,
    pub payload: serde_json::Value,
}

impl IterationContinuationCheckpoint {
    pub const VERSION: u16 = 1;
    // ResolveCheckpoint may retain up to 4 MiB in this same LoopState and the
    // filesystem envelope caps the complete state at 8 MiB. Leave a full 2 MiB
    // for the ledger markers, pending batch and serde envelope instead of
    // allowing two individually-valid 4 MiB payloads to make an invalid state.
    pub const MAX_BYTES: usize = 2 * 1024 * 1024;

    pub fn try_new(pause: &super::super::types::AgenticPauseState) -> Result<Self, String> {
        let payload = serde_json::to_value(pause)
            .map_err(|error| format!("iteration continuation could not be encoded: {error}"))?;
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| format!("iteration continuation could not be measured: {error}"))?;
        if bytes.len() > Self::MAX_BYTES {
            return Err(format!(
                "iteration continuation is {} bytes, above the {} byte boundary",
                bytes.len(),
                Self::MAX_BYTES
            ));
        }
        Ok(Self {
            version: Self::VERSION,
            payload,
        })
    }

    pub fn decode(&self) -> Result<super::super::types::AgenticPauseState, String> {
        if self.version != Self::VERSION {
            return Err(format!(
                "iteration continuation version {} is not supported by version {}",
                self.version,
                Self::VERSION
            ));
        }
        let bytes = serde_json::to_vec(&self.payload)
            .map_err(|error| format!("iteration continuation could not be measured: {error}"))?;
        if bytes.len() > Self::MAX_BYTES {
            return Err(format!(
                "iteration continuation is {} bytes, above the {} byte boundary",
                bytes.len(),
                Self::MAX_BYTES
            ));
        }
        serde_json::from_value(self.payload.clone())
            .map_err(|error| format!("iteration continuation could not be decoded: {error}"))
    }
}

impl IterationCheckpoint {
    pub fn is_for(&self, cursor: LoopCursor) -> bool {
        cursor.phase == super::outcome::Phase::Epilogue && self.iteration == cursor.iteration
    }

    /// Elapsed wall time, with a future/corrupt clock saturating to zero.
    pub fn elapsed_ms_at(&self, now_ms: i64) -> u64 {
        u64::try_from(now_ms.saturating_sub(self.started_at_ms)).unwrap_or(0)
    }
}

impl Default for LoopCursor {
    fn default() -> Self {
        Self {
            iteration: 1,
            phase: super::outcome::Phase::first(),
        }
    }
}

// ============================================================================
// What a holder that did not run the previous phase does not have
// ============================================================================

/// The values a phase reads that an **earlier phase of the same iteration**
/// produced. Epilogue is the deliberate exception to the original memory-only
/// rule: [`IterationCheckpoint`] now carries its two scalars, but their names
/// stay here so a missing/misaligned checkpoint is classified as a refused cold
/// entry rather than silently substituted.
///
/// # This is the answer to "what is missing", not a wish list
///
/// The design asks what the gap is between [`LoopState`] and what a
/// `WorkerHost` needs to serve one phase. For five of the six phases the answer
/// is not a field of the run's state at all — it is a value the *previous phase
/// of this same iteration* handed forward in memory, through the
/// `IterationCarry` on `executor.rs`'s `InProcessWorkerHost`. Committing more
/// identity, more budget or more cursor does not close it.
///
/// The names below are that hand-off, spelled once. `executor.rs`'s `run_phase`
/// reads them and fails with `carry_missing` when one is absent; this function
/// is what a *holder* consults **before** deciding it may enter a phase at all.
/// Duplicated lists drift, so `run_loop/mod.rs`'s
/// `the_names_a_phase_carries_in_memory_have_one_spelling` asserts they agree —
/// and asserts it in the direction that matters, which is this list going
/// **short**: a name dropped here makes a phase look enterable that will fail
/// on entry, after a claim, a fence increment and an attempt charged against
/// [`LoopState::phase_attempts`].
///
/// There are **three** lists, not two, and the third is easy to miss: the gate
/// filters by its own `CARRY_MEMBERS` const to decide how many refusal sites to
/// expect, so a fifth carry value has to be added there before the gate can see
/// it at all. What that gate does and does not carry is written down on the test
/// itself rather than promised here.
///
/// # `Epilogue` is in this table and holds nothing in `IterationCarry`
///
/// Deliberately, and it is the row a narrower "carry inputs" function would
/// have got wrong. `phases::epilogue::run` takes
/// `history_iterations_len_at_iter_start` and `iteration_started_at`, which the
/// host measures at **iteration entry** — `executor.rs` assigns both at the top
/// of the `for` body — and holds as plain fields rather than in the carry. A
/// successful Prepare boundary copies them into [`IterationCheckpoint`], which
/// is the only basis a foreign holder may use. A missing or iteration-mismatched
/// checkpoint remains a refusal.
///
/// So the question this table answers is *what did an earlier part of THIS
/// iteration produce in memory*, not *what is in the carry struct*.
///
/// # Why `Prepare` is empty, read rather than assumed
///
/// `phases::prepare::run` takes the context, the executors, the history, the
/// protective state, the environment state, the checkpoint hints, the
/// cancellation token and the iteration number — eight bindings, none of them
/// produced by an earlier phase of the same iteration. It is the only phase for
/// which that is true, which is what makes the iteration boundary the one
/// admissible pickup point.
pub fn in_memory_inputs_of(phase: super::outcome::Phase) -> &'static [&'static str] {
    match phase {
        super::outcome::Phase::Prepare => &[],
        super::outcome::Phase::Observe => &["browser_primitive_enabled"],
        super::outcome::Phase::Decide => &["browser_primitive_enabled", "observed_state"],
        super::outcome::Phase::Resolve => &["observed_state", "decided"],
        super::outcome::Phase::Apply => &["resolved"],
        super::outcome::Phase::Epilogue => &[
            "history_iterations_len_at_iter_start",
            "iteration_started_at",
        ],
    }
}

/// Whether a holder that did **not** run this run's previous phase may enter at
/// the committed cursor.
///
/// # The decision this type records
///
/// Most foreign pickups still begin at an iteration boundary. Resolve and Apply
/// are deliberate recovery entries: the driver either restores Decide's
/// versioned capsule or journals far enough back to observe the live world
/// again. Observe and Decide are recoverable only while a durable cold
/// reconstruction is already in progress; a new holder at Decide rewinds to
/// Observe rather than using the prior holder's missing observation. Epilogue
/// is recoverable only when its iteration-bound [`IterationCheckpoint`] is
/// present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignPickup {
    /// The cursor sits at the start of an iteration. Every value the phase
    /// reads is either committed or rebuilt by the phase itself.
    AtIterationBoundary,
    /// The cursor is mid-iteration, but the driver has a durable recovery path:
    /// restore the pre-Resolve capsule or journal back to a fresh observation.
    RecoverableMidIteration { phase: super::outcome::Phase },
    /// The cursor sits inside an iteration. The named values live only in the
    /// memory of the holder that ran the earlier phase.
    MidIteration {
        phase: super::outcome::Phase,
        needs: &'static [&'static str],
    },
}

impl LoopState {
    /// Whether a holder that did not run the previous phase may enter here.
    ///
    /// # It does not consider a park, and the caller must
    ///
    /// A parked run's pickup is a **park exit**, not a phase entry: the driver
    /// clears the wait and returns without running a phase, so the cursor's
    /// in-memory inputs are never read. A caller using this to gate a claim
    /// must therefore ask it only when [`LoopState::wait`] is `None` — gating a
    /// parked key on this answer would refuse the one transition the wake index
    /// exists to produce.
    ///
    /// Left to the caller rather than folded in, because folding it in would
    /// make the answer for a parked run `AtIterationBoundary` — a claim that a
    /// cold entry is *safe*, rather than that the question does not arise, and
    /// the two are the same answer only until somebody uses this for something
    /// other than a claim gate.
    ///
    /// ## And the caller must ask again AFTER the exit
    ///
    /// The paragraph above is half of the obligation, and a caller that stopped
    /// there has gated nothing. `driver_worker::leave_park` clears the wait,
    /// commits, and **does not move the cursor** — the run comes back at the
    /// phase it parked at, which for a parent that parked out of `Apply` is a
    /// phase this function refuses. The pickup a park exit admits is therefore
    /// admitted for exactly one transition, and the next entry is a cold entry
    /// like any other.
    ///
    /// `worker_runner`'s `after_park` is where that second ask lives. Recorded
    /// here because the obligation belongs to this function's contract, and the
    /// first caller to honour only the first half shipped a guard that provided
    /// no protection on the one path it carved out.
    pub fn foreign_pickup(&self) -> ForeignPickup {
        if matches!(
            self.cursor.phase,
            super::outcome::Phase::Resolve | super::outcome::Phase::Apply
        ) || (self.cold_reobserve
            && matches!(
                self.cursor.phase,
                super::outcome::Phase::Observe | super::outcome::Phase::Decide
            ))
            || self
                .iteration_checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.is_for(self.cursor))
        {
            return ForeignPickup::RecoverableMidIteration {
                phase: self.cursor.phase,
            };
        }
        let needs = in_memory_inputs_of(self.cursor.phase);
        if needs.is_empty() {
            ForeignPickup::AtIterationBoundary
        } else {
            ForeignPickup::MidIteration {
                phase: self.cursor.phase,
                needs,
            }
        }
    }
}

/// Which workers may run this execution.
///
/// With coding shadow repositories moved to shared storage, **a local Chrome is
/// the only resource that must survive between phases**, which is what makes this
/// boundary narrow enough to enforce. Verified rather than assumed: shell
/// subprocesses live and die inside a single dispatch, the shell stream context
/// holds label metadata rather than a process handle, and `Cdp` browser sessions
/// live in magicutor.
///
/// **That framing is qualified by [`Placement::Portable`]'s doc below**, which
/// records the design doc's observation that this boundary was drawn around the
/// wrong resource. Nothing here is redrawn; the pointer exists so a reader meets
/// the qualification rather than only the claim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "placement", rename_all = "snake_case")]
pub enum Placement {
    /// Not bound to any worker — which since 2026-08-28 means **not yet
    /// committed**, and no longer means *movable*.
    ///
    /// # What this class narrowed to, and why
    ///
    /// It was drawn to mean "any worker may take this run". That meaning did not
    /// survive the decision that a live run stays with its driver:
    /// [`LoopState::for_commit`] renews the pin on **every** commit, not only on
    /// one that still holds a user-typed ephemeral secret, so a run is
    /// [`Placement::Pinned`] from its first commit until one [`PIN_TTL_MS`] after
    /// its last. The only window this variant can still describe is the one
    /// *before* that first commit. It says a run has not been bound yet; it no
    /// longer says a run can be moved.
    ///
    /// [`LoopState::claimable_by`] still answers `true` for it, and that is the
    /// same sentence read under the new meaning rather than a survivor of the old
    /// one: a run nothing has bound yet is takeable by whoever reaches it first.
    /// What changed is which runs can *be* unbound — not what being unbound
    /// admits.
    ///
    /// # Why it is KEPT rather than deleted, and not for symmetry
    ///
    /// **No production state ever commits `Portable`.** `LoopState::new` does
    /// construct one — it is the `Default` — but `executor.rs`'s
    /// `StatelessArm::seed_if_absent` overwrites it with `Pinned` before the first
    /// commit, so the value is produced and never published. A variant no
    /// committed record can hold is the tempting thing to delete.
    ///
    /// It is the wrong thing to delete, because two guards take this variant as
    /// their precondition and neither has another one:
    ///
    /// - `portable_ceiling_is_cdp_only` returns early on every other placement, so
    ///   this variant is its **only** firing condition.
    /// - [`super::journal::apply_owner_transition`] has an independent `Portable`
    ///   branch that intersects the incoming ceiling with `[cdp]` and refuses the
    ///   handover outright when that intersection would be empty
    ///   (`HandoverLeavesPortableRunNoTransport`). Named here as well as sixty
    ///   lines below, because somebody who removes the predicate would otherwise
    ///   believe they had cleared the only obstacle.
    ///
    /// What both guard is a **pairing** that is still reachable — a run arriving
    /// portable while its browser ceiling permits a process-local transport.
    /// Deleting the variant does not delete a dead value; it deletes both guards'
    /// precondition, and their subject with it.
    ///
    /// # The non-primitive browser routes were audited too
    ///
    /// `compiled_handlers/screenshot_preview.rs` and
    /// `compiled_handlers/capture_reference.rs` launch a hardcoded `Headless`
    /// browser outside primitive dispatch. They now require the runtime-owned
    /// agent id and scope, load that scoped agent definition, and resolve
    /// `Headless` against its `browser_transports` ceiling before launch. A
    /// missing definition, unreadable store, or incompatible ceiling refuses.
    ///
    /// `apps/browser_capability.rs` and `media_seam/browser_join.rs` are not
    /// alternate agent-tool doors: app-installation browser grants and meeting
    /// media are separate authority domains with their own admission contracts.
    /// They must not be made to inherit an agent's loop placement implicitly.
    ///
    /// # The redraw this deliberately does NOT do
    ///
    /// Recorded here because it is the next reader's first question, and left
    /// unacted on. The design doc observes that this class was drawn around the
    /// **wrong resource**: a local Chrome surviving between *phases*, where the
    /// thing that actually crosses a boundary is a chat thread's browser session
    /// surviving between *executions*. Redrawing it around that is new design
    /// rather than cleanup, so the observation stands and the boundary stays where
    /// it is. See
    /// `docs/archive/plans/2026-08-28-stateless-loop-open-questions-design.md` §2.
    ///
    /// # Still cdp-only, and still what `Default` gives
    ///
    /// Cdp-only, enforced through the existing `BrowserTransportCeiling`.
    ///
    /// This is what [`Default`] gives, and it is deliberately no longer what an
    /// **absent** `placement` key gives: [`LoopState::placement`] is required on
    /// the wire, so a record that lost the key fails to parse. The `Default`
    /// impl remains because a run that has declared nothing is not yet bound — the
    /// caller that needs a local Chrome asks for [`Placement::Pinned`] explicitly.
    ///
    /// # What guards this, stated no wider than it is true
    ///
    /// An earlier version of this comment said the default "does not fail open,
    /// because the gate refuses a `Headed`/`Headless` effect on a portable
    /// execution", and a later correction said **there is no such gate**. The
    /// correction was right and is still right: nothing reads a `Placement` to
    /// refuse an effect, and there is deliberately no dispatch-time placement
    /// gate — adding one would give "may this run open a headed browser" two
    /// answers.
    ///
    /// What exists instead is an **invariant**, checked in two places, that ties
    /// this class to the ceiling that already refuses:
    ///
    /// > `Portable` ⇒ `BrowserTransportCeiling::parse(identity.browser_transports)`
    /// > permits `Cdp` and nothing else.
    ///
    /// - [`LoopState::for_commit`] refuses to project a state that violates it,
    ///   so a violating state cannot be published.
    /// - [`super::journal::apply_owner_transition`] **intersects** a portable
    ///   run's ceiling with `[cdp]` instead of replacing it, which is the case no
    ///   start-time check would catch: a run can start portable under a `[cdp]`
    ///   ceiling and hand over to an agent whose ceiling is unrestricted, leaving
    ///   the placement correct, the ceiling correct, and the combination wrong.
    ///
    /// With the invariant holding, the refusal at
    /// `primitive_dispatch/dispatch.rs` — `transport_ceiling.resolve(requested)`,
    /// and `enforce_ceiling_on_attached_session` for re-attachment — *is* the
    /// placement gate, and it already produces the operator-legible `NOT BROWSED`
    /// error naming the ceiling.
    ///
    /// **What this does not cover.** This invariant governs browser authority
    /// held by the agentic run. App-installation browser grants and meeting
    /// media remain distinct authority domains and enforce their own contracts.
    #[default]
    Portable,
    /// Holds a process-local resource. Only this worker may claim it, and only
    /// until `pinned_until_ms`.
    Pinned {
        worker: WorkerId,
        /// Wall clock past which this pin no longer binds anybody.
        ///
        /// **Required on the wire — no `default`.** Zero is a legitimate-looking
        /// value that every real clock is already past, so a dropped key would
        /// read as *lapsed* and hand a run holding a live credential to every
        /// worker — the same widening `placement` itself is required to prevent,
        /// one level down. Absence is a parse failure instead.
        ///
        /// Renewed by [`LoopState::for_commit`] on **every** commit — not only
        /// one that still holds a secret, which is what it said before
        /// 2026-08-28 — so this bounds the gap between a worker's commits rather
        /// than the life of the run. See [`PIN_TTL_MS`].
        pinned_until_ms: i64,
    },
}

/// Why an execution is parked.
///
/// `Job` and `Children` are one enum rather than two states because they park
/// and wake identically — what differs is who resolves the wake token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "wait", rename_all = "snake_case")]
pub enum WaitReason {
    /// An externalized long job: a coding run, an eight-hour build. The
    /// execution holds nothing but a row on disk while it waits.
    Job { job_id: String },
    /// Delegated child executions, each its own `LoopState` in the same store
    /// with its own lease, placement and budget.
    ///
    /// # The hazard this shape must preserve
    ///
    /// An idempotent retry can park a parent with **no live child left to wake
    /// it** — a permanent hang. Under a stateless driver retries become routine,
    /// so it gets worse rather than better. Parking on children is therefore
    /// conditional on at least one live child, with already-complete children
    /// consumed as results rather than waited on; [`WaitReason::children`] is
    /// where that condition is enforced instead of at each caller.
    Children {
        child_execution_ids: Vec<String>,
        /// Where the work this park depends on is **addressed**, as against
        /// merely named. See [`ResumeAddress`], which carries the whole
        /// argument for why an id is not an address.
        ///
        /// **`#[serde(default)]`, and the default is *unknown* rather than
        /// *none*.** A park committed before this field existed loads as
        /// [`ResumeAddress::unstated`], and every reader owes that value the
        /// reading *"this park does not say where its work is"* — never
        /// *"there is nowhere else its work could be"*. The two differ by
        /// exactly the false proof this field was added to remove, so the
        /// distinction is not stylistic: `super::reconciler::child_state`
        /// refuses to answer `Finished` about a child whose last terminal is
        /// `Success` when the chain it was given is unstated, precisely
        /// because an unstated chain cannot rule out a segment it cannot see.
        ///
        /// Absence here **cannot widen anything**, which is why a default is
        /// safe on this field where it is refused on [`LoopState::placement`]
        /// and [`LoopState::journal_seq`]: the value is read by a diagnosis
        /// that fails towards *suspected* rather than towards *proved*, and by
        /// nothing that schedules, claims or admits.
        #[serde(default)]
        resume: ResumeAddress,
    },
}

/// Where a park's work is addressed once the segment that committed it ended.
///
/// # A child execution id is a NAME, and the reconciler needs an ADDRESS
///
/// `child_execution_ids` above names the executions a parent is waiting on. It
/// does not say which **loop-state key** any of that work is committed under,
/// and those two are not the same string: `executor.rs`'s
/// `loop_state_execution_id` folds a resume generation, a nesting chain and a
/// refinement pass index into the id before it becomes an `ExecutionKey`. A
/// delegated child's pass 0 therefore commits its `RunEnded { Success }` under
/// the bare id while its refinement pass 1 runs — doing real work — under
/// `{child}-p1`.
///
/// A reader holding only the bare id reads pass 0's terminal and concludes the
/// child is finished. That is not a hypothetical: it is what
/// `super::reconciler::ParkReason::EveryChildHasFinished` was proved from
/// before this field existed, past a fifteen-minute grace a refinement pass
/// routinely outlives.
///
/// # Why the addresses are recorded rather than derived
///
/// The spelling of a segment key belongs to `executor.rs` — one function,
/// `loop_state_execution_id`, is the only thing that composes one. A reader
/// that re-derived `{child}-p1` would be a second copy of that spelling in a
/// module that cannot see the first, and the two would drift the first time a
/// suffix changed. So the producer spells the keys with the one function that
/// owns the spelling and writes them down, and the reader only reads.
///
/// It also records a bound the reader could not recover at all: **how far the
/// chain goes**. `MAX_REFINEMENT_PASSES` is a private constant of
/// `executor.rs`, and a chain that is merely *probed until absent* cannot tell
/// an exhausted chain from one whose next segment has not committed yet — the
/// first proves the child is done and the second proves nothing.
///
/// **And the bound is the WRITER's, which is what a reader has to hold onto.**
/// A non-empty chain says *this producer believed it named every pass*, never
/// *this binary's `MAX_REFINEMENT_PASSES` was that number*. Nothing on the park
/// records the bound it was written under, so raising `MAX_REFINEMENT_PASSES`
/// makes every park **already on disk** one segment short, and
/// `super::reconciler::child_state` would take those chains as complete and
/// prove `EveryChildHasFinished` about a child working under the new last pass
/// — the same false proof this field removed, one pass further out. Raising
/// that constant therefore owes a migration or a written bound on the park; it
/// is not a constant that can be edited alone.
///
/// # What is deliberately NOT here
///
/// The `-r{n}` resume axis of a *child*. Its generations are
/// `prior offset + prior iterations`, so they are strictly increasing and not
/// contiguous, and a parent cannot know at park time which one a child will
/// take. That axis stays a **false negative** — a resumed child leaves a
/// *resumable* terminal at the key it left, which reads as live and leaves the
/// park alone. That is the direction that hides findings rather than inventing
/// them, and it is why closing the `-p` axis alone is what makes the ground
/// sound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeAddress {
    /// The loop-state key **this run's own** work continues under after the
    /// park.
    ///
    /// This is the field that tells an abandoned park from a hung one. A
    /// `Children` wake is a durable readiness receipt, while the run itself is
    /// resumed through the pause record its outcome carries **under this
    /// different execution key**. A reader that could not name that key had to
    /// report every such park identically, whether the work had moved on or
    /// whether nothing had happened at all.
    ///
    /// `None` means *not stated* — a park committed before this field existed,
    /// or one whose pause record named no execution id. It does **not** mean
    /// the run was never resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_resumes_as: Option<String>,
    /// Every loop-state key each named child's work may be addressed by.
    ///
    /// Keyed by the child's bare execution id rather than positional against
    /// `child_execution_ids`, so a producer that writes the two lists in
    /// different orders mis-addresses nothing. A child with no entry here is
    /// **unstated**, which is the reading an old park's empty list gets for
    /// every one of its children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_segments: Vec<ChildSegments>,
}

impl ResumeAddress {
    /// The address a park carries when it does not carry one.
    ///
    /// A named constructor rather than `Default::default()` at the call sites,
    /// because *"unstated"* is a reading a reader has to apply and *"default"*
    /// is a reading that invites being skipped.
    pub fn unstated() -> Self {
        Self::default()
    }

    /// The segment keys this park names for one child, nearest-first, or `None`
    /// when it names none.
    ///
    /// An empty list answers `None` for the same reason: a producer that wrote
    /// an entry with no segments in it has said nothing, and the caller must
    /// take the unstated path rather than treat a zero-length chain as an
    /// exhausted one.
    pub fn segments_for(&self, child_execution_id: &str) -> Option<&[String]> {
        self.child_segments
            .iter()
            .find(|chain| chain.child_execution_id == child_execution_id)
            .map(|chain| chain.segments.as_slice())
            .filter(|segments| !segments.is_empty())
    }
}

/// Every loop-state key one named child's work may be addressed by,
/// nearest-first from the key its first segment took.
///
/// Nearest-first is load-bearing: the reader walks the chain in order and the
/// **last** entry it finds committed is the child's current segment, so a list
/// written in another order would judge the child by a segment it had already
/// left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildSegments {
    /// The child's bare execution id, as it appears in `child_execution_ids`.
    pub child_execution_id: String,
    /// The keys, first segment first. Never empty when written by a producer
    /// that has anything to say; see [`ResumeAddress::segments_for`].
    pub segments: Vec<String>,
}

impl WaitReason {
    /// Park on children, refusing a park that nothing can wake.
    ///
    /// Returns `None` when the live-child set is empty, which the caller must
    /// read as *"do not park — consume the completed children as results"*. A
    /// constructor rather than a validation the caller may skip, because the
    /// failure it prevents is a run that hangs forever with nothing logged.
    ///
    /// Parks with an **unstated** address. A caller that can name where the
    /// work is should use [`WaitReason::children_at`] instead; this spelling
    /// stays because a park with no address is still a correct park — it is
    /// only a less diagnosable one.
    pub fn children(live_child_execution_ids: Vec<String>) -> Option<Self> {
        Self::children_at(live_child_execution_ids, ResumeAddress::unstated())
    }

    /// The same refusal, for a caller that can say where the work is.
    pub fn children_at(
        live_child_execution_ids: Vec<String>,
        resume: ResumeAddress,
    ) -> Option<Self> {
        if live_child_execution_ids.is_empty() {
            return None;
        }
        Some(WaitReason::Children {
            child_execution_ids: live_child_execution_ids,
            resume,
        })
    }

    /// The token whose resolution makes this execution runnable again.
    ///
    /// One token per park rather than one per child: the parent wakes **once**
    /// on `delegation_results_ready` and reads the children's outcomes, rather
    /// than waking per child. That is the existing runtime's semantic and this
    /// preserves it.
    ///
    /// # The token says WHAT is waited on, not WHICH completion answered
    ///
    /// A run may park on one token more than once — a child that reports twice
    /// is the ordinary case — so the token alone cannot tell a re-delivered
    /// report from a second one. That is why
    /// [`super::store::LoopStateStore::resolve_wake`] takes a **resolution id**
    /// beside this token: the completer's own event identity, stable across a
    /// retry of the same completion and different for a genuinely new one. Only
    /// the completer can answer that, which is why it supplies it rather than
    /// the store inferring it. A job runner's attempt id and a child's
    /// completion id are the two natural values.
    ///
    /// # The token is derived from the IDS and from nothing else
    ///
    /// [`ResumeAddress`] is deliberately not in it, and the `..` below is the
    /// enforcement rather than an abbreviation. A completer computes this token
    /// from **its own** view of the child set — it has the ids and it has never
    /// seen the parent's park — so folding anything the completer cannot know
    /// into the token would produce a value nothing resolves and a park that
    /// hangs forever. Adding the address therefore had to leave this function's
    /// answer byte-identical, and does: a park committed before the field
    /// existed and one committed after it resolve to the same string.
    pub fn wake_token(&self) -> String {
        match self {
            WaitReason::Job { job_id } => format!("job:{job_id}"),
            WaitReason::Children {
                child_execution_ids,
                ..
            } => {
                // Order-independent so a parent that recorded its children in a
                // different order still resolves to the same token — the
                // completer computes this from its own view.
                let mut sorted = child_execution_ids.clone();
                sorted.sort();
                format!("children:{}", sorted.join(","))
            },
        }
    }
}

/// Which worker is asking.
///
/// A newtype rather than a `String` because it is compared for equality against
/// a placement and against a lease holder, and both comparisons decide who may
/// mutate an execution. A bare string in those positions is one typo away from
/// comparing an execution id to a worker id and finding they differ.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkerId(String);

impl WorkerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn recovery_admission_worker_id(token: &str) -> WorkerId {
    WorkerId::new(format!("recovery-admission-{token}"))
}

impl std::fmt::Display for WorkerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_checkpoints_refuse_unknown_versions_and_oversized_payloads() {
        let current = ResolveCheckpoint::try_new(serde_json::json!({"decision": "continue"}))
            .expect("small checkpoint");
        assert!(current.require_current().is_ok());

        let mut future = current;
        future.version = ResolveCheckpoint::VERSION + 1;
        assert!(
            future.require_current().is_err(),
            "a cold worker must never guess how to interpret a future capsule"
        );

        let oversized = serde_json::Value::String("x".repeat(ResolveCheckpoint::MAX_BYTES));
        assert!(
            ResolveCheckpoint::try_new(oversized.clone()).is_err(),
            "JSON framing makes this larger than the checkpoint byte boundary"
        );
        let deserialized_shape = ResolveCheckpoint {
            version: ResolveCheckpoint::VERSION,
            payload: oversized,
        };
        assert!(
            deserialized_shape.require_current().is_err(),
            "a deserialized value must pass the same byte boundary as a locally minted one"
        );
    }

    #[test]
    fn exact_continuation_round_trips_non_bare_segment_and_protective_counters() {
        use crate::magician_v2::execution::agentic::{AgenticPauseState, EnvironmentState};

        let exact_segment = "exec-r-name-nested-r2-n5-p3-s4-a1";
        let mut pause = AgenticPauseState::new(
            17,
            "preserved goal",
            "preserved criteria",
            EnvironmentState::Uninitialized,
            "bounded prior actions".to_string(),
            4000,
            3,
        );
        pause.execution_id = Some("exec-r-name".to_string());
        pause.task_id = Some("task-1".to_string());
        pause.principal = Some("principal-1".to_string());
        pause.workspace = Some("workspace-1".to_string());
        pause.stateless_parked_segment = Some(exact_segment.to_string());
        pause.work_budget_consumed_ms = 91_337;
        pause.llm_tokens_used = 44_009;

        let checkpoint =
            IterationContinuationCheckpoint::try_new(&pause).expect("bounded continuation");
        let restored = checkpoint.decode().expect("current continuation");
        assert_eq!(restored.execution_id.as_deref(), Some("exec-r-name"));
        assert_eq!(
            restored.stateless_parked_segment.as_deref(),
            Some(exact_segment),
            "nested/refinement/pipeline suffixes are opaque exact addresses"
        );
        assert_eq!(restored.work_budget_consumed_ms, 91_337);
        assert_eq!(restored.llm_tokens_used, 44_009);

        let binding = LoopSegmentBinding {
            base_execution_id: "exec-r-name".to_string(),
            exact_segment_id: exact_segment.to_string(),
            preseed_admission_token: None,
        };
        assert_ne!(binding.base_execution_id, binding.exact_segment_id);
    }

    #[test]
    fn a_read_costs_one_lock_and_no_string_copies() {
        // The shape claim behind holding an `Arc` inside the lock rather than the
        // value: two reads of the same identity must be the same allocation, not
        // two deep copies. This is what makes collapsing twelve mutexes into one
        // cheaper than what it replaced rather than merely tidier.
        let slot = ExecutorRunIdentity::default();
        slot.update(|identity| {
            identity.principal = Some("owner".to_string());
            identity.workspace = Some("default".to_string());
            identity.task_id = Some("task-1".to_string());
        });

        let first = slot.get();
        let second = slot.get();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "two reads must share one allocation; deep-cloning per read is the \
             cost this type exists to remove"
        );
        assert_eq!(first.principal.as_deref(), Some("owner"));
        assert_eq!(first.workspace.as_deref(), Some("default"));
        assert_eq!(first.task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn a_reader_holding_a_snapshot_never_sees_a_half_applied_change() {
        // Copy-on-write, and the reason for it: a reader that took the identity
        // before an owner transition must keep the identity it took. Mutating in
        // place would let it observe the new agent with the old ceiling — the
        // exact pairing the ceiling exists to prevent.
        let slot = ExecutorRunIdentity::default();
        slot.update(|identity| {
            identity.agent_id = Some("parent".to_string());
            identity.browser_transports = vec!["cdp".to_string(), "headed".to_string()];
        });
        let before = slot.get();

        slot.update(|identity| {
            identity.agent_id = Some("narrow-delegate".to_string());
            identity.browser_transports = vec!["cdp".to_string()];
        });
        let after = slot.get();

        assert_eq!(before.agent_id.as_deref(), Some("parent"));
        assert_eq!(before.browser_transports.len(), 2, "the snapshot is frozen");
        assert_eq!(after.agent_id.as_deref(), Some("narrow-delegate"));
        assert_eq!(
            after.browser_transports,
            vec!["cdp".to_string()],
            "a delegate's narrower ceiling replaces its parent's, never merges"
        );
    }

    #[test]
    fn a_poisoned_lock_still_narrows_the_ceiling() {
        // REGRESSION GUARD for the policy this collapse forced a choice about.
        // `set_executor_browser_transports` recovered from poisoning on purpose;
        // `set_executor_current_agent` beside it skipped. Under one lock the
        // skipping policy would leave a previous owner's WIDER ceiling in place
        // across a transition, which is the widening that comment warned about.
        let slot = ExecutorRunIdentity::default();
        slot.update(|identity| {
            identity.browser_transports = vec!["cdp".to_string(), "headed".to_string()];
            // The task spawn cap rides the same lock. It used to have the
            // OPPOSITE policy — `max_spawned_tasks_or_deny` returned `Some(0)` on
            // poison, denying every spawn, because a lock it could not read left
            // it unable to say what the cap was. Under one lock the two policies
            // cannot both hold, and recovery is the correct one: the cap is
            // readable, so discarding it would stop an autonomous cycle spawning
            // anything because something unrelated panicked.
            identity.max_spawned_tasks = Some(5);
        });

        let poisoner = slot.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.inner.lock().unwrap();
            panic!("poison the identity lock");
        })
        .join();

        // Through `update`, which is what every production write uses. An
        // earlier cut of this test exercised `set` instead — which recovers
        // identically, but which nothing in the runtime calls, so it guarded the
        // one method the policy never applies to.
        slot.update(|identity| identity.browser_transports = vec!["cdp".to_string()]);

        assert_eq!(
            slot.get().browser_transports,
            vec!["cdp".to_string()],
            "a poisoned lock must not preserve a wider predecessor's ceiling"
        );
        assert_eq!(
            slot.get().max_spawned_tasks,
            Some(5),
            "and the cap set BEFORE the poisoning must still be readable — this \
             is what `max_spawned_tasks_or_deny` used to answer with Some(0), \
             discarding a cap the runtime still held"
        );
    }

    #[test]
    fn seeding_from_the_context_keeps_what_the_context_does_not_own() {
        // The API contract that keeps the seeding path safe: `apply_context`
        // writes what the context owns and nothing else.
        //
        // Precisely, because the tempting overstatement is wrong: the later
        // writers of these three fields ARE unconditional, so a wholesale replace
        // would not lose them forever. What it would do is blank them for the
        // span between seeding and those writers — a span containing an `.await`
        // — so a dispatch racing that window would read an identity stripped of
        // three fields the run still holds. The twelve separate writes had no
        // such window, which is why collapsing them is where it could appear.
        let mut identity = RunIdentity {
            ui_thread: Some("thread-7".to_string()),
            primitive_goal: Some("the user's goal".to_string()),
            primitive_success_criteria: Some("the criteria".to_string()),
            principal: Some("stale".to_string()),
            ..RunIdentity::default()
        };

        let mut ctx = super::super::super::types::AgenticContext::new("goal", "criteria");
        ctx.principal = Some("fresh".to_string());
        identity.apply_context(&ctx);

        assert_eq!(identity.principal.as_deref(), Some("fresh"), "context wins");
        assert_eq!(
            identity.ui_thread.as_deref(),
            Some("thread-7"),
            "the UI thread is resolved by a service lookup, not by the context"
        );
        assert_eq!(
            identity.primitive_goal.as_deref(),
            Some("the user's goal"),
            "the primitive goal is written later in setup and must survive seeding"
        );
        assert_eq!(
            identity.primitive_success_criteria.as_deref(),
            Some("the criteria")
        );
    }

    #[test]
    fn an_identity_survives_the_wire() {
        // A second holder is TOLD this rather than inheriting it, so it has to
        // survive a round trip. Absent fields stay absent: `None` for the work
        // authority means no authority, never unrestricted.
        let identity = RunIdentity {
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: Some("presto".to_string()),
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            browser_transports: vec!["cdp".to_string()],
            max_spawned_tasks: Some(3),
            ..RunIdentity::default()
        };

        let encoded = serde_json::to_string(&identity).expect("serialize");
        let restored: RunIdentity = serde_json::from_str(&encoded).expect("read back");
        assert_eq!(restored, identity);
    }

    #[test]
    fn a_ceiling_cannot_go_missing_from_the_wire() {
        // REGRESSION GUARD for a real hole this file shipped. Both ceilings were
        // `#[serde(default, skip_serializing_if = ..)]`, and BOTH default to the
        // WIDE value: `BrowserTransportCeiling::parse(&[])` permits every
        // transport, and `create_task` enforces the spawn cap only under
        // `if let Some(..)`. So a record that merely lacked either key resumed
        // unrestricted, and no reader could tell "declared nothing" from "field
        // dropped".
        //
        // They are required now, so absence is a parse failure — the run refuses
        // to load rather than loading wide. Asserted by DELETING each key from a
        // real encoding rather than by inspecting the attributes, because the
        // attribute is not the property; what a foreign writer can omit is.
        let identity = RunIdentity {
            browser_transports: vec!["cdp".to_string()],
            max_spawned_tasks: Some(3),
            ..RunIdentity::default()
        };
        let encoded = serde_json::to_string(&identity).expect("serialize");

        for ceiling in ["browser_transports", "max_spawned_tasks"] {
            let mut value: serde_json::Value =
                serde_json::from_str(&encoded).expect("re-read as a document");
            assert!(
                value
                    .as_object_mut()
                    .expect("an identity encodes as an object")
                    .remove(ceiling)
                    .is_some(),
                "{ceiling} must be WRITTEN, or a reader has nothing to miss"
            );
            let stripped = serde_json::to_string(&value).expect("re-encode");

            assert!(
                serde_json::from_str::<RunIdentity>(&stripped).is_err(),
                "a record without `{ceiling}` must refuse to load; loading it \
                 silently hands the run an unrestricted ceiling"
            );
        }
    }

    #[test]
    fn a_run_that_lost_its_whole_identity_refuses_to_load() {
        // The same hole one level up: `LoopState.identity` was `#[serde(default)]`,
        // and `RunIdentity::default()` is unrestricted in both ceilings. One
        // missing key would have resumed a run with no browser ceiling and no
        // spawn cap at once.
        let encoded =
            serde_json::to_string(&LoopState::new(RunIdentity::default())).expect("serialize");
        let mut value: serde_json::Value =
            serde_json::from_str(&encoded).expect("re-read as a document");
        value
            .as_object_mut()
            .expect("a loop state encodes as an object")
            .remove("identity");

        assert!(
            serde_json::from_str::<LoopState>(&value.to_string()).is_err(),
            "a state without an identity must refuse to load rather than \
             defaulting to unrestricted"
        );
    }

    #[test]
    fn an_empty_identity_costs_only_its_two_required_ceilings() {
        // Every OPTIONAL field is `skip_serializing_if`, so a run that carries no
        // identity adds no bytes for them. The two CEILINGS are the exception,
        // and have to be: they are required on the wire precisely so a reader can
        // tell "declared nothing" — an explicit `null`, an empty list — from
        // "field dropped", which is a parse failure.
        //
        // An earlier version of this asserted `"{}"`, which stopped being true
        // the moment the ceilings became required and was failing in the tree.
        // Worth keeping rather than deleting: the way to make `"{}"` true again
        // is to put `skip_serializing_if` back on a ceiling, which is exactly the
        // regression, so this asserts the new shape rather than dropping the
        // guard.
        let encoded = serde_json::to_string(&RunIdentity::default()).expect("serialize");
        assert_eq!(
            encoded,
            r#"{"max_spawned_tasks":null,"browser_transports":[]}"#
        );
    }
}

#[cfg(test)]
mod loop_state_tests {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::effects::{
        BatchMode, CommittedActRef, EffectId, PendingBatch, PendingEffect, RetrySafety,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;

    /// A fixed instant every case in this module judges against.
    ///
    /// A constant rather than `Utc::now()`, because the pin expiry these cases
    /// exercise is a comparison against a clock — a fixture that read the real
    /// one would make "is this pin live" depend on when the suite ran, which is
    /// the exact shape of flake `PIN_TTL_MS` would otherwise introduce into
    /// every commit case at once.
    const TEST_NOW_MS: i64 = 1_700_000_000_000;

    fn state() -> LoopState {
        LoopState::new(RunIdentity {
            execution_id: Some("exec-1".to_string()),
            ..RunIdentity::default()
        })
    }

    #[test]
    fn a_fresh_state_starts_at_the_first_phase_of_the_first_iteration() {
        let state = state();
        assert_eq!(state.cursor.iteration, 1);
        assert_eq!(state.cursor.phase, Phase::Prepare);
        assert_eq!(state.journal_seq, 0);
        assert_eq!(state.placement, Placement::Portable);
        assert!(state.is_runnable_at(0));
    }

    #[test]
    fn an_absent_deadline_is_not_a_passed_one() {
        // The distinction a sentinel would erase. A run with no wall-clock
        // deadline must not be killed by a comparison against zero.
        let mut state = state();
        assert!(!state.deadline_passed(i64::MAX));

        state.deadline_at_ms = Some(1_000);
        assert!(!state.deadline_passed(999));
        assert!(state.deadline_passed(1_000));
        assert!(!state.is_runnable_at(1_000));
    }

    #[test]
    fn a_parked_execution_is_not_runnable_however_its_timers_read() {
        let mut state = state();
        state.wait = Some(WaitReason::Job {
            job_id: "coding-7".to_string(),
        });
        assert!(
            !state.is_runnable_at(i64::MAX),
            "something outside has to resolve the wait first"
        );
    }

    #[test]
    fn a_retry_delay_is_honoured_by_the_scheduler_rather_than_slept_on() {
        let mut state = state();
        state.runnable_at_ms = 5_000;
        assert!(!state.is_runnable_at(4_999));
        assert!(state.is_runnable_at(5_000));
    }

    #[test]
    fn parking_on_children_with_no_live_child_is_refused() {
        // The `WaitingChildren` hang: an idempotent retry parks a parent with no
        // live child left to wake it. The design calls this a required test
        // case rather than a note.
        assert_eq!(WaitReason::children(Vec::new()), None);
        let parked = WaitReason::children(vec!["child-1".to_string()])
            .expect("one live child is enough to park on");
        assert_eq!(parked.wake_token(), "children:child-1");
    }

    #[test]
    fn a_children_wake_token_does_not_depend_on_the_order_the_parent_listed_them() {
        let one = WaitReason::children(vec!["b".to_string(), "a".to_string()]).expect("live");
        let other = WaitReason::children(vec!["a".to_string(), "b".to_string()]).expect("live");
        assert_eq!(one.wake_token(), other.wake_token());
    }

    #[test]
    fn a_park_committed_before_addresses_existed_still_loads_and_resolves_the_same_token() {
        // The on-disk shape of a `Children` park as it was written before
        // `ResumeAddress` existed, spelled by hand rather than by round-tripping
        // a value — a round trip through today's type would write the new key
        // and pin nothing.
        let legacy = r#"{"wait":"children","child_execution_ids":["child-1","child-2"]}"#;
        let parked: WaitReason = serde_json::from_str(legacy).expect("an old park still loads");

        let WaitReason::Children {
            child_execution_ids,
            resume,
        } = &parked
        else {
            panic!("an old children park must not load as some other wait");
        };
        assert_eq!(child_execution_ids.len(), 2);
        assert_eq!(
            resume,
            &ResumeAddress::unstated(),
            "an absent address must read as UNSTATED — a reader that took it for `there is \
             nowhere else` would prove exactly the thing the field was added to disprove"
        );
        assert!(
            resume.segments_for("child-1").is_none(),
            "an unstated address names no segment for any child"
        );

        // The token is the completer's contract and it is computed from the ids.
        // An old park and a new one over the same children must resolve to one
        // string, or every park committed before this change hangs forever.
        let addressed = WaitReason::children_at(
            vec!["child-1".to_string(), "child-2".to_string()],
            ResumeAddress {
                run_resumes_as: Some("parent-r9".to_string()),
                child_segments: vec![ChildSegments {
                    child_execution_id: "child-1".to_string(),
                    segments: vec!["child-1".to_string(), "child-1-p1".to_string()],
                }],
            },
        )
        .expect("two live children");
        assert_eq!(parked.wake_token(), addressed.wake_token());
    }

    #[test]
    fn an_address_names_the_segments_of_the_child_it_was_written_for() {
        // Keyed, not positional: the lookup must follow the id rather than the
        // order, because the two lists are written by different expressions.
        let resume = ResumeAddress {
            run_resumes_as: None,
            child_segments: vec![
                ChildSegments {
                    child_execution_id: "b".to_string(),
                    segments: vec!["b".to_string(), "b-p1".to_string()],
                },
                ChildSegments {
                    child_execution_id: "a".to_string(),
                    segments: vec!["a".to_string()],
                },
                // A producer that had nothing to say still says nothing.
                ChildSegments {
                    child_execution_id: "c".to_string(),
                    segments: Vec::new(),
                },
            ],
        };
        assert_eq!(resume.segments_for("a"), Some(&["a".to_string()][..]));
        assert_eq!(
            resume.segments_for("b"),
            Some(&["b".to_string(), "b-p1".to_string()][..])
        );
        assert_eq!(
            resume.segments_for("c"),
            None,
            "an empty chain is unstated, not exhausted"
        );
        assert_eq!(resume.segments_for("missing"), None);
    }

    #[test]
    fn a_pinned_execution_admits_only_its_own_worker() {
        let mine = WorkerId::new("worker-a");
        let theirs = WorkerId::new("worker-b");

        let mut state = state();
        assert!(
            state.claimable_by(&mine, TEST_NOW_MS),
            "portable is claimable by anyone"
        );

        state.placement = Placement::Pinned {
            worker: mine.clone(),
            pinned_until_ms: TEST_NOW_MS + 1,
        };
        assert!(state.claimable_by(&mine, TEST_NOW_MS));
        assert!(!state.claimable_by(&theirs, TEST_NOW_MS));
    }

    // ========================================================================
    // The commit projection
    // ========================================================================

    /// A context with an open work-budget segment that started `ago` in the past.
    ///
    /// `checked_sub`, not `-`: subtracting from an `Instant` panics on underflow,
    /// and a machine whose monotonic clock started moments ago would make the
    /// panic a flake rather than a failure.
    fn ctx_with_open_segment(
        ago: std::time::Duration,
    ) -> super::super::super::types::AgenticContext {
        let mut ctx = super::super::super::types::AgenticContext::new("goal", "criteria");
        ctx.work_budget_segment_started_at = std::time::Instant::now().checked_sub(ago);
        assert!(
            ctx.work_budget_segment_started_at.is_some(),
            "the fixture needs an open segment; without one this file's budget \
             cases would assert against a closed one and pass on a projection \
             that folded nothing"
        );
        ctx
    }

    fn cdp_only_state() -> LoopState {
        LoopState::new(RunIdentity {
            execution_id: Some("exec-1".to_string()),
            browser_transports: vec!["cdp".to_string()],
            ..RunIdentity::default()
        })
    }

    #[test]
    fn a_commit_charges_the_open_segment() {
        // The defect: `work_budget_consumed_ms` is a half-open accumulator, and
        // the open half is an `Instant` that means nothing to another process. A
        // commit that published only the closed half would hand the next worker
        // a budget missing everything this worker spent — silently, because a
        // budget that comes out low does not error, it just lets the run overrun.
        let mut ctx = ctx_with_open_segment(std::time::Duration::from_secs(30));
        // The closed half lives on the CONTEXT, and both halves must appear.
        ctx.work_budget_consumed_ms = 1_000;
        let worker = WorkerId::new("worker-a");
        let mut state = cdp_only_state();

        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("a cdp-only portable run projects");

        // Elapsed only grows, so the lower bound has no upper edge and cannot
        // flake on a slow machine.
        assert!(
            state.work_budget_consumed_ms >= 31_000,
            "the projection charged {}ms; the context's closed half was 1_000 and \
             its open segment had been running 30s",
            state.work_budget_consumed_ms
        );

        // And the total REPLACES the state's previous answer rather than
        // accumulating onto it. Asserted with the segment CLOSED so the number
        // is exact and the assertion has no timing edge: an accumulating fold
        // would publish 49_000 here. An earlier cut used a decoy larger than the
        // charge and a `<` assertion, which said the same thing less precisely
        // and only by projecting a state whose budget went backwards — the one
        // shape `for_commit` now refuses.
        let mut closed = super::super::super::types::AgenticContext::new("goal", "criteria");
        closed.work_budget_segment_started_at = None;
        closed.work_budget_consumed_ms = 42_000;
        let mut replaced = cdp_only_state();
        replaced.work_budget_consumed_ms = 7_000;
        replaced
            .for_commit(CommitPoint {
                ctx: &closed,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("a closed segment still projects");
        assert_eq!(
            replaced.work_budget_consumed_ms, 42_000,
            "the context is the source of truth and the previous commit's total \
             must be replaced, not accumulated onto"
        );
    }

    #[test]
    fn a_driver_that_did_not_seed_the_budget_is_refused_rather_than_refunded() {
        // The silent half of Decision 3, and the reason the load-side obligation
        // is checked rather than only written down. `for_commit` REPLACES the
        // total from the context, so a worker that loaded a state carrying 45
        // seconds and ran a phase without seeding its own context publishes
        // roughly zero — refunding the whole run's budget, on every handoff,
        // reporting nothing, because a budget that comes out low does not error.
        let unseeded = ctx_with_open_segment(std::time::Duration::from_secs(1));
        assert_eq!(
            unseeded.work_budget_consumed_ms, 0,
            "the fixture is a FRESH worker's context; without that this asserts \
             nothing about seeding"
        );
        let worker = WorkerId::new("worker-a");
        let mut state = cdp_only_state();
        state.work_budget_consumed_ms = 45_000;

        let error = state
            .for_commit(CommitPoint {
                ctx: &unseeded,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect_err("an unseeded projection must be refused, not published");
        assert!(
            matches!(
                error,
                LoopStateRefusal::WorkBudgetWentBackwards {
                    committed_ms: 45_000,
                    ..
                }
            ),
            "got {error}"
        );
        assert_eq!(
            state.work_budget_consumed_ms, 45_000,
            "and the refusal left the committed total where it was"
        );

        // THE EDGE, and it is the whole difference between `<` and `<=`. A
        // projection that publishes exactly what is already committed — a seeded
        // context whose segment is closed, which is every commit taken without
        // running a phase — must be ACCEPTED. Written with `<=` this guard would
        // refuse it, and `leave_park` (which commits without running a phase) is
        // exactly that shape: the run could never leave a park.
        let mut exact = super::super::super::types::AgenticContext::new("goal", "criteria");
        exact.work_budget_segment_started_at = None;
        exact.work_budget_consumed_ms = 45_000;
        state
            .for_commit(CommitPoint {
                ctx: &exact,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("publishing exactly the committed total is not going backwards");
        assert_eq!(state.work_budget_consumed_ms, 45_000);

        // A driver that DID seed still commits, or this guard would refuse every
        // handoff rather than the ones that lost the seed.
        let mut seeded = ctx_with_open_segment(std::time::Duration::from_secs(1));
        seeded.work_budget_consumed_ms = state.work_budget_consumed_ms;
        state
            .for_commit(CommitPoint {
                ctx: &seeded,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("a seeded context projects");
        assert!(
            state.work_budget_consumed_ms >= 46_000,
            "the seeded projection charged {}ms; 45s was carried and the open \
             segment had been running one more",
            state.work_budget_consumed_ms
        );
    }

    #[test]
    fn a_commit_does_not_bank_time_for_work_that_may_be_discarded() {
        // Why the fold is read-side. A mutating close (`consumed_ms += elapsed;
        // started_at = Some(now)`) charges a phase that then loses its lease and
        // commits nothing — so a run that keeps losing its lease burns its whole
        // budget without advancing an iteration. The live context must come out
        // of a projection exactly as it went in.
        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(30));
        let opened_at = ctx.work_budget_segment_started_at;
        let closed_total_before = ctx.work_budget_consumed_ms;
        let worker = WorkerId::new("worker-a");
        let mut state = cdp_only_state();

        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("first projection");
        let first = state.work_budget_consumed_ms;

        assert_eq!(
            ctx.work_budget_consumed_ms, closed_total_before,
            "the projection must not move the context's closed total"
        );
        assert_eq!(
            ctx.work_budget_segment_started_at, opened_at,
            "the projection must not reopen the segment; doing so is what banks \
             time for an attempt that may never commit"
        );

        // And it is idempotent in the direction that matters: a commit rejected
        // by the store's compare-and-swap and retried recomputes the same base
        // plus a slightly larger elapsed, never the base twice.
        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("retry after a conflict");
        assert!(
            state.work_budget_consumed_ms >= first,
            "elapsed only grows: {} then {}",
            first,
            state.work_budget_consumed_ms
        );
        assert!(
            state.work_budget_consumed_ms < first + 30_000,
            "a second projection charged the 30s segment again ({first} → {}); \
             the fold is not idempotent",
            state.work_budget_consumed_ms
        );
    }

    #[test]
    fn only_one_place_writes_the_work_budget_total() {
        // The realistic regression is not deleting the fold — it is a second
        // writer added later that charges differently, or not at all. A runtime
        // assertion cannot see "how many places assign this field", so the guard
        // is a scan, in the idiom `outcome.rs` already uses.
        //
        // Its reach, stated rather than assumed: this catches a second writer in
        // THIS file. It does not catch a future driver that commits a state it
        // built without projecting — only making `LoopStateStore::commit` take a
        // type whose constructor performs the fold would do that, and that
        // changes a trait two store impls and a 22-case contract suite implement.
        const SOURCE: &str = include_str!("state.rs");
        // Split so the needle does not appear verbatim in the file it scans —
        // which would make the count two and the assertion permanently wrong
        // about a file that scans itself.
        const FIELD: &str = concat!("self.", "work_budget_consumed_ms");
        // EVERY assignment spelling, not one. An earlier cut scanned for
        // `<field> =` alone, so `<field> += elapsed` — the mutating close this
        // whole decision rejects — walked straight past it. That is the same
        // defect `no_phase_and_no_iteration_blocks_on_a_backoff_of_its_own`
        // records in `outcome.rs`: scanning for one spelling let three others
        // through.
        const COMPOUND: [&str; 10] = ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="];
        let writes = SOURCE
            .match_indices(FIELD)
            .filter(|(at, _)| {
                let after = SOURCE[at + FIELD.len()..].trim_start();
                // A bare `=` that is not `==`, or any compound assignment. A
                // comparison (`>=`, `<`, `==`) is a read and does not count.
                (after.starts_with('=') && !after.starts_with("=="))
                    || COMPOUND.iter().any(|operator| after.starts_with(operator))
            })
            .count();
        assert_eq!(
            writes, 1,
            "`LoopState::for_commit` must be the only writer of the work-budget \
             total; a second one charges on a different rule and the difference \
             shows up as a driver-parity failure, not as a budget bug"
        );
    }

    #[test]
    fn placement_cannot_go_missing_from_the_wire() {
        // The same hole `a_ceiling_cannot_go_missing_from_the_wire` closes, one
        // field over: `Placement::default()` is `Portable`, the class ANY worker
        // may claim. A record that merely lost this key would have offered an
        // execution holding a process-local Chrome to a worker with no display.
        //
        // Asserted by DELETING the key from a real encoding and re-parsing,
        // because the serde attribute is not the property — what a foreign
        // writer can omit is.
        let mut state = cdp_only_state();
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-a"),
            pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
        };
        let encoded = serde_json::to_string(&state).expect("serialize");

        let mut value: serde_json::Value =
            serde_json::from_str(&encoded).expect("re-read as a document");
        assert!(
            value
                .as_object_mut()
                .expect("a loop state encodes as an object")
                .remove("placement")
                .is_some(),
            "placement must be WRITTEN, or a reader has nothing to miss"
        );

        assert!(
            serde_json::from_str::<LoopState>(&value.to_string()).is_err(),
            "a record without `placement` must refuse to load; loading it \
             silently offers a pinned execution to every worker"
        );
    }

    #[test]
    fn no_field_of_a_committed_state_may_go_missing_from_the_wire() {
        // `placement` and `identity` were made required one at a time, each for
        // its own reason. The reason generalises, and the remaining five fields
        // had exactly the same property: **every scalar on this type defaults to
        // the permissive or reset value**, and every one of them is written
        // unconditionally, so a reader cannot tell a legitimate zero from a
        // dropped key.
        //
        // What each absence bought, before this:
        //
        // - `journal_seq` → 0 → `Journal::orphaned(0)` is the WHOLE log, so the
        //   next append sweeps every record the run ever committed.
        // - `work_budget_consumed_ms` → 0 → the run's entire work budget is
        //   refunded, which is the exact failure Decision 3 exists to prevent,
        //   arriving through the load side rather than the commit side.
        // - `cursor` → iteration 1, first phase → a run nine iterations in
        //   restarts and re-spends every LLM call.
        // - `phase_attempts` → 0 → the quarantine counter resets on every load,
        //   so a run failing one phase forever is never quarantined.
        // - `runnable_at_ms` → 0 → runnable now, whatever backoff was committed.
        //
        // Asserted by DELETING each key from a real encoding and re-parsing,
        // because the attribute is not the property; what a foreign writer can
        // omit is. Every optional field stays optional and is not listed here —
        // `wait`, `deadline_at_ms`, `pending`, `iteration_checkpoint`,
        // `cold_reobserve` and `resolve_checkpoint` all mean "absent/inactive"
        // when absent, which is a different thing from "dropped".
        let mut state = cdp_only_state();
        state.cursor = LoopCursor {
            iteration: 9,
            phase: Phase::Apply,
        };
        state.journal_seq = 44;
        state.runnable_at_ms = 5_000;
        state.work_budget_consumed_ms = 61_000;
        state.phase_attempts = 2;
        let encoded = serde_json::to_string(&state).expect("serialize");

        for required in [
            "identity",
            "cursor",
            "journal_seq",
            "placement",
            "runnable_at_ms",
            "work_budget_consumed_ms",
            "phase_attempts",
        ] {
            let mut value: serde_json::Value =
                serde_json::from_str(&encoded).expect("re-read as a document");
            assert!(
                value
                    .as_object_mut()
                    .expect("a loop state encodes as an object")
                    .remove(required)
                    .is_some(),
                "`{required}` must be WRITTEN, or a reader has nothing to miss"
            );
            assert!(
                serde_json::from_str::<LoopState>(&value.to_string()).is_err(),
                "a record without `{required}` must refuse to load rather than \
                 resuming on this field's permissive default"
            );
        }

        // And the intact document still loads, or the loop above would be
        // satisfied by a type that refuses everything.
        assert_eq!(
            serde_json::from_str::<LoopState>(&encoded).expect("the intact record loads"),
            state
        );
    }

    #[test]
    fn a_portable_run_permitted_a_process_local_browser_is_never_published() {
        // Decision 2's invariant. A portable execution may be claimed by any
        // worker, so a ceiling that permits `headed` or `headless` describes a
        // run that can be told to open a Chrome on a machine that has none. The
        // ceiling and the placement are each individually correct; it is the
        // pairing that is wrong, which is why this is checked where the pairing
        // is published rather than where either half is set.
        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(30));
        let worker = WorkerId::new("worker-a");

        for (transports, why) in [
            (
                Vec::new(),
                "an empty ceiling parses as `allowed: None` and permits every \
                 transport, so it is the widest value of all",
            ),
            (
                vec!["cdp".to_string(), "headed".to_string()],
                "a portable run permitted `headed` can be scheduled anywhere and \
                 then told to open a local Chrome",
            ),
            (
                vec!["headless".to_string()],
                "and one that cannot use cdp at all is not portable in the sense \
                 this class means",
            ),
        ] {
            let mut state = LoopState::new(RunIdentity {
                browser_transports: transports.clone(),
                ..RunIdentity::default()
            });
            state.work_budget_consumed_ms = 7;

            let error = state
                .for_commit(CommitPoint {
                    ctx: &ctx,
                    worker: &worker,
                    now_ms: TEST_NOW_MS,
                    holds_user_typed_ephemeral_secret: false,
                })
                .expect_err(why);
            assert!(
                matches!(
                    error,
                    LoopStateRefusal::PortableRunMayReachAProcessLocalBrowser { .. }
                ),
                "{transports:?}: got {error}"
            );
            // A refused projection wrote nothing. The context above carries a
            // 30-second open segment, so a projection that charged before
            // validating would have moved this off 7.
            assert_eq!(
                state.work_budget_consumed_ms, 7,
                "a refused projection must leave the state as it found it"
            );
        }

        // And the same ceiling on a PINNED run is fine: it holds a worker, which
        // is what a process-local transport is for. Without this the assertions
        // above would pass on a projection that refused every state.
        let mut pinned = LoopState::new(RunIdentity {
            browser_transports: vec!["cdp".to_string(), "headed".to_string()],
            ..RunIdentity::default()
        });
        pinned.placement = Placement::Pinned {
            worker: worker.clone(),
            pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
        };
        pinned
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &worker,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("a pinned run may hold a wide ceiling");
    }

    #[test]
    fn every_transport_but_cdp_makes_a_portable_run_unpublishable() {
        // The "and nothing else" half of Decision 2's invariant, asserted over
        // `BrowserTransport::ALL` rather than over the two transports that exist
        // today. A fourth variant added to that enum joins this test on the same
        // commit that adds it — where naming them by hand would have let it be
        // silently permitted on a run any worker may claim.
        use crate::magician_v2::execution::primitive_dispatch::browser::session::BrowserTransport;

        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(1));
        let worker = WorkerId::new("worker-a");
        let mut checked = 0usize;
        for transport in BrowserTransport::ALL {
            if transport == BrowserTransport::Cdp {
                continue;
            }
            checked += 1;
            let mut state = LoopState::new(RunIdentity {
                browser_transports: vec!["cdp".to_string(), transport.label().to_string()],
                ..RunIdentity::default()
            });
            let error = state
                .for_commit(CommitPoint {
                    ctx: &ctx,
                    worker: &worker,
                    now_ms: TEST_NOW_MS,
                    holds_user_typed_ephemeral_secret: false,
                })
                .expect_err("a portable run permitted a process-local transport");
            assert!(
                matches!(
                    error,
                    LoopStateRefusal::PortableRunMayReachAProcessLocalBrowser { .. }
                ),
                "{}: got {error}",
                transport.label()
            );
        }
        assert!(
            checked >= 2,
            "the loop found {checked} non-cdp transports, so it has stopped \
             reading the enum it is supposed to police"
        );
    }

    #[test]
    fn a_run_that_took_a_user_typed_secret_is_pinned_to_the_worker_holding_it() {
        // Decision 4. `SecretStore.ephemeral` has no persistence path, so the
        // password or OTP a user typed lives in exactly one process. A run that
        // changes worker mid-flight finds its placeholders unresolvable and
        // re-prompts for the same OTP every time it is rescheduled.
        //
        // `holds_user_typed_ephemeral_secret: true` is DECORATIVE here since
        // 2026-08-28: every commit pins, so this passes identically with `false`.
        // Left set because it is the case the name describes and because the
        // `!claimable_by(&other)` half still bites — but read the assertion below
        // as "a run holding a secret is pinned", never as "a secret is what pins
        // it". The shape of this test is the last place the deleted gate can still
        // be inferred from. See
        // `every_commit_renews_the_pin_so_a_live_run_cannot_be_taken_from_under_it`.
        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(1));
        let holder = WorkerId::new("worker-a");
        let other = WorkerId::new("worker-b");
        let mut state = cdp_only_state();

        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: true,
            })
            .expect("project");

        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: holder.clone(),
                pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
            },
            "acquiring a user-typed ephemeral secret pins the run, for one TTL"
        );
        assert!(state.claimable_by(&holder, TEST_NOW_MS));
        assert!(
            !state.claimable_by(&other, TEST_NOW_MS),
            "the worker without the secret must not be offered the run"
        );
    }

    #[test]
    fn a_pin_is_never_released_by_a_later_commit() {
        // The narrowing rule `claimable_by` states, as a test. The tempting
        // implementation — derive placement from the flag on every commit — would
        // un-pin the run at the first commit taken after the loop's ephemeral
        // scope guard fires, which is precisely when the secret is still needed
        // by the effect that has not been re-dispatched yet.
        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(1));
        let holder = WorkerId::new("worker-a");
        let mut state = cdp_only_state();
        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: true,
            })
            .expect("project the pin");

        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("project again with no secret held");

        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: holder,
                // Same expiry because both commits are taken at the same
                // `now_ms`, NOT because the second one declined to renew — since
                // 2026-08-28 every commit renews. Deliberately left at one clock
                // reading: what this test is about is that the pin survives a
                // commit with no secret at all, and moving the clock here would
                // make it re-assert the renewal that
                // `every_commit_renews_the_pin_so_a_live_run_cannot_be_taken_from_under_it`
                // owns.
                pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
            },
            "a commit does not RELEASE a pin; only its expiry does, and only \
             later. A run returned to Portable here would be offered at once to \
             a worker that cannot resolve its placeholders"
        );
    }

    #[test]
    fn a_pinned_run_refuses_to_move_to_another_worker() {
        // Re-labelling the pin would make the record consistent and the run
        // broken: the pin exists because worker A holds something worker B does
        // not, and renaming the owner does not move the secret.
        let mut state = cdp_only_state();
        let first = WorkerId::new("worker-a");
        let second = WorkerId::new("worker-b");

        state
            .pin_to(&first, TEST_NOW_MS)
            .expect("the first pin is taken");
        state
            .pin_to(&first, TEST_NOW_MS)
            .expect("re-pinning to the same worker is accepted");

        let error = state
            .pin_to(&second, TEST_NOW_MS)
            .expect_err("a LIVE pin must not be re-pinned elsewhere");
        assert!(
            matches!(
                error,
                LoopStateRefusal::PinnedElsewhere {
                    until_ms,
                    ..
                } if until_ms == TEST_NOW_MS + PIN_TTL_MS
            ),
            "the refusal must name when the pin lapses; got {error}"
        );
        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: first,
                pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
            },
            "and the refusal left the owner and the expiry where they were"
        );
    }

    // ========================================================================
    // The pin's time bound
    // ========================================================================

    #[test]
    fn a_pin_stops_binding_the_moment_it_lapses_and_not_a_millisecond_later() {
        // THE EDGE, stated rather than left to the reader: expiry is `<=`, so a
        // pin whose `pinned_until_ms` is exactly now is OVER. That matches
        // `Lease::is_expired_at` and `deadline_passed` beside it — one spelling
        // of "expired" in this subsystem — and it is the direction that cannot
        // leave a lapsed pin binding for an extra tick.
        let holder = WorkerId::new("worker-a");
        let other = WorkerId::new("worker-b");
        let mut state = cdp_only_state();
        let until = TEST_NOW_MS + PIN_TTL_MS;
        state.placement = Placement::Pinned {
            worker: holder.clone(),
            pinned_until_ms: until,
        };

        assert!(
            !state.claimable_by(&other, until - 1),
            "one millisecond before the expiry the pin still binds"
        );
        assert!(
            state.claimable_by(&other, until),
            "AT the expiry the pin is over; `<` here instead of `<=` would hold \
             a stranded run for one more tick and, more to the point, would be a \
             second spelling of expiry in a file that already has one"
        );
        assert!(
            state.claimable_by(&other, until + 1),
            "and after it, plainly"
        );

        // The holder is admitted throughout, or the assertions above would be
        // satisfied by a placement that binds nobody.
        for now in [until - 1, until, until + 1] {
            assert!(
                state.claimable_by(&holder, now),
                "the worker holding the secret is always admitted, at {now}"
            );
        }
    }

    #[test]
    fn a_lapsed_pin_lets_another_worker_take_the_run_over() {
        // The whole point of the bound. Before the expiry the refusal stands and
        // names when it lifts; after it, the run is takeable — otherwise a run
        // pinned to a worker that died is exactly as stranded as it was under an
        // unbounded pin, and the bound would be decoration.
        let dead = WorkerId::new("worker-a");
        let live = WorkerId::new("worker-b");
        let mut state = cdp_only_state();
        state
            .pin_to(&dead, TEST_NOW_MS)
            .expect("the first worker pins it");
        let until = TEST_NOW_MS + PIN_TTL_MS;

        let error = state
            .pin_to(&live, until - 1)
            .expect_err("a live pin is not takeable");
        assert!(
            matches!(error, LoopStateRefusal::PinnedElsewhere { .. }),
            "got {error}"
        );

        state
            .pin_to(&live, until)
            .expect("a lapsed pin IS takeable, at the same edge `claimable_by` uses");
        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: live.clone(),
                pinned_until_ms: until + PIN_TTL_MS,
            },
            "and the takeover starts a fresh TTL of its own"
        );
        assert!(
            !state.claimable_by(&dead, until),
            "the previous holder has no standing once it has been taken over"
        );
    }

    #[test]
    fn the_holder_re_pinning_renews_rather_than_leaving_the_expiry_where_it_was() {
        // Why `PIN_TTL_MS` only has to cover the gap between one worker's
        // commits rather than the length of a run. If a re-pin returned the
        // placement unchanged — which is what "re-pinning is a no-op" used to
        // say — a run longer than the TTL would unpin itself underneath a worker
        // that was alive, holding the secret, and committing the whole time.
        let holder = WorkerId::new("worker-a");
        let mut state = cdp_only_state();
        state.pin_to(&holder, TEST_NOW_MS).expect("pinned");

        let later = TEST_NOW_MS + PIN_TTL_MS - 1;
        state.pin_to(&holder, later).expect("renewed");
        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: holder.clone(),
                pinned_until_ms: later + PIN_TTL_MS,
            },
            "a re-pin by the holder pushes the expiry out"
        );

        let other = WorkerId::new("worker-b");
        assert!(
            !state.claimable_by(&other, TEST_NOW_MS + PIN_TTL_MS),
            "and the renewal is what keeps the run bound past the ORIGINAL \
             expiry, which is the property the renewal exists for"
        );
    }

    #[test]
    fn every_commit_renews_the_pin_so_a_live_run_cannot_be_taken_from_under_it() {
        // The renewal rule as `for_commit` applies it, since `for_commit` is the
        // only caller in production: EVERY commit renews, whether or not the run
        // still holds a user-typed ephemeral secret.
        let ctx = ctx_with_open_segment(std::time::Duration::from_secs(1));
        let holder = WorkerId::new("worker-a");
        let mut state = cdp_only_state();

        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: TEST_NOW_MS,
                holds_user_typed_ephemeral_secret: true,
            })
            .expect("the acquiring commit pins");

        let later = TEST_NOW_MS + 60_000;
        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: later,
                holds_user_typed_ephemeral_secret: true,
            })
            .expect("a later commit that still holds it renews");
        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: holder.clone(),
                pinned_until_ms: later + PIN_TTL_MS,
            },
        );

        // ── The commit that no longer holds the secret ──────────────────────
        //
        // This assertion used to read the other way. It required the expiry NOT
        // to move once the secret was gone, on the stated grounds that "a pin
        // that kept renewing after the scope was cleared would be the unbounded
        // pin again, wearing a timestamp".
        //
        // That reasoning is true of renewal on a TIMER and false of renewal on a
        // COMMIT, and the difference is the whole of it. A commit is not a clock
        // tick: a run that stops committing stops renewing, so the pin still
        // lapses within one `PIN_TTL_MS` of the LAST commit and is still bounded.
        // The old rule therefore did not buy a bound on a dead run — the expiry
        // already was one — it bought a LAPSE UNDER A LIVE ONE. An ordinary run
        // holds no ephemeral secret, so nothing renewed the pin it was given at
        // creation, and one TTL in it went claimable while still going.
        //
        // What this gives up is migratability, and it is given up deliberately:
        // a live run stays with its driver and does not move mid-flight. Refused
        // alongside it, recorded here so they are not re-derived — a cooperative
        // handoff the current driver agrees to (YAGNI: nothing needs to move a
        // live run), and two TTLs, a short one without a secret and a long one
        // with (it satisfies both properties and needs a constant nobody has a
        // reason for). See
        // `docs/archive/plans/2026-08-28-stateless-loop-open-questions-design.md` §1.
        let later_still = later + 60_000;
        state
            .for_commit(CommitPoint {
                ctx: &ctx,
                worker: &holder,
                now_ms: later_still,
                holds_user_typed_ephemeral_secret: false,
            })
            .expect("a commit that no longer holds it must still succeed");
        assert_eq!(
            state.placement,
            Placement::Pinned {
                worker: holder.clone(),
                pinned_until_ms: later_still + PIN_TTL_MS,
            },
            "a commit renews the pin whether or not a secret is held: the pin is \
             the liveness marker, and a live run that could not renew would be \
             offered to a scan between two phases and taken from under itself"
        );

        let other = WorkerId::new("worker-b");
        assert!(
            !state.claimable_by(&other, later_still + PIN_TTL_MS - 1),
            "a live run is not claimable by anyone else while its pin holds"
        );
        assert!(
            state.claimable_by(&other, later_still + PIN_TTL_MS + 1),
            "and one TTL after its LAST commit it is claimable again, which is \
             what keeps a crashed run recoverable"
        );
    }

    #[test]
    fn a_pin_that_lost_its_expiry_on_the_wire_refuses_to_load() {
        // One level down from `placement_cannot_go_missing_from_the_wire`, and
        // the same argument: zero is a value every real clock is already past,
        // so a dropped `pinned_until_ms` reads as LAPSED and hands a run holding
        // a live credential to every worker. Asserted by deleting the key from a
        // real encoding, because the attribute is not the property.
        let mut state = cdp_only_state();
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-a"),
            pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
        };
        let encoded = serde_json::to_string(&state).expect("serialize");

        let mut value: serde_json::Value =
            serde_json::from_str(&encoded).expect("re-read as a document");
        assert!(
            value
                .get_mut("placement")
                .and_then(|placement| placement.as_object_mut())
                .expect("placement encodes as an object")
                .remove("pinned_until_ms")
                .is_some(),
            "pinned_until_ms must be WRITTEN, or a reader has nothing to miss"
        );
        assert!(
            serde_json::from_str::<LoopState>(&value.to_string()).is_err(),
            "a pin without its expiry must refuse to load; loading it as zero \
             would read as already lapsed and offer the run to everyone"
        );

        // And the intact pin round-trips, or the assertion above would be
        // satisfied by a variant nothing can parse.
        assert_eq!(
            serde_json::from_str::<LoopState>(&encoded).expect("the intact pin loads"),
            state
        );
    }

    /// The names that must never appear in a boundary record, and why.
    ///
    /// Three shapes wear the `RUN` label in the field audit and only one of them
    /// belongs on the wire. This is the list of the other two plus the grants,
    /// held in one place so the test below can be read as a statement rather
    /// than as a pile of assertions.
    const NAMES_THAT_MUST_NOT_BE_PERSISTED: [(&str, &str); 6] = [
        (
            "session_file_sandbox_roots",
            "a grant: forging one hands a run filesystem access nobody approved",
        ),
        (
            "session_tool_allowlist",
            "a grant: forging one skips an authorization prompt the user never saw",
        ),
        (
            "app_labeled_tool_results",
            "server-labeled bytes the owning loop drains before any sink sees them",
        ),
        (
            "shell_stream_ctx",
            "an in-flight handoff; restoring it revives a handoff nobody is taking",
        ),
        (
            "continuation_section_fingerprints",
            "runtime-only: a cold resume must re-send one bounded snapshot rather              than skip sections against a checkpoint the far side never established",
        ),
        (
            "steer_queue",
            "an inbound channel; the Arc is a delivery mechanism a worker cannot be handed",
        ),
    ];

    #[test]
    fn nothing_on_the_forbidden_list_reaches_the_wire() {
        // `RunGrants` is guarded by the compiler (see `grants.rs`), which is the
        // stronger check and the reason it is not repeated here. This covers the
        // rest, where nothing structural stops a later edit from adding a field:
        // the in-flight handoffs and the inbound channels. Their failure mode is
        // silent — labeled bytes in a sink, or a restored handoff — so the guard
        // has to be a test rather than a comment.
        let mut state = state();
        state.cursor = LoopCursor {
            iteration: 9,
            phase: Phase::Apply,
        };
        state.journal_seq = 44;
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-a"),
            pinned_until_ms: TEST_NOW_MS + PIN_TTL_MS,
        };
        state.wait = WaitReason::children(vec!["child-1".to_string()]);
        state.runnable_at_ms = 5;
        state.deadline_at_ms = Some(6);
        state.work_budget_consumed_ms = 7;
        state.phase_attempts = 2;
        state.pending = Some(PendingBatch {
            iteration: 9,
            phase: Phase::Apply,
            mode: BatchMode::Parallel { admitted: 1 },
            effects: vec![PendingEffect {
                effect_id: EffectId::parse("llm-1:tool:call-1").expect("well-formed"),
                tool: "browser__click".to_string(),
                arguments_fingerprint: "fp".to_string(),
                retry_safety: RetrySafety::NotRetrySafe,
                // A click reaches nobody, so it is not outward and has no act to
                // reconcile against. `None` is the semantically correct value
                // here and the outward case is exercised by the round-trip test
                // below, which carries a `Some`.
                reconcile_ref: None,
                // Nor is a click reattachable. The round-trip below is where
                // this field's wire shape is exercised.
                reattach_ref: None,
            }],
        });
        let encoded = serde_json::to_string(&state).expect("serialize");
        for (name, why) in NAMES_THAT_MUST_NOT_BE_PERSISTED {
            assert!(
                !encoded.contains(name),
                "`{name}` reached the boundary record — {why}\n{encoded}"
            );
        }
        // And the fixture is not vacuously empty: if the state above ever stops
        // populating its fields, the loop over the forbidden names would pass on
        // an empty document and prove nothing.
        assert!(encoded.contains("journal_seq"), "{encoded}");
        assert!(encoded.contains("pending"), "{encoded}");
        // And it round-trips, which is what stops this fixture drifting below a
        // bound again: a value the store would refuse now fails here, in the
        // test that carries it, rather than in whatever later test first adds a
        // parse.
        assert_eq!(
            serde_json::from_str::<LoopState>(&encoded).expect("the fixture must be readable"),
            state
        );
    }

    #[test]
    fn a_committed_state_survives_the_wire_with_its_pending_batch() {
        let mut state = state();
        state.cursor = LoopCursor {
            iteration: 4,
            phase: Phase::Apply,
        };
        state.journal_seq = 21;
        state.work_budget_consumed_ms = 9_000;
        state.iteration_checkpoint = Some(IterationCheckpoint {
            iteration: 4,
            history_iterations_len_at_start: 19,
            started_at_ms: 1_700_000_000_000,
        });
        state.pending = Some(PendingBatch {
            iteration: 4,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects: vec![
                PendingEffect {
                    effect_id: EffectId::parse("llm-1:tool:call-1").expect("well-formed"),
                    // An OUTWARD dispatch, so it has an act to reconcile against.
                    // Deliberately not the `None` the fixture above carries: a pair
                    // of fixtures that both said `None` would stay green if the
                    // field were deleted, skipped or never written, since serde
                    // defaults an absent `Option` to `None` anyway.
                    tool: "gmail__send".to_string(),
                    arguments_fingerprint: "fp".to_string(),
                    retry_safety: RetrySafety::NotRetrySafe,
                    reconcile_ref: Some(
                        CommittedActRef::new(
                            format!("act-{}", "0123456789abcdef".repeat(2)),
                            "anonymous",
                            "default",
                        )
                        .expect("the fixture ref must be the shape derive_act_ref mints"),
                    ),
                    reattach_ref: None,
                },
                // A SECOND member, and a reattachable one, for the same reason the
                // first carries a `Some` act ref: a batch whose every member said
                // `reattach_ref: None` would round-trip green with the field
                // deleted, skipped or never written. This is the only shape that
                // carries one — the gate mints it for a coding job and for nothing
                // else — so it has to be in the fixture rather than assumed.
                PendingEffect {
                    effect_id: EffectId::parse("llm-1:tool:call-2").expect("well-formed"),
                    tool: "run_coding_task".to_string(),
                    arguments_fingerprint: "fp-2".to_string(),
                    retry_safety: RetrySafety::Reattachable,
                    // A coding job reaches no third party, so it is not outward.
                    reconcile_ref: None,
                    reattach_ref: Some("cinv-000102030405060708090a0b".to_string()),
                },
            ],
        });

        let encoded = serde_json::to_string(&state).expect("serialize");
        let restored: LoopState = serde_json::from_str(&encoded).expect("read back");
        assert_eq!(restored, state);
        assert_eq!(
            restored
                .pending
                .as_ref()
                .map(|batch| batch.effects[0].retry_safety),
            Some(RetrySafety::NotRetrySafe)
        );
        let restored_ref = restored
            .pending
            .as_ref()
            .and_then(|batch| batch.effects[0].reconcile_ref.as_ref())
            .expect(
                "an outward effect that loses its act ref on the wire reads as `not outward`, \
                 and a worker that reconciles it finds no record — which is a licence to \
                 re-send a live message",
            );
        assert_eq!(
            restored_ref.act_ref_in_scope("anonymous", "default"),
            Some(format!("act-{}", "0123456789abcdef".repeat(2))).as_deref()
        );
        // The scope halves are asserted by name. A ref that arrived back without
        // them would still satisfy the line above if the accessor stopped
        // comparing — and a ref that cannot name its own scope is read under
        // whatever scope the pickup happens to run in, which is the same silent
        // false `DidNotFire`.
        assert_eq!(restored_ref.principal(), "anonymous");
        assert_eq!(restored_ref.workspace(), "default");
        assert_eq!(
            restored_ref.act_ref_in_scope("someone-else", "default"),
            None
        );
        // The reattachable member's ref, by VALUE. A batch that came back with
        // the field defaulted to `None` reads as an effect that names no job,
        // which `driver_worker::resolve_effects` holds as indeterminate — a run
        // that stalls at a resume, with nothing on the wire saying why.
        assert_eq!(
            restored
                .pending
                .as_ref()
                .and_then(|batch| batch.effects[1].reattach_ref.as_deref()),
            Some("cinv-000102030405060708090a0b")
        );
    }

    #[test]
    fn only_the_iteration_boundary_or_a_recovery_entry_admits_a_cold_holder() {
        // The whole of `ForeignPickup`'s content, asserted over EVERY phase
        // rather than over the one the fixture happens to sit at. A table read
        // through the type under test would agree with itself; iterating
        // `Phase::ORDER` is what makes a phase added later have to be placed.
        //
        // "Exactly one" is the load-bearing half. A change that emptied the
        // table would make every cursor look enterable and every cold pickup a
        // silently re-decided turn, and a test asserting only `Decide` is
        // refused would stay green through it.
        let mut admissible = Vec::new();
        let mut recoverable = Vec::new();
        for phase in super::super::outcome::Phase::ORDER {
            let mut state = LoopState::new(RunIdentity::default());
            state.cursor = LoopCursor {
                iteration: 4,
                phase,
            };
            match state.foreign_pickup() {
                ForeignPickup::AtIterationBoundary => admissible.push(phase),
                ForeignPickup::RecoverableMidIteration { phase } => recoverable.push(phase),
                // These two are WEAK and are worth exactly what they check,
                // which is said here so they are not read as coverage they do
                // not give. `foreign_pickup` is the sole constructor of this
                // variant, it fills `phase` from the cursor, and it reaches this
                // branch only when `needs` is non-empty — so neither assertion
                // can fail against the shape of the code as written. What each
                // catches is a one-token mutation of that constructor: a `phase`
                // filled from anything but the cursor, and an inverted
                // emptiness test, which would put `Prepare` here with nothing to
                // name. The load-bearing assertion is `admissible` below.
                ForeignPickup::MidIteration {
                    phase: named,
                    needs,
                } => {
                    assert_eq!(
                        named, phase,
                        "the refusal must name the cursor's own phase; an operator reading it \
                         has nothing else to go on"
                    );
                    assert!(
                        !needs.is_empty(),
                        "a mid-iteration refusal with an empty `needs` says a phase cannot be \
                         entered and declines to say what for"
                    );
                },
            }
        }
        assert_eq!(
            admissible,
            vec![super::super::outcome::Phase::Prepare],
            "the iteration boundary is the ONLY admissible cold entry. `Epilogue` reads nothing \
             from `IterationCarry` and is still refused, because the host measures its two \
             inputs at iteration entry — a cold epilogue runs the stuck detector against another \
             process's clock and a history length of zero"
        );
        assert_eq!(
            recoverable,
            vec![
                super::super::outcome::Phase::Resolve,
                super::super::outcome::Phase::Apply,
            ],
            "Resolve and Apply are recovery entries even without a capsule: the driver journals \
             them back to a fresh Observe before phase code or effects run"
        );

        let mut reconstruction = LoopState::new(RunIdentity::default());
        reconstruction.cold_reobserve = true;
        for phase in [
            super::super::outcome::Phase::Observe,
            super::super::outcome::Phase::Decide,
        ] {
            reconstruction.cursor.phase = phase;
            assert!(matches!(
                reconstruction.foreign_pickup(),
                ForeignPickup::RecoverableMidIteration { phase: recovered } if recovered == phase
            ));
        }

        reconstruction.cold_reobserve = false;
        reconstruction.cursor.phase = super::super::outcome::Phase::Epilogue;
        reconstruction.iteration_checkpoint = Some(IterationCheckpoint {
            iteration: reconstruction.cursor.iteration,
            history_iterations_len_at_start: 7,
            started_at_ms: 1_000,
        });
        assert!(matches!(
            reconstruction.foreign_pickup(),
            ForeignPickup::RecoverableMidIteration {
                phase: super::super::outcome::Phase::Epilogue
            }
        ));

        reconstruction
            .iteration_checkpoint
            .as_mut()
            .expect("checkpoint")
            .iteration += 1;
        assert!(matches!(
            reconstruction.foreign_pickup(),
            ForeignPickup::MidIteration {
                phase: super::super::outcome::Phase::Epilogue,
                ..
            }
        ));
    }

    #[test]
    fn a_park_is_not_answered_by_the_cursor_check() {
        // The obligation `foreign_pickup`'s documentation places on its caller,
        // as a test, because it is the one way this guard turns into a hang: a
        // parked run's pickup is a park EXIT, which runs no phase and reads none
        // of these values. This function cannot see the park — that is
        // deliberate — so what is asserted here is that the park is still
        // visible on the state beside it, which is what a caller gates on.
        let mut state = LoopState::new(RunIdentity::default());
        state.cursor = LoopCursor {
            iteration: 2,
            phase: super::super::outcome::Phase::Apply,
        };
        state.wait = Some(WaitReason::Children {
            child_execution_ids: vec!["child-1".to_string()],
            resume: ResumeAddress::unstated(),
        });

        assert!(
            matches!(
                state.foreign_pickup(),
                ForeignPickup::RecoverableMidIteration { phase }
                    if phase == super::super::outcome::Phase::Apply
            ),
            "the cursor answer must NOT change because a wait is present; folding the park in \
             here would answer `AtIterationBoundary`, which erases the required recovery \
             transition rather than saying that the park-exit question does not arise"
        );
        assert!(
            state.wait.is_some(),
            "the park a caller gates on must be readable beside the cursor answer"
        );
    }
}
