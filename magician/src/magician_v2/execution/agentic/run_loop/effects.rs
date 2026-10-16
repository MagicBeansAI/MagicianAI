//! Which effects this run has in flight, and what a worker may do about them.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Effect discipline*.
//!
//! # What this is NOT
//!
//! It is **not a second record of what happened to an outward act**. That record
//! exists — `OutwardAssertionStore` writes the disclosure before anything leaves
//! and `reconcile_outward_effect` reads it back — and the turn-boundary
//! contract's own non-goal is *"no new effect ledger"*. Duplicating it would
//! create two answers to "did this send happen" that have to be kept in step,
//! and the failure mode of them disagreeing is a re-sent live message.
//!
//! What is missing, and what this module supplies, is the **loop-side view**:
//! which effects *this run* committed an intent for, at which phase, and with
//! what retry safety — so a worker picking the execution up in `Apply` knows
//! what it was in the middle of before it asks anybody whether it landed.
//! [`EffectDisposition::Reconcile`] is where this module hands the question to
//! the record that already answers it.
//!
//! # Identity is not minted here either
//!
//! `effect_id = "{llm_call_id}:tool:{model_tool_call_id}"` is composed by
//! `LlmToolLineageIdentity::tool_execution_id_for`, which is the single composer
//! for the whole runtime, and is threaded through `PrimitiveExecCtx` to every
//! dispatch arm. [`EffectId`] wraps that string and validates its shape; it does
//! not invent a second one. See `docs/components/magician/effect-identity.md`.
//!
//! # The guarantee, stated precisely
//!
//! **At-most-once plus explicit reconciliation** — not exactly-once. A
//! non-idempotent external effect cannot be made exactly-once without
//! cooperation from the far side. What this buys is that the loop never
//! *silently* double-fires: every indeterminate case is surfaced rather than
//! guessed, and `retry_safety` defaults to not-safe so an undeclared capability
//! fails closed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::outcome::Phase;
use crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity;
use crate::magician_v2::execution::agentic::outward_settle::EffectReconciliation;

/// The separator that makes an effect id an effect id.
const TOOL_SEPARATOR: &str = ":tool:";

/// The longest an effect id may be.
///
/// Both halves are runtime-minted — an `llm_call_id` and a provider's tool-call
/// id — so a legitimate value is well under this. The bound exists because the
/// id is written into a ledger row and a journal line, and an unbounded string
/// arriving from a provider response is exactly the shape that turns a bounded
/// read into an unbounded one.
pub const MAX_EFFECT_ID_BYTES: usize = 512;

/// The most effects one dispatch batch may carry.
///
/// A batch is one primary candidate plus its admitted follow-ups. The real
/// number is single digits; this is a ceiling on a **deserialized** batch, so a
/// ledger row that grew somewhere else cannot make a worker fan out without
/// limit. Enforced on read as well as on construction — see
/// [`PendingBatch::validate`].
pub const MAX_BATCH_EFFECTS: usize = 256;

/// An effect id, checked.
///
/// # Why a newtype over a `String`
///
/// Because `None` and `""` mean different things and the difference is a
/// security property. The effect-identity contract is explicit: `None` means
/// **not attributable**, never *"safe to repeat"*. A blank
/// `model_tool_call_id` would compose to `{llm_call}:tool:` for *every*
/// unidentified call in a turn, so two distinct requests would present one key —
/// and a remote honouring it answers the second from the first's cached
/// response, losing a real effect with no error raised.
///
/// Every constructor here refuses that. A caller that does not have both halves
/// gets no id, and must carry `None` — which every reader treats as
/// unattributable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EffectId(String);

impl EffectId {
    /// Compose from the two halves, refusing either being blank.
    ///
    /// Delegates the format to `LlmToolLineageIdentity::tool_execution_id_for`
    /// so there is still exactly one composer in the runtime. This adds the
    /// caller-side check that composer's own documentation says is the caller's
    /// job.
    pub fn from_parts(llm_call_id: &str, model_tool_call_id: &str) -> Result<Self, EffectIdError> {
        if llm_call_id.trim().is_empty() {
            return Err(EffectIdError::EmptyHalf {
                half: "llm_call_id",
            });
        }
        if model_tool_call_id.trim().is_empty() {
            return Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id",
            });
        }
        Self::parse(&LlmToolLineageIdentity::tool_execution_id_for(
            llm_call_id,
            model_tool_call_id,
        ))
    }

    /// Take an id back off the wire.
    ///
    /// Accepts the sub-step form `{effect_id}:step:{step_id}` too, because a
    /// workflow replay legitimately fans one model tool call out into several
    /// requests and each gets its own id. The split is on the **first**
    /// `:tool:`, so a step id containing the separator cannot re-anchor the
    /// parse.
    pub fn parse(raw: &str) -> Result<Self, EffectIdError> {
        if raw.len() > MAX_EFFECT_ID_BYTES {
            return Err(EffectIdError::TooLong { bytes: raw.len() });
        }
        if raw.chars().any(|ch| ch.is_control()) {
            return Err(EffectIdError::ControlCharacter);
        }
        let Some((llm_call_id, tail)) = raw.split_once(TOOL_SEPARATOR) else {
            return Err(EffectIdError::MissingSeparator);
        };
        if llm_call_id.trim().is_empty() {
            return Err(EffectIdError::EmptyHalf {
                half: "llm_call_id",
            });
        }
        // A blank call id is refused whether it is blank to the end of the
        // string or blank up to a `:step:` suffix. The second form reaches here
        // only off the wire, and it carries the same defect: the segment that is
        // supposed to name which of the turn's tool calls this is names nothing.
        if tail.trim().is_empty() || tail.starts_with(':') {
            return Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id",
            });
        }
        Ok(Self(raw.to_string()))
    }

    /// The sub-id for one step of a dispatch that fans out.
    ///
    /// One key for a whole replay would identify nothing, and a remote honouring
    /// it would collapse the fan-out into a single request.
    ///
    /// Unlike [`Self::from_parts`], this does **not** delegate: the `:step:`
    /// shape is composed here and also, inline, by
    /// `api_mining::workflow_replay::step_executor`. Two spellings of one format
    /// is one more than there should be — it belongs beside
    /// `tool_execution_id_for` with the other composer — and until it moves, a
    /// change to either has to move both.
    pub fn sub_step(&self, step_id: &str) -> Result<Self, EffectIdError> {
        if step_id.trim().is_empty() {
            return Err(EffectIdError::EmptyHalf { half: "step_id" });
        }
        Self::parse(&format!("{}:step:{step_id}", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The address this attempt's records are filed under.
    ///
    /// A one-way digest rather than the raw id, and deliberately **not** the
    /// far-side key: that value travels to arbitrary remotes, and one that also
    /// indexed our own records would let a remote holding it recognise our
    /// internal trail. The two derivations are domain-separated in
    /// `LlmToolLineageIdentity`, which is where they stay.
    ///
    /// It is also what makes an effect id safe as a filename: the raw id embeds
    /// a provider's tool-call id, which is neither length-bounded in a way a
    /// filesystem cares about nor free of separators.
    pub fn local_attempt_key(&self) -> String {
        LlmToolLineageIdentity::local_attempt_key(&self.0)
    }
}

impl fmt::Display for EffectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for EffectId {
    type Error = EffectIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<EffectId> for String {
    fn from(value: EffectId) -> Self {
        value.0
    }
}

/// Why a string is not an effect id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectIdError {
    EmptyHalf { half: &'static str },
    MissingSeparator,
    TooLong { bytes: usize },
    ControlCharacter,
}

impl fmt::Display for EffectIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectIdError::EmptyHalf { half } => write!(
                f,
                "an effect id needs a non-empty {half}; a blank one would collide with every \
                 other unidentified call in the turn"
            ),
            EffectIdError::MissingSeparator => {
                write!(f, "an effect id must contain '{TOOL_SEPARATOR}'")
            },
            EffectIdError::TooLong { bytes } => {
                write!(f, "an effect id of {bytes} bytes is over the limit")
            },
            EffectIdError::ControlCharacter => {
                write!(f, "an effect id may not contain control characters")
            },
        }
    }
}

impl std::error::Error for EffectIdError {}

/// What may be done with an effect whose result is missing.
///
/// The design's third resolution mode — reattach — was added after the coding
/// flow was read: a coding run is *not* retry-safe, because it mutates a real
/// repository, but it *is* re-attachable by `native_session_id`. Collapsing it
/// into either of the other two would be wrong in both directions: re-firing
/// runs the job twice, and surfacing to the user abandons a resumable job.
///
/// # The default is not-safe, on purpose
///
/// `#[serde(default)]` on a ledger row resolves an absent field to
/// [`RetrySafety::NotRetrySafe`]. An older record, a hand edit, or a capability
/// that never declared its safety therefore fails closed. The opposite default
/// would make every unlabelled effect silently re-firable, which is the exact
/// failure this whole module exists to prevent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySafety {
    /// Re-running it costs nothing that matters — a read, a snapshot, a GET.
    RetrySafe,
    /// Not repeatable, but resumable by session identity. Coding jobs.
    Reattachable,
    /// Neither. Browser clicks, sends, writes.
    #[default]
    NotRetrySafe,
}

impl RetrySafety {
    /// The more conservative of two declarations.
    ///
    /// Ordered by what each one forbids rather than by anything alphabetical:
    /// `RetrySafe` forbids nothing, `Reattachable` forbids re-firing, and
    /// `NotRetrySafe` forbids assuming a resumable session as well. A reader
    /// that needs to reconcile two sources about one effect wants the one that
    /// forbids more.
    pub const fn stricter(one: Self, other: Self) -> Self {
        match (one, other) {
            (RetrySafety::NotRetrySafe, _) | (_, RetrySafety::NotRetrySafe) => {
                RetrySafety::NotRetrySafe
            },
            (RetrySafety::Reattachable, _) | (_, RetrySafety::Reattachable) => {
                RetrySafety::Reattachable
            },
            (RetrySafety::RetrySafe, RetrySafety::RetrySafe) => RetrySafety::RetrySafe,
        }
    }
}

/// How the members of a batch relate to each other.
///
/// One iteration dispatches a **batch**, not an action: the primary candidate,
/// then a parallel slice of leading follow-ups admitted only when the primary is
/// a parallelizable read-only action, then the remaining follow-ups in sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BatchMode {
    /// `admitted` members ran together and settle **out of order**, which is why
    /// resume needs a per-effect status map rather than a cursor.
    ///
    /// One constraint bounds the resume problem sharply and is worth keeping in
    /// view: admission requires the follow-up to be pre-authorized and to be
    /// unable to raise a sandbox HITL, so **a parallel member can never pause**.
    /// Only the sequential tail can.
    Parallel {
        admitted: usize,
    },
    Sequential,
}

/// One effect the run committed to before firing it.
///
/// # The arguments are deliberately absent
///
/// The design sketches `PendingEffect { effect_id, tool, args, retry_safety }`.
/// The `args` are not here, and the omission is a decision rather than an
/// oversight: a durable ledger row that carried raw tool arguments would carry
/// secret values to disk, which is precisely what the pause store already
/// refuses to do (`persisted_pending_inputs` strips them, with the note that
/// "secret values stay in the in-memory secret scope and must not be written to
/// pause files").
///
/// # Where a re-firing worker gets them instead
///
/// An earlier version of this comment left that open and offered the decision
/// record as one candidate. The candidate is wrong, and it is worth naming
/// rather than deleting, because it is the first place an implementer looks:
/// `DecisionRecord` is a **summary**. What `persist_decision_record_if_available`
/// writes is the reasoning, a 100-character `{:?}` prefix of the action, and a
/// truncated result preview. The arguments are not in it and never were.
///
/// They are in the conversation, on the assistant turn the effect id is minted
/// from — `effect_id_for_candidate` composes the id from
/// `history.assistant_turns.last()` and the candidate's tool-call id, so the
/// turn that carries the id carries the arguments too. A re-firing worker
/// **re-derives** them from that turn, and the re-derivation is checked:
/// [`EffectLedger::authorize_refire`] refuses one that does not reproduce the
/// committed `arguments_fingerprint`. Re-deriving is safe only because that
/// check makes a bad re-derivation loud.
///
/// See `docs/archive/plans/2026-08-26-stateless-driver-decisions.md`, Decision 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEffect {
    pub effect_id: EffectId,
    /// Capability or tool name, for operators reading the ledger.
    pub tool: String,
    /// The `arguments_fingerprint` the lineage identity already computes.
    ///
    /// # Empty means *no fingerprint was committed*, and it is not nothing
    ///
    /// The `Gate` computes this through `LlmToolLineageIdentity::new`, the same
    /// constructor `WorkerHost::rederive_dispatch` calls, so the committed
    /// value and the re-derived one are comparable by construction rather than
    /// by two functions happening to agree.
    ///
    /// It can still come out empty: a turn with no trace receipt, a candidate
    /// the model gave no tool-call id, or a scoped HMAC key that will not load.
    /// Those are the cases where there is genuinely nothing for a
    /// re-derivation to reproduce, and an empty fingerprint must never
    /// *compare equal to another empty one* —
    /// [`EffectLedger::authorize_refire`] refuses it outright, because two
    /// absences matching would license a re-fire on the strength of nothing
    /// having been recorded twice, which is the same shape as reading an absent
    /// act as *nothing was sent*.
    ///
    /// Recording the intent is still allowed: an unfingerprintable dispatch
    /// must be *recorded*, it just may never be *re-fired*.
    pub arguments_fingerprint: String,
    #[serde(default)]
    pub retry_safety: RetrySafety,
    /// The outward act ref this dispatch reconciles against, computed at `Gate`
    /// time while the resolved params are still in hand — alongside the class
    /// the outward classifier computes from those same params.
    ///
    /// A one-way digest (`derive_act_ref` over a blake3 payload ref), so
    /// committing it discloses nothing the row's `tool` field does not already
    /// disclose.
    ///
    /// # `None` means *no outward record names this dispatch*
    ///
    /// It must **never** be read as *nothing left*. The two answers look alike
    /// and license opposite things: a reader that conflates them answers
    /// [`ReconciledEffect::SafeToRefire`] for every effect that merely had no
    /// ref to carry, which re-sends live messages. An effect whose disposition
    /// is [`EffectDisposition::Reconcile`] and whose `reconcile_ref` is `None`
    /// has no record to ask, and no record to ask is
    /// [`ReconciledEffect::SurfaceToUser`].
    ///
    /// Exactly two writers produce it, and `DispatchReconcileRef::committed` is
    /// the only collapse that may:
    ///
    /// - **The dispatch is not outward** — reads, snapshots, clicks, everything
    ///   that cannot reach anybody. The common case by far.
    /// - **The dispatch is outward and this execution has no scoped store.** The
    ///   outward gate destructures the same three values before a send and
    ///   refuses when any is missing — `outward_gate::contact_refusal` for every
    ///   act that names recipients, and the dispatch-record block's *"NOT SENT"*
    ///   for the remainder — so nothing left in this state either. This is why
    ///   `DispatchReconcileRef::Unscoped` commits `None` instead of failing the
    ///   phase: a refusal at the `Gate` for a send the gate below already
    ///   refuses is a guard at the producer, and it would end every run that has
    ///   no principal.
    ///
    /// What may **never** commit `None` is the third shape:
    /// `DispatchReconcileRef::Undeterminable`, where the scope IS present, the
    /// send therefore proceeds, and the ref still could not be named. That one
    /// refuses the dispatch.
    ///
    /// # Loaded by, never re-derived — which is the entire point
    ///
    /// `outward_settle::reconcile_outward_effect` derives the act ref from
    /// `serde_json::to_vec(resolved_params)`, and that is sound only while the
    /// **same map instance** the gate hashed is in hand. `resolved_params` is a
    /// `HashMap` under the default `RandomState`, so two maps with identical
    /// contents hold different hash keys and serialise their entries in
    /// different orders — a re-derived ref is a *different* ref, it reads an
    /// absent record, and `load_act`'s `Ok(None)` arm answers
    /// `DidNotFire { Prepared }`: positive evidence that nothing left, which is
    /// a silent licence to re-send. Committing the ref deletes the
    /// re-derivation, and with it that failure —
    /// `outward_settle::reconcile_committed_effect` is the entry point that
    /// takes this value and derives nothing.
    ///
    /// Carried here rather than on [`EffectLedgerEntry`] because the case that
    /// needs it is precisely the one with no row: a worker that died between
    /// the intent commit and the fire, which [`EffectLedger::disposition`]
    /// documents as the reason ledger silence is not evidence.
    ///
    /// # `Some` is also evidence, in the other direction
    ///
    /// A ref is written only for a dispatch the outward classifier said can
    /// reach somebody, so carrying one says *this effect is a live send*. That
    /// outranks a [`RetrySafety::RetrySafe`] declaration, which would otherwise
    /// answer [`EffectDisposition::Refire`] and re-send with no record read at
    /// all — see [`EffectLedger::disposition`], which applies the rule.
    ///
    /// # Checked on the way in
    ///
    /// The ref becomes a **filesystem path component** —
    /// `OutwardAssertionStore::load_act` joins it into the scope root — and
    /// since it now arrives off the wire it is the first act ref in the runtime
    /// that was not minted in-process. [`CommittedActRef`] refuses anything
    /// that is not the derived shape, at parse and again in
    /// [`PendingBatch::validate`].
    ///
    /// # It carries its scope, and that is not decoration
    ///
    /// See [`CommittedActRef`]. A bare ref could not name the directory it
    /// lives under, and reading it under the wrong one produces the same
    /// silent false `DidNotFire` this whole field exists to delete.
    ///
    /// # This field CHANGED WIRE SHAPE, with no backward-compatible read
    ///
    /// It was `Option<String>` — a JSON string — and is now
    /// `Option<CommittedActRef>`, a JSON object. Nothing here accepts the old
    /// spelling: a record carrying `"reconcile_ref": "act-…"` is a hard parse
    /// error, `load_sync` reports it as `StoreError::Corrupt`, and the execution
    /// is quarantined rather than resumed.
    ///
    /// That is a decision, and it was taken against checked evidence rather than
    /// against the assumption that no committed data exists — an assumption
    /// already found false once on this branch about a different store. What was
    /// checked, on 2026-08-28:
    ///
    /// 1. `Option<String>` appears in exactly one commit, `99b120f4d4`
    ///    (2026-08-27), which is the only commit that has ever touched this
    ///    file. It is not on `master` and carries no tag.
    /// 2. **At that commit there is no non-test construction site of
    ///    [`PendingEffect`] anywhere in `magician/src`.** Every one is inside a
    ///    `#[cfg(test)]` module. The sole production constructor —
    ///    `phases::apply`'s gate — did not exist yet; `phases/` was there and
    ///    contained no reference to this type. So no build of that commit could
    ///    write a `PendingEffect` into a committed state at all, with or without
    ///    a ref.
    /// 3. The only persistence vector is [`super::state::LoopState::pending`],
    ///    which lands in `store/fs.rs`'s `snapshot-*.json`. No journal record
    ///    carries a `PendingEffect`, so a log cannot hold the old shape either.
    /// 4. `FsLoopStateStore::new` is genuinely reachable from production
    ///    (`StatelessArm::new`), but the driver is opt-in through
    ///    `MAGICIAN_EXECUTION_DRIVER=stateless` and no `executions/`,
    ///    `journal.jsonl` or `snapshot-*.json` exists anywhere on the machine
    ///    this was checked on — repo, every worktree, the SSD roots, the notes
    ///    root, `~/Library/Application Support`.
    ///
    /// Point 2 is the one that does not depend on where anybody looked: the
    /// shipped code could not produce the old shape, so there is nothing to
    /// migrate even if a snapshot turned up tomorrow.
    ///
    /// **If that stops being true** — if a `snapshot-*.json` predating
    /// `99b120f4d4`'s successor is ever found — the fix is an untagged enum or a
    /// hand-written `Deserialize` on this field accepting both spellings, with
    /// the bare string read as a **scope-less** ref that
    /// `outward_settle::reconcile_committed_effect` must refuse rather than read
    /// under the live scope. Reading it under the live scope is the exact silent
    /// false `DidNotFire` that [`CommittedActRef`] exists to delete, so a
    /// compatibility shim that quietly adopted the current scope would reopen
    /// the defect the type closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_ref: Option<CommittedActRef>,

    /// The job this dispatch reattaches to, named **before** it starts.
    ///
    /// Written by the gate for a [`RetrySafety::Reattachable`] dispatch and by
    /// nothing else — today that is `run_coding_task`, whose value is a coding
    /// invocation id minted by
    /// `coding_engine::ledger::coding_invocation_id_for_effect` from this
    /// effect's id. The same string is stamped into the dispatch's parameters,
    /// so the id in this row and the id in `coding_ledger.json` are one value
    /// carried rather than two derivations agreeing.
    ///
    /// # It is NOT the session, and the distinction is the whole design
    ///
    /// A session handle does not exist until the job has run, so an intent —
    /// which is contractually written before the fire — could never carry one.
    /// An invocation id exists before the job starts, which is why it is what
    /// this field holds. The session is looked up THROUGH it at recovery:
    /// `WorkerHost::reattach_state` reads the coding ledger and
    /// answers the `native_session_id` the job reported — since 2026-08-29 that
    /// includes a session reported *mid-turn*, so a worker killed while the turn
    /// was open resolves too. An entry with no session at all means the engine
    /// never named one, and that stays `Advanced::EffectIndeterminate`.
    ///
    /// `None` for every effect that is not reattachable, and for a reattachable
    /// one the gate could not key — a dispatch with no effect id mints no row at
    /// all, so there is no third state where a row exists and the ref is merely
    /// missing by accident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reattach_ref: Option<String>,
}

/// An act ref **together with the scope it was derived under**.
///
/// # Why the two cannot be separated
///
/// `derive_act_ref` folds `principal` and `workspace` into the digest, and the
/// act's log then lands under that scope's root. Both halves of the address are
/// therefore the scope's, and a ref on its own names nothing: it is half a
/// coordinate.
///
/// Before this type, [`PendingEffect::reconcile_ref`] was a bare `String` and
/// `outward_settle::reconcile_committed_effect` rebuilt the root from the live
/// `AgenticContext` at pickup. A worker resuming under a different principal or
/// workspace therefore read a directory the ref could never name, found
/// nothing, and got `EffectReconciliation::DidNotFire { Prepared }` — **positive
/// evidence that nothing was sent, which licenses a re-send.** Nothing
/// downstream could detect it, because *"this scope has no such act"* and
/// *"this is the wrong scope"* are the same absence. It is the same false
/// licence as re-deriving the ref, arriving through the scope instead.
///
/// Holding the three together means a caller cannot hold the ref without also
/// holding the answer to *which scope derived it*, so the mismatch is at last a
/// question something can ask. `outward_settle::reconcile_committed_effect`
/// asks it and **refuses** on disagreement rather than reading under either
/// scope; the reasoning for refusing over trusting is stated there.
///
/// # Private fields, checked constructor
///
/// `RunGrants` earns its "one writer" claim structurally by not deriving
/// `Serialize`; a `pub` field earns nothing, as `Placement` found out. These
/// are private and the only two ways in — [`Self::new`] and the hand-written
/// `Deserialize` below — both run [`validate_reconcile_ref`]. There is no
/// third.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommittedActRef {
    act_ref: String,
    principal: String,
    workspace: String,
}

/// The wire form, so the hand-written [`Deserialize`] has something to delegate
/// the field parse to.
///
/// All three are `String`, never `Option<String>`: serde answers a missing
/// `Option` field with `None` through `missing_field`'s option specialization
/// whether or not `#[serde(default)]` is present, so an optional scope half
/// would be a scope that could go missing from the wire — and a record that
/// lost its principal would resume addressing a *different* directory while
/// looking well-formed. A missing `String` field is a hard parse error, which
/// is the fail-closed direction.
#[derive(Deserialize)]
struct CommittedActRefWire {
    act_ref: String,
    principal: String,
    workspace: String,
}

impl CommittedActRef {
    /// Bind an act ref to the scope it was derived under.
    ///
    /// The scope halves are taken verbatim. They are **not** trimmed,
    /// lower-cased or otherwise normalised, because `derive_act_ref` folded
    /// these exact bytes into the digest: a normalisation here would produce a
    /// value that no longer matches the scope that minted the ref, which is the
    /// mismatch this type exists to make visible.
    pub fn new(
        act_ref: impl Into<String>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Result<Self, EffectError> {
        let act_ref = act_ref.into();
        validate_reconcile_ref(&act_ref)?;
        Ok(Self {
            act_ref,
            principal: principal.into(),
            workspace: workspace.into(),
        })
    }

    /// The addressable ref — **only** to a caller that can name the scope it
    /// was derived under.
    ///
    /// This is the whole enforcement, and it is a type rather than a rule: the
    /// string that addresses `outward_assertions/<principal>/<workspace>/acts/`
    /// is unobtainable without presenting the scope, so there is no way to
    /// spell the read that started this — load the record under whatever scope
    /// the worker happened to resume in, find nothing, and report
    /// `DidNotFire`. A `names_scope`-style predicate beside a plain accessor
    /// would have left that spelling available and made the protection a
    /// convention that the next caller has to remember.
    ///
    /// Exact string equality, deliberately. `derive_act_ref` folded these raw
    /// bytes into the digest, so two scopes differing by a byte derive two
    /// different refs — a caller whose scope differs by a byte is not holding
    /// the scope that minted this one, however alike the two look on a
    /// filesystem after `safe_segment` has normalised them. Comparing
    /// normalised directory names would call two scopes equal on the strength
    /// of the very lossiness that makes them collide.
    pub fn act_ref_in_scope(&self, principal: &str, workspace: &str) -> Option<&str> {
        (self.principal == principal && self.workspace == workspace).then(|| self.act_ref.as_str())
    }

    /// The scope halves, for the message that explains a refusal. Deliberately
    /// not paired with a bare ref accessor — see [`Self::act_ref_in_scope`].
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The same shape check the constructor and the deserializer run.
    ///
    /// Both ways in already run it, so on a value this process is holding this
    /// is a redundancy — kept because [`PendingBatch::validate`] runs on load as
    /// well as on construction, and a redundant check that costs a string scan
    /// is the cheapest possible insurance against a fourth way in being added
    /// without one.
    pub fn validate(&self) -> Result<(), EffectError> {
        validate_reconcile_ref(&self.act_ref)
    }
}

impl<'de> Deserialize<'de> for CommittedActRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let wire = CommittedActRefWire::deserialize(deserializer)?;
        CommittedActRef::new(wire.act_ref, wire.principal, wire.workspace).map_err(D::Error::custom)
    }
}

/// Renders as `act-…@principal/workspace`, so a log line says which scope a ref
/// belongs to rather than leaving a reader to assume it is theirs.
///
/// # This is a one-line bypass, and pretending otherwise would be worse
///
/// An earlier version of this comment claimed the rendering was not a way around
/// [`CommittedActRef::act_ref_in_scope`], on the grounds that it is not the
/// derived shape and `reconciliation_from_act`'s shape check would refuse it.
/// That is true of the WHOLE rendering and false of half of it: an act ref
/// contains no `@`, so `committed.to_string().split('@').next().unwrap()` yields
/// exactly the bare string the accessor exists to withhold, and
/// `is_derived_act_ref` accepts it.
///
/// So the enforcement is:
///
/// - **Structural** against the failure that actually happened — a caller who
///   simply has a `CommittedActRef` and needs a ref cannot get one without
///   naming a scope, so *"read it under whatever scope this worker resumed in"*
///   has no spelling.
/// - **Conventional** against a caller who goes out of their way to split a
///   rendered log string. Nothing in the type system stops that, and a
///   `Display` impl that withheld the ref would take the scope out of the log
///   lines it exists to put it in.
///
/// The distinction matters because the next reader budgets for whichever one
/// this comment claims. If a bare accessor is ever genuinely needed, add a named
/// one with its own justification rather than routing through here.
impl fmt::Display for CommittedActRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}/{}", self.act_ref, self.principal, self.workspace)
    }
}

/// The prefix every derived act ref carries.
///
/// **Aliased, not redeclared.** These two describe `derive_act_ref`'s output,
/// which is defined next to it in `evidence::outward_assertions`. Writing the
/// numbers again here would mean a widened `stable_id` left this module's error
/// message and test fixtures describing a shape nothing produces — while the
/// check itself, which now delegates, silently kept working. The error text
/// would then be actively misleading about why a ref was refused.
pub use crate::magician_v2::evidence::outward_assertions::ACT_REF_PREFIX as RECONCILE_REF_PREFIX;

/// How many hex characters follow that prefix. See [`RECONCILE_REF_PREFIX`].
pub use crate::magician_v2::evidence::outward_assertions::ACT_REF_DIGEST_HEX as RECONCILE_REF_DIGEST_HEX;

/// The longest prefix of a rejected ref an error may quote back.
///
/// The value being rejected is by definition one this runtime did not mint, so
/// its length is not something this process chose. An operator needs to see
/// enough to recognise it; a log line does not need to carry all of it.
const RECONCILE_REF_ECHO_LIMIT: usize = 64;

/// Refuse an act ref that is not the shape `derive_act_ref` produces.
///
/// Strict rather than sanitising, because there is no such thing as a *nearly*
/// correct act ref: it is a digest, so one that is not exactly the derived form
/// names a record that was never written. Loosening this to "reject separators"
/// would still admit a ref that reads a different act's disclosure, and
/// tightening it costs nothing — every legitimate value is minted by one
/// function.
pub fn validate_reconcile_ref(value: &str) -> Result<(), EffectError> {
    // The shape belongs to the function that MINTS it, not to this one. An
    // earlier cut restated the prefix and digest width here, which made the
    // safety property have two spellings — and a widened `stable_id` would then
    // have left this checker quietly refusing every legitimate ref. Delegating
    // means there is one definition and one drift guard
    // (`the_minted_shape_is_the_shape_the_checker_accepts`).
    if crate::magician_v2::evidence::outward_assertions::is_derived_act_ref(value) {
        return Ok(());
    }
    Err(EffectError::MalformedReconcileRef {
        offered: value.chars().take(RECONCILE_REF_ECHO_LIMIT).collect(),
    })
}

/// The whole batch an iteration committed before any of it fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBatch {
    pub iteration: usize,
    pub phase: Phase,
    pub mode: BatchMode,
    pub effects: Vec<PendingEffect>,
}

impl PendingBatch {
    /// Refuse a batch no worker should act on.
    ///
    /// Called on **construction and on load**. A bound checked only when a value
    /// is built is not a bound: the value that hurts is the one that arrived from
    /// disk.
    pub fn validate(&self) -> Result<(), EffectError> {
        if self.effects.is_empty() {
            return Err(EffectError::EmptyBatch);
        }
        if self.effects.len() > MAX_BATCH_EFFECTS {
            return Err(EffectError::BatchTooLarge {
                count: self.effects.len(),
                limit: MAX_BATCH_EFFECTS,
            });
        }
        if let BatchMode::Parallel { admitted } = self.mode {
            if admitted > self.effects.len() {
                return Err(EffectError::ParallelSliceOverruns {
                    admitted,
                    effects: self.effects.len(),
                });
            }
        }
        let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
        for effect in &self.effects {
            if seen.insert(effect.effect_id.as_str(), ()).is_some() {
                return Err(EffectError::DuplicateEffectId {
                    effect_id: effect.effect_id.to_string(),
                });
            }
            // The same check the deserializer runs, run again on a batch this
            // process built. One implementation, two call sites: the shape of an
            // act ref matters because it addresses a file, and a value that
            // never crossed a deserializer is no more trustworthy for it.
            if let Some(reconcile_ref) = &effect.reconcile_ref {
                reconcile_ref.validate()?;
            }
        }
        Ok(())
    }
}

/// What the runtime learned about an effect after it fired.
///
/// An **absent** outcome on a ledger row means *no result was recorded*. It does
/// **not** mean the effect did not happen, and no reader may treat it that way —
/// that reading is the one the effect-identity work exists to make impossible.
/// See [`EffectDisposition`] for what absence actually licenses.
///
/// # Three of these are about a dispatch that ran. One is about one that did not
///
/// [`Self::NotDispatched`] is the odd one, and the distinction it carries is the
/// whole reason it exists as its own variant rather than as a
/// [`Self::Failed`] with an explanatory reason. *"We tried and it failed"* and
/// *"we never tried"* recover **differently**: the first is indeterminate at the
/// transport, the second is the one case in this module where the loop can act
/// with certainty. Folding the second into the first throws that certainty away
/// and there is no way to get it back from the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EffectOutcome {
    /// The dispatch reported success.
    Succeeded { at_ms: i64 },
    /// The dispatch reported success and retained a bounded, provider-safe
    /// result projection that a restarted Apply can adopt into history.
    SucceededWithResult {
        at_ms: i64,
        result: serde_json::Value,
    },
    /// The dispatch reported failure, and the transport confirms nothing left.
    ///
    /// Distinct from [`EffectOutcome::Indeterminate`]: this is the case where we
    /// **know** it did not happen.
    Failed { at_ms: i64, reason: String },
    /// The dispatch reported failure and retained its bounded result projection.
    FailedWithResult {
        at_ms: i64,
        reason: String,
        result: serde_json::Value,
    },
    /// The transport ran and could not say whether the effect landed.
    ///
    /// A CDP disconnect mid-click, a POST that timed out. Recording this is the
    /// point: the alternative is an empty row, which reads identically to "we
    /// never got there".
    Indeterminate { at_ms: i64, reason: String },
    /// The gate admitted this effect and the dispatch never reached it.
    ///
    /// **The only outcome in this enum that proves an effect did NOT go out.**
    /// Every other answer — including an absent one — leaves open the
    /// possibility that something left, which is why [`EffectDisposition`] has
    /// no "assume nothing happened" arm. This variant is that assumption made
    /// *earned*: it is written by the half of `phases::apply` that decides which
    /// admitted members to fire, about a member it decided against, from inside
    /// the same function that would have fired it.
    ///
    /// It exists because the gate now admits the whole in-turn batch before any
    /// of it fires (see `phases::apply::gate`), so a batch that bails partway —
    /// a failed sibling, a confirmation-gated follow-up, the work budget — holds
    /// intents for members that were never attempted. Without this the ledger
    /// would report them as unsettled and a resume would treat a dispatch that
    /// demonstrably never happened as one that might have.
    ///
    /// `reason` is why the dispatch stopped short, not why anything failed.
    NotDispatched { at_ms: i64, reason: String },
}

/// One row of the loop-side effect ledger.
///
/// # This row is a superset of the [`PendingEffect`] it was minted from
///
/// Deliberately, and it is what makes the ledger the durable record rather than
/// an annotation on one. `driver_worker::resolve_effects` asks
/// [`EffectLedger::disposition`] about every unsettled row, and that question
/// needs the whole dispatch — including [`Self::reconcile_ref`], whose absence
/// is the difference between asking the outward record and surfacing an
/// indeterminate effect to a user. A row that carried only the identity would
/// send every non-retry-safe resume to `SurfaceToUser` for want of a field it
/// once had, which is a false stall rather than a false fire, but is still a
/// resume the run cannot make.
///
/// [`Self::pending`] is the reconstruction, and it is total: every field
/// `PendingEffect` has is on this row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectLedgerEntry {
    pub effect_id: EffectId,
    pub iteration: usize,
    pub phase: Phase,
    pub tool: String,
    pub arguments_fingerprint: String,
    #[serde(default)]
    pub retry_safety: RetrySafety,
    /// When the intent was committed — before the effect fired, always.
    pub intent_at_ms: i64,
    /// New-protocol intent rows are only *prepared* until the driver publishes
    /// their complete [`PendingBatch`] in `LoopState.pending` with a fenced CAS.
    /// This closes the sequential multi-row window: if the third intent write
    /// fails, the first two rows are not mistaken for effects that may have
    /// fired. Missing on legacy rows means active/conservative, preserving the
    /// only safe interpretation of intents written before this marker existed.
    #[serde(default)]
    pub prepared_only: bool,
    /// Absent means **no result recorded**, never "did not fire".
    ///
    /// The one outcome that *does* mean "did not fire" says so by name:
    /// [`EffectOutcome::NotDispatched`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<EffectOutcome>,
    /// What a [`RetrySafety::Reattachable`] effect reattaches THROUGH.
    ///
    /// Copied from [`PendingEffect::reattach_ref`] at intent time, which is what
    /// makes it reachable at all: an intent is written before the fire, so a
    /// field that could only be filled in afterwards would be `None` in exactly
    /// the case recovery needs it — the worker that died mid-job.
    ///
    /// It is therefore a **coding invocation id**, not a session handle. See
    /// [`PendingEffect::reattach_ref`] for why those are different values and
    /// where the session is looked up. `#[serde(default)]` so a row written
    /// before the field existed still loads; such a row reattaches to nothing
    /// and is held rather than surfaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reattach_ref: Option<String>,
    /// The outward record this dispatch reconciles against, as the gate named it.
    ///
    /// Copied from [`PendingEffect::reconcile_ref`] at intent time and never
    /// re-derived — re-derivation under a resumed worker's scope is the exact
    /// false `DidNotFire` [`CommittedActRef`] exists to delete.
    ///
    /// `None` means *this dispatch is not outward*, never *nothing left*. See
    /// [`EffectDisposition::Reconcile`].
    ///
    /// `#[serde(default)]` so a row written before this field existed still
    /// loads. Such a row resolves as though the dispatch were not outward, which
    /// for a non-retry-safe effect is `SurfaceToUser` — the conservative
    /// direction, and the only one available when the ref was never recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_ref: Option<CommittedActRef>,
}

impl EffectLedgerEntry {
    /// A row for an effect about to be fired.
    pub fn intent(pending: &PendingEffect, iteration: usize, phase: Phase, at_ms: i64) -> Self {
        Self {
            effect_id: pending.effect_id.clone(),
            iteration,
            phase,
            tool: pending.tool.clone(),
            arguments_fingerprint: pending.arguments_fingerprint.clone(),
            retry_safety: pending.retry_safety,
            intent_at_ms: at_ms,
            prepared_only: true,
            outcome: None,
            reattach_ref: pending.reattach_ref.clone(),
            reconcile_ref: pending.reconcile_ref.clone(),
        }
    }

    /// The dispatch this row was committed for, as the batch stated it.
    ///
    /// The inverse of [`Self::intent`], and total rather than lossy — see this
    /// type's own docs for why that matters. It exists so a worker resuming from
    /// the ledger alone can ask [`EffectLedger::disposition`] the same question
    /// a worker holding a [`PendingBatch`] asks, and get the same answer.
    pub fn pending(&self) -> PendingEffect {
        PendingEffect {
            effect_id: self.effect_id.clone(),
            tool: self.tool.clone(),
            arguments_fingerprint: self.arguments_fingerprint.clone(),
            retry_safety: self.retry_safety,
            reconcile_ref: self.reconcile_ref.clone(),
            reattach_ref: self.reattach_ref.clone(),
        }
    }
}

/// Whether the intents for a gated batch are on durable storage.
///
/// The receipt `phases::apply::dispatch` requires before it will fire anything.
/// Its constructors are `pub(in ..::run_loop)`, which is the whole mechanism:
/// `executor.rs` implements the worker host and sits **outside** this module
/// tree, so a host cannot mint one at all and can only pass on the value a
/// driver handed it. A host therefore cannot dispatch a batch that no driver
/// considered.
///
/// # The ceiling, stated exactly, so nobody reads more into it
///
/// **This does not make "dispatch without recording intents" unspellable.** It
/// makes it unspellable *outside* `run_loop`, and inside `run_loop` it makes it
/// a named claim rather than an omission. A driver can still answer
/// [`Self::no_durable_ledger`] — and one has to, because `driver_inproc` holds
/// no store and could not record an intent if it wanted to.
///
/// So the true claim is: **you cannot dispatch without stating your ledger
/// posture, and only a driver may state it.** Not "you cannot dispatch without
/// writing". No receipt type could enforce the stronger claim while the
/// in-process arm exists, because that arm has nothing to write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyIntents(IntentsPosture);

#[derive(Debug, Clone, PartialEq, Eq)]
enum IntentsPosture {
    /// Every member of the gated batch has an intent row on disk.
    Committed { effects: usize },
    /// This arm has no durable store, so nothing could precede the fire.
    NoDurableLedger,
}

impl ApplyIntents {
    /// Every member of the batch has an intent on disk.
    ///
    /// Minted by `driver_worker::record_batch_intents` and nowhere else — the
    /// one function that awaits `LoopStateStore::record_effect_intent` for each
    /// member. Adding a second caller means adding a second definition of what
    /// this value claims.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn committed(effects: usize) -> Self {
        Self(IntentsPosture::Committed { effects })
    }

    /// This arm cannot record an intent, and says so rather than staying quiet.
    ///
    /// `driver_inproc` is the caller: its continuation is the Rust stack, so
    /// there is no store to write to and no later worker to read one. The value
    /// exists so that arm's position is a statement in the type rather than an
    /// absence a reader has to infer.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn no_durable_ledger() -> Self {
        Self(IntentsPosture::NoDurableLedger)
    }

    /// How many intents were recorded, or `None` on an arm that records none.
    ///
    /// For logging and for tests. Nothing branches on it to decide whether to
    /// fire: a `NoDurableLedger` batch still dispatches, because refusing would
    /// break the arm that is still the production default.
    pub fn recorded_intents(&self) -> Option<usize> {
        match self.0 {
            IntentsPosture::Committed { effects } => Some(effects),
            IntentsPosture::NoDurableLedger => None,
        }
    }
}

/// What a driver already established about each member of a batch `Apply` is
/// about to re-enter.
///
/// The second value `phases::apply::dispatch` takes from a driver, beside
/// [`ApplyIntents`], and the two answer different questions: the receipt says
/// *your intents are on disk*, this says *here is what the ledger and the
/// outward record already know about these exact members*.
///
/// # A PLAN, not a capability, and the distinction is the whole reason this is
/// # admissible
///
/// `phases/mod.rs` states the phase contract: a phase's **parameter list IS the
/// claim about what that phase may touch**, and it is load-bearing rather than
/// stylistic — `apply`'s own signature explains why one parameter is
/// `&Option<..>` and not `&mut`, because *"an unused `&mut` quietly weakens
/// it"*. A store, a ledger-writer handle or a callback would all widen that
/// claim: the phase could then go and **ask** something, at a moment of its own
/// choosing, which is exactly the ordering the gate/dispatch seam exists to take
/// away from it.
///
/// This is a **value**. It was computed before the phase was entered, by the one
/// party holding the store, and the phase can only read it. Nothing it carries
/// can be re-queried, refreshed, or written back. That is why an effect *plan*
/// is admissible where an effect *writer* is not, and it is the sentence to
/// quote at the next change that wants to thread a store into a phase.
///
/// # The ceiling, stated exactly
///
/// Its constructors are `pub(in ..::run_loop)`, so `executor.rs` — which
/// implements the worker host from outside this module tree — cannot mint one
/// and can only pass on the value a driver handed it. What that does **not** buy
/// is "a caller cannot dispatch without a real plan": a driver may legitimately
/// answer [`Self::nothing_resolved`], and `driver_inproc` has to, because it
/// holds no store and resolves nothing. So the true claim is the same one
/// [`ApplyIntents`] makes: **you cannot dispatch without stating what you
/// resolved, and only a driver may state it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPlan(PlanPosture);

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanPosture {
    /// The driver resolved a committed batch, member by member.
    ///
    /// Empty is reachable and is not the same statement as
    /// [`Self::NothingResolved`]: it means *this claim's cursor was `Apply`, the
    /// driver looked, and neither the committed batch nor the ledger named
    /// anything*. That is the ordinary first entry into a turn.
    Resolved(Vec<PlannedEffect>),
    /// This arm resolves nothing, ever, because it has no store to resolve
    /// against.
    ///
    /// `driver_inproc` is the caller, for the same reason it answers
    /// [`ApplyIntents::no_durable_ledger`]: its continuation is the Rust stack,
    /// so no earlier attempt's effects can exist to be planned around. The value
    /// exists so that arm's position is a statement in the type rather than an
    /// absence a reader has to infer.
    NothingResolved,
}

/// One member of an [`EffectPlan`].
///
/// Keyed by [`EffectId`] and never by tool name: two members of one batch can
/// share a tool, and a plan matched by tool would hand one member's verdict to
/// the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEffect {
    pub effect_id: EffectId,
    pub action: EffectAction,
}

impl EffectPlan {
    /// What the driver resolved, in the order it resolved it.
    ///
    /// Minted by `driver_worker::advance_once` from the same `Vec<ResolvedEffect>`
    /// it puts on the `PhaseEntry`, so the gate and the dispatch are told the
    /// same thing about the same members.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn resolved(
        planned: Vec<PlannedEffect>,
    ) -> Self {
        Self(PlanPosture::Resolved(planned))
    }

    /// This arm has no store, so nothing could have been resolved.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn nothing_resolved() -> Self {
        Self(PlanPosture::NothingResolved)
    }

    /// How many members the driver resolved, or `None` on an arm that resolves
    /// none.
    ///
    /// For logging and for tests, exactly as [`ApplyIntents::recorded_intents`]
    /// is. Nothing branches on it to decide whether to fire — that decision is
    /// per member, through [`Self::action_for`].
    pub fn planned_effects(&self) -> Option<usize> {
        match &self.0 {
            PlanPosture::Resolved(planned) => Some(planned.len()),
            PlanPosture::NothingResolved => None,
        }
    }

    /// What the driver established about this exact effect, if anything.
    ///
    /// `None` means *the driver said nothing about this member*, which for a
    /// member the gate just admitted means it is owed: no ledger row claims it
    /// fired, so nothing licenses skipping it.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn action_for(
        &self,
        effect_id: &EffectId,
    ) -> Option<&EffectAction> {
        match &self.0 {
            PlanPosture::Resolved(planned) => planned
                .iter()
                .find(|entry| entry.effect_id == *effect_id)
                .map(|entry| &entry.action),
            PlanPosture::NothingResolved => None,
        }
    }

    /// Every effect this plan names, in the driver's order.
    ///
    /// The correspondence check reads it: a plan naming a member the gate did
    /// not admit is a plan about a different batch, and `phases::apply` refuses
    /// rather than dispatching around it.
    pub(in crate::magician_v2::execution::agentic::run_loop) fn effect_ids(
        &self,
    ) -> impl Iterator<Item = &EffectId> + '_ {
        let planned: &[PlannedEffect] = match &self.0 {
            PlanPosture::Resolved(planned) => planned.as_slice(),
            PlanPosture::NothingResolved => &[],
        };
        planned.iter().map(|entry| &entry.effect_id)
    }
}

/// Refuse an offered dispatch that is not the one an effect id already names.
///
/// The single check behind [`EffectLedger::record_intent`] — which refuses an
/// effect id re-pointed at a different call — and
/// [`EffectLedger::authorize_refire`], which refuses a re-derivation that does
/// not reproduce what was committed. One function rather than two, because two
/// would drift, and the drift would be a re-fire the intent commit itself would
/// have refused.
///
/// The two mismatches are different errors on purpose. A fingerprint mismatch
/// reported as an [`EffectError::IntentConflict`] prints the same tool name in
/// both halves of its message and sends an operator looking for a repointing
/// that is not there — the same defect the duplicate-row arm of
/// [`EffectLedger::from_entries`] already avoids, for the same reason.
fn refuse_unless_same_dispatch(
    effect_id: &EffectId,
    recorded_tool: &str,
    recorded_arguments_fingerprint: &str,
    offered_tool: &str,
    offered_arguments_fingerprint: &str,
) -> Result<(), EffectError> {
    if recorded_tool != offered_tool {
        return Err(EffectError::IntentConflict {
            effect_id: effect_id.to_string(),
            recorded_tool: recorded_tool.to_string(),
            offered_tool: offered_tool.to_string(),
        });
    }
    if recorded_arguments_fingerprint != offered_arguments_fingerprint {
        return Err(EffectError::ArgumentsConflict {
            effect_id: effect_id.to_string(),
            tool: recorded_tool.to_string(),
            recorded_fingerprint: recorded_arguments_fingerprint.to_string(),
            offered_fingerprint: offered_arguments_fingerprint.to_string(),
        });
    }
    Ok(())
}

/// What a worker may do about one effect whose result it does not have.
///
/// Deliberately does **not** include "assume nothing happened". Every path that
/// could reach that conclusion goes through [`EffectDisposition::Reconcile`],
/// which asks the durable outward record, and only that record's positive
/// `DidNotFire` licenses a re-fire of a non-retry-safe effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectDisposition {
    /// A result is recorded. Take it and move on; no re-spend, no re-fire.
    Adopt,
    /// Fire it. Reached two ways, and only one of them is a guess.
    ///
    /// - The effect is declared **retry-safe**, so firing twice is licensed by
    ///   the capability rather than by evidence.
    /// - The ledger recorded [`EffectOutcome::NotDispatched`] — positive
    ///   evidence that it never went out. This is the one path to a fire that
    ///   rests on knowledge rather than on a safety declaration, and it holds
    ///   for a `NotRetrySafe` effect too, because "may this run twice" is not
    ///   the question when it has not run once.
    Refire,
    /// No result, and the effect is resumable by session identity. Never
    /// re-fire, never straight to the user.
    ///
    /// The payload is the **invocation** the gate named, not the session — see
    /// [`PendingEffect::reattach_ref`]. `None` is a row that names no invocation
    /// at all, which a driver holds as indeterminate rather than resolving: it
    /// has nothing to look the session up by.
    Reattach { reattach_ref: Option<String> },
    /// No result, and re-firing could double a real-world effect. Ask the
    /// outward record via `reconcile_outward_effect`, then act on
    /// [`ReconciledEffect`].
    Reconcile,
}

/// What the outward record's answer licenses.
///
/// The mapping is one-way on purpose: `StillUnknown` may never become a re-fire,
/// because reading "we don't know" as "nothing left" re-sends a live message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciledEffect {
    /// The effect exists. Settle against it. `by_this_attempt` distinguishes
    /// "the effect exists" from "this attempt made it" — for deciding whether to
    /// re-send only the first matters, for explaining what happened both do.
    AlreadyFired { by_this_attempt: bool },
    /// Positive evidence nothing left: the act was refused, or never moved past
    /// preparation. Sound because the gate writes the disclosure *before*
    /// anything leaves and fails the send closed if that write fails.
    SafeToRefire,
    /// No answer. Surface it; do not guess.
    SurfaceToUser { reason: String },
}

/// The only four things a worker may do about an effect it did not fire.
///
/// There is deliberately no "assume nothing happened": every path that could
/// reach that conclusion goes through the outward record, and only its positive
/// answer produces [`EffectAction::Refire`].
///
/// # It lives here rather than in `driver_worker`, and that MOVED on 2026-08-29
///
/// It was declared beside [`ResolvedEffect`](super::driver_worker::ResolvedEffect)
/// while the driver was its only reader. Since `phases::apply::dispatch` takes
/// an [`EffectPlan`], the phase reads it too — and a phase importing a type from
/// the driver that runs it is the wrong direction: `driver_worker` depends on
/// `phases`, not the reverse. `driver_worker` re-exports the name, so
/// `driver_worker::EffectAction` still resolves.
///
/// # Every variant is answered explicitly, and a `_` arm here is a live re-fire
///
/// `phases::apply` matches this exhaustively on purpose. A plan that handled
/// only [`Self::Reattach`] and let the rest fall through would fire the
/// [`Self::Adopt`] case a second time — an effect whose result is already
/// recorded, dispatched again because nobody named its arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectAction {
    /// A result is recorded. Take it; do not fire.
    Adopt {
        result: Option<serde_json::Value>,
        succeeded: bool,
    },
    /// Licensed to fire again — declared retry-safe, or the outward record gave
    /// positive evidence nothing left — **and** the re-derived arguments
    /// reproduce the committed fingerprint.
    Refire,
    /// Resume by session identity.
    ///
    /// The payload is the **session**, not the ledger row's `reattach_ref`. The
    /// row names an invocation;
    /// [`WorkerHost::reattach_state`](super::driver_worker::WorkerHost::reattach_state)
    /// turns that into the handle a resume speaks to, and this variant is only
    /// reached once it has. Naming the two alike is how a worker ends up handing
    /// an invocation id to something expecting a session.
    ///
    /// `phases::apply::dispatch` maps this to its reattach member posture and
    /// forwards the session id through the coding runtime parameters. The
    /// variant is reached only for a coding job, because `run_coding_task` is
    /// the only capability `phases::apply::declared_retry_safety` calls
    /// `Reattachable`.
    Reattach { native_session_id: String },
    /// The outward record says it already fired. Settle against it; do not send.
    AlreadyFired { by_this_attempt: bool },
}

impl From<&EffectReconciliation> for ReconciledEffect {
    fn from(reconciliation: &EffectReconciliation) -> Self {
        match reconciliation {
            EffectReconciliation::Fired {
                by_this_attempt, ..
            } => ReconciledEffect::AlreadyFired {
                by_this_attempt: *by_this_attempt,
            },
            EffectReconciliation::DidNotFire { .. } => ReconciledEffect::SafeToRefire,
            EffectReconciliation::StillUnknown { reason } => ReconciledEffect::SurfaceToUser {
                reason: reason.clone(),
            },
        }
    }
}

/// The loop's own view of what it has in flight.
///
/// A map rather than a log, because parallel batch members settle out of order
/// and the question a resuming worker asks is *"what happened to this one"* —
/// which a status map answers in one lookup and a scan answers by reading
/// everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EffectLedger {
    entries: BTreeMap<String, EffectLedgerEntry>,
}

/// Take a ledger back off the wire, checking what a bare map cannot check for
/// itself.
///
/// Hand-written rather than derived, for the reason this module keeps having to
/// make: the rows that hurt are the ones that arrived from disk. A derived
/// transparent impl accepts a map of any size, and — the part that is not merely
/// a bound — a map whose **key disagrees with the row filed under it**. That is
/// not cosmetic. [`EffectLedger::get`] looks a row up by key, so a row filed
/// under the wrong one is invisible to [`EffectLedger::disposition`] while
/// [`EffectLedger::entries`] still reports it: an effect with a recorded
/// `Succeeded` would present as an effect with no result, which for anything
/// declared retry-safe is a licence to fire it a second time.
impl<'de> Deserialize<'de> for EffectLedger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let entries = BTreeMap::<String, EffectLedgerEntry>::deserialize(deserializer)?;
        if entries.len() > MAX_LEDGER_ENTRIES {
            return Err(D::Error::custom(
                EffectError::LedgerFull {
                    limit: MAX_LEDGER_ENTRIES,
                }
                .to_string(),
            ));
        }
        for (key, entry) in &entries {
            if key.as_str() != entry.effect_id.as_str() {
                return Err(D::Error::custom(format!(
                    "an effect ledger row filed under {key} names the effect {}; a lookup would \
                     never find it and a scan would report it anyway",
                    entry.effect_id
                )));
            }
        }
        Ok(Self { entries })
    }
}

impl EffectLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assemble a ledger from rows a store loaded, checking what a store cannot.
    ///
    /// Two rows claiming one effect id is a real defect — the ledger is a map
    /// and a store that filed two rows under one key has lost one of them — so
    /// it is an error rather than a last-writer-wins merge. The entry count is
    /// bounded here as well as at insert, because the rows that hurt are the
    /// ones that arrived from disk rather than the ones this process created.
    pub fn from_entries(
        entries: impl IntoIterator<Item = EffectLedgerEntry>,
    ) -> Result<Self, EffectError> {
        let mut ledger = Self::default();
        for entry in entries {
            if ledger.entries.len() >= MAX_LEDGER_ENTRIES {
                return Err(EffectError::LedgerFull {
                    limit: MAX_LEDGER_ENTRIES,
                });
            }
            if ledger.entries.contains_key(entry.effect_id.as_str()) {
                // Not an `IntentConflict`. The two rows may name the same tool,
                // and reporting that as one id pointed at two dispatches sends an
                // operator looking for a repointing that never happened. What
                // actually went wrong is that two rows were filed under one key
                // and one of them is already lost.
                return Err(EffectError::DuplicateEffectId {
                    effect_id: entry.effect_id.to_string(),
                });
            }
            ledger.entries.insert(entry.effect_id.to_string(), entry);
        }
        Ok(ledger)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, effect_id: &EffectId) -> Option<&EffectLedgerEntry> {
        self.entries.get(effect_id.as_str())
    }

    pub fn entries(&self) -> impl Iterator<Item = &EffectLedgerEntry> {
        self.entries.values()
    }

    /// Record that an effect is about to fire.
    ///
    /// Re-recording the same intent is fine and expected — a phase that crashed
    /// after committing its intent re-runs and commits it again. Re-recording it
    /// pointed at a **different dispatch** is not: an effect id names one attempt
    /// at one thing, and letting the second silently replace the first would mean
    /// a reconciliation asked about tool B and answered about tool A.
    pub fn record_intent(&mut self, mut entry: EffectLedgerEntry) -> Result<(), EffectError> {
        if let Some(existing) = self.entries.get(entry.effect_id.as_str()).cloned() {
            refuse_unless_same_dispatch(
                &entry.effect_id,
                &existing.tool,
                &existing.arguments_fingerprint,
                &entry.tool,
                &entry.arguments_fingerprint,
            )?;
            // `reconcile_ref` is dispatch identity, not optional decoration.
            // Re-recording is monotonic: an older worker may omit a ref the
            // durable row already knows, and a newer gate may fill one a legacy
            // row lacked. Two present values must be equal, because choosing
            // either unequal act would reconcile this effect id against a
            // different dispatch.
            match (&existing.reconcile_ref, &entry.reconcile_ref) {
                (Some(recorded), Some(offered)) if recorded != offered => {
                    return Err(EffectError::ReconcileRefConflict {
                        effect_id: entry.effect_id.to_string(),
                        recorded: recorded.to_string(),
                        offered: offered.to_string(),
                    })
                },
                (Some(recorded), None) => {
                    entry.reconcile_ref = Some(recorded.clone());
                },
                (Some(_), Some(_)) | (None, Some(_)) | (None, None) => {},
            }
            // An intent re-committed after a crash must not erase a terminal
            // result that landed in between. `NotDispatched` is deliberately
            // different: it licensed this new attempt to fire, so the durable
            // row must be re-armed to unknown *before* that fire. Leaving the
            // old positive "nothing left" evidence in place would let a crash
            // after the new fire license yet another fire on recovery.
            let rearming_not_dispatched = matches!(
                existing.outcome.as_ref(),
                Some(EffectOutcome::NotDispatched { .. })
            );
            match existing.outcome.as_ref() {
                Some(EffectOutcome::NotDispatched { .. }) => {
                    entry.outcome = None;
                },
                Some(_) => {
                    // A terminal outcome remains authoritative, but a legacy
                    // row may still learn the act ref the current gate proved.
                    // Do this only after the equality check above: returning
                    // early before it would let settled rows silently disagree
                    // about which outward act their effect id names.
                    if existing.reconcile_ref.is_none() && entry.reconcile_ref.is_some() {
                        self.entries
                            .get_mut(entry.effect_id.as_str())
                            .expect("the cloned existing row remains present")
                            .reconcile_ref = entry.reconcile_ref;
                    }
                    return Ok(());
                },
                None => {},
            }
            // Nor may it erase what the effect reattaches THROUGH. A coding job
            // whose row lost its invocation id would come back as
            // `Reattach { reattach_ref: None }` — reattach to what? — and the
            // engine would start a fresh session instead. Recoverable, and
            // exactly the kind of silent degradation that is never traced back
            // to the intent commit that caused it. The re-run's value wins when
            // it has one; otherwise what was recorded stands — and a re-run of
            // the same effect mints the same id anyway, so the two agree in
            // every case this is reached honestly.
            if entry.reattach_ref.is_none() {
                entry.reattach_ref = existing.reattach_ref.clone();
            }
            // Nor may it widen the safety the first commit recorded. A re-run can
            // be a different build, or the same one after a capability changed
            // what it declares, and the safety that governs an effect is the one
            // that was true when it left. [`Self::disposition`] takes the
            // stricter of the recorded and the offered value for exactly this
            // reason — and that is worth nothing if the re-commit can overwrite
            // the recorded half first.
            entry.retry_safety = RetrySafety::stricter(existing.retry_safety, entry.retry_safety);
            // A legacy active row never becomes prepared-only merely because a
            // newer worker re-recorded it. Conversely, a prepared row stays
            // prepared until the state CAS names its complete batch.
            entry.prepared_only = rearming_not_dispatched || existing.prepared_only;
        }
        if self.entries.len() >= MAX_LEDGER_ENTRIES
            && !self.entries.contains_key(entry.effect_id.as_str())
        {
            return Err(EffectError::LedgerFull {
                limit: MAX_LEDGER_ENTRIES,
            });
        }
        self.entries.insert(entry.effect_id.to_string(), entry);
        Ok(())
    }

    /// Record what happened to an effect.
    ///
    /// Recording the *same* outcome twice is idempotent. Recording a
    /// **different** one is an error rather than an overwrite: two holders
    /// settling one effect differently is a real defect, and the version that
    /// wins by arriving second is not more likely to be right.
    ///
    /// # Two evidence upgrades: `NotDispatched` and `Indeterminate`
    ///
    /// [`EffectOutcome::NotDispatched`] is a statement about **one attempt** —
    /// *this* run of `Apply` admitted the effect and stopped before firing it.
    /// A later attempt that actually dispatched knows strictly more, and it
    /// knows it about the same effect id, so letting it write is not two holders
    /// disagreeing; it is one holder reporting and a later one finishing the
    /// job.
    ///
    /// A recorded [`EffectOutcome::Indeterminate`] is explicitly no answer.
    /// Durable reconciliation or an operator resolution may replace it with a
    /// terminal result or with `NotDispatched`; a second, different
    /// `Indeterminate` still conflicts because it is not stronger evidence.
    ///
    /// The reverse is refused. Writing `NotDispatched` over a recorded
    /// `Succeeded` would turn a send that happened into positive evidence that
    /// nothing left — the one lie [`EffectDisposition`] has no defence against,
    /// because `NotDispatched` is the variant it *trusts*.
    pub fn record_outcome(
        &mut self,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> Result<(), EffectError> {
        let Some(entry) = self.entries.get_mut(effect_id.as_str()) else {
            return Err(EffectError::UnknownEffect {
                effect_id: effect_id.to_string(),
            });
        };
        match &entry.outcome {
            Some(existing) if existing == &outcome => Ok(()),
            // See the asymmetry above. Guarded on the OFFERED value too, so
            // `NotDispatched` over `NotDispatched` with a different reason is
            // still a conflict rather than a silent reason-rewrite.
            Some(EffectOutcome::NotDispatched { .. })
                if !matches!(outcome, EffectOutcome::NotDispatched { .. }) =>
            {
                entry.outcome = Some(outcome);
                Ok(())
            },
            Some(EffectOutcome::Indeterminate { .. })
                if !matches!(outcome, EffectOutcome::Indeterminate { .. }) =>
            {
                entry.outcome = Some(outcome);
                Ok(())
            },
            Some(existing) => Err(EffectError::OutcomeConflict {
                effect_id: effect_id.to_string(),
                recorded: format!("{existing:?}"),
                offered: format!("{outcome:?}"),
            }),
            None => {
                entry.outcome = Some(outcome);
                Ok(())
            },
        }
    }

    /// Every effect this run committed to and does not have an answer for.
    ///
    /// **The question the ledger is asked on recovery**, and it is asked here
    /// rather than at the call site because it is a question about the ledger's
    /// own contents and the definition of *unsettled* must not have two
    /// spellings. `driver_worker::resolve_effects` iterates this and asks
    /// [`Self::disposition`] about each row.
    ///
    /// # `Indeterminate` counts as unsettled, and that is not an oversight
    ///
    /// [`Self::disposition`] already treats a recorded `Indeterminate` as
    /// equivalent to no row at all — *"it is the ledger saying it does not know,
    /// which is the same position as no row"*. A row it would route to
    /// `Reconcile` is a row a resume must resolve, so excluding it here would
    /// let a known-unknown effect past the boundary while an unknown-unknown one
    /// was held. The two definitions have to agree, and this is the one that
    /// decides what gets asked.
    ///
    /// [`EffectOutcome::NotDispatched`] is settled: it is the answer, not the
    /// absence of one.
    pub fn unsettled(&self) -> impl Iterator<Item = &EffectLedgerEntry> {
        self.entries.values().filter(|entry| match &entry.outcome {
            None | Some(EffectOutcome::Indeterminate { .. }) => true,
            Some(
                EffectOutcome::Succeeded { .. }
                | EffectOutcome::SucceededWithResult { .. }
                | EffectOutcome::Failed { .. }
                | EffectOutcome::FailedWithResult { .. }
                | EffectOutcome::NotDispatched { .. },
            ) => false,
        })
    }

    /// Point an existing row at what it reattaches through.
    ///
    /// **Not the ordinary path.** The ordinary path is the intent commit:
    /// [`EffectLedgerEntry::intent`] copies [`PendingEffect::reattach_ref`], so a
    /// row is born pointing at the invocation the gate named and this method has
    /// nothing to add. It stays because the ledger is a store and a store may be
    /// corrected — and because a row loaded from a snapshot written before the
    /// field existed can be given one without rewriting the intent.
    pub fn record_reattach_ref(
        &mut self,
        effect_id: &EffectId,
        reattach_ref: impl Into<String>,
    ) -> Result<(), EffectError> {
        let Some(entry) = self.entries.get_mut(effect_id.as_str()) else {
            return Err(EffectError::UnknownEffect {
                effect_id: effect_id.to_string(),
            });
        };
        entry.reattach_ref = Some(reattach_ref.into());
        Ok(())
    }

    /// What to do about one effect, given what the ledger knows.
    ///
    /// An effect the ledger has never heard of is [`EffectDisposition::Reconcile`]
    /// when it is not retry-safe, **not** `Refire`. The ledger's silence is not
    /// evidence: the intent commit and the fire are two operations, and a worker
    /// that died between them left no row for an effect that may well have
    /// happened.
    ///
    /// # Silence is not evidence; `NotDispatched` is
    ///
    /// The two are worth holding apart, because they look alike from a distance
    /// and license opposite things. *No row* means nobody wrote anything down —
    /// consistent with a fire nobody survived to record. A recorded
    /// [`EffectOutcome::NotDispatched`] means the function that would have fired
    /// it wrote down that it did not, **after** the intent was already durable.
    /// That is testimony, not absence, and it is the only thing in this module
    /// that licenses firing a `NotRetrySafe` effect without reading the outward
    /// record first.
    ///
    /// # A committed act ref outranks a `RetrySafe` declaration
    ///
    /// [`PendingEffect::reconcile_ref`] is written only for a dispatch the
    /// outward classifier says can reach somebody, so its presence is positive
    /// evidence that this effect is a live send. A `RetrySafe` declaration on
    /// one of those would answer [`EffectDisposition::Refire`] — a re-send with
    /// no record read at all, which is worse than the false `DidNotFire` this
    /// module spends most of its length closing.
    ///
    /// The two verdicts genuinely can disagree, which is why this is a rule and
    /// not an assertion: the safety declaration comes from a capability, and the
    /// outward verdict comes from a classifier that also reads the arguments —
    /// `outward_dispatch_class` exists precisely because token matching alone
    /// called a real `gmail raw` send *"not outward"*. A capability declaring
    /// itself read-only while a passthrough argument turns one call into a send
    /// is the case, and the same conservatism [`RetrySafety::stricter`] applies
    /// to a changed declaration applies here.
    pub fn disposition(&self, pending: &PendingEffect) -> EffectDisposition {
        let entry = self.get(&pending.effect_id);
        // A recorded success or a recorded failure both answer the question. An
        // `Indeterminate` row does not — it is the ledger saying it does not
        // know, which is the same position as no row at all.
        if let Some(entry) = entry {
            match entry.outcome {
                Some(EffectOutcome::Succeeded { .. })
                | Some(EffectOutcome::SucceededWithResult { .. })
                | Some(EffectOutcome::Failed { .. })
                | Some(EffectOutcome::FailedWithResult { .. }) => return EffectDisposition::Adopt,
                // The one row that licenses a fire without asking anybody, and
                // it does so for a `NotRetrySafe` effect as well. `Reconcile`
                // exists to find out whether something left; this row already
                // says it did not, and it was written by the function that
                // would have sent it. Routing it to `Reconcile` anyway would
                // ask the outward record a question the loop has already
                // answered better — and for a non-outward dispatch there is no
                // record to ask, so it would surface to a user instead.
                Some(EffectOutcome::NotDispatched { .. }) => return EffectDisposition::Refire,
                Some(EffectOutcome::Indeterminate { .. }) | None => {},
            }
        }
        // The stricter of what the caller is holding and what was recorded when
        // the effect was committed. They can disagree — a capability's declared
        // safety can change between the intent and the resume, and a resumed
        // worker can be running different configuration from the one that fired
        // — and taking the caller's word would let a later, laxer declaration
        // re-fire something that was recorded as unsafe when it left.
        let retry_safety = entry.map_or(pending.retry_safety, |entry| {
            RetrySafety::stricter(entry.retry_safety, pending.retry_safety)
        });
        // The row is the authority when it has a ref; the offered batch is the
        // fallback for unsplit/legacy paths with no row. This is the same
        // recorded-first rule `Reattachable` uses below. Looking only at the
        // offered value would make `record_intent`'s monotonic ref preservation
        // inert and let an older batch omission bypass the outward downgrade.
        let committed_reconcile_ref = entry
            .and_then(|entry| entry.reconcile_ref.as_ref())
            .or(pending.reconcile_ref.as_ref());
        let retry_safety = match retry_safety {
            // See this method's own documentation. Narrowed to the one
            // declaration that licenses an unconditional fire: `Reattachable`
            // already forbids re-firing and resumes by session identity, and
            // running it through `stricter` here would replace that resume with
            // a reconciliation — whose `DidNotFire` is a licence to run a job
            // that mutates a real repository a second time.
            RetrySafety::RetrySafe if committed_reconcile_ref.is_some() => {
                RetrySafety::NotRetrySafe
            },
            declared => declared,
        };
        match retry_safety {
            RetrySafety::RetrySafe => EffectDisposition::Refire,
            // The RECORDED ref first, the offered one second, and the order is
            // not arbitrary: what was written down when the effect left is the
            // authority, exactly as it is for `retry_safety` above.
            //
            // The fallback is not decoration either. A host that answers
            // `splits_apply_gate_from_dispatch() == false` writes no intents at
            // all, yet its `Apply` really did dispatch — `commit_failed_attempt`
            // publishes the batch and nothing else. Reading only the row would
            // hand that run `None` and hold it, while the batch in hand names
            // the very invocation the coding ledger has on disk.
            RetrySafety::Reattachable => EffectDisposition::Reattach {
                reattach_ref: entry
                    .and_then(|entry| entry.reattach_ref.clone())
                    .or_else(|| pending.reattach_ref.clone()),
            },
            RetrySafety::NotRetrySafe => EffectDisposition::Reconcile,
        }
    }

    /// Licence a re-fire against what the run committed.
    ///
    /// The arguments a worker re-fires with are re-derived from the assistant
    /// turn the effect id names, and a re-derivation that drifts is silent:
    /// nothing in a tool call refuses arguments that are merely *different*
    /// from the ones the first attempt used. The fingerprint is what makes it
    /// loud. Call this after the disposition licenses a fire and before the
    /// dispatch, and fail the execution on the error rather than firing anyway
    /// — a mismatch means the worker does not have the dispatch it thinks it
    /// has.
    ///
    /// # This is not the reconciliation licence
    ///
    /// The fingerprint licenses a **re-fire**; [`PendingEffect::reconcile_ref`]
    /// licenses a **reconciliation**. Neither substitutes for the other, and
    /// collapsing them would quietly delete a step: a
    /// [`RetrySafety::NotRetrySafe`] effect still goes
    /// [`EffectDisposition::Reconcile`] → [`ReconciledEffect::SafeToRefire`] →
    /// re-derive → this check → fire. A matching fingerprint says the worker
    /// re-derived the right dispatch. It says nothing whatever about whether
    /// that dispatch already left.
    ///
    /// # Both statements of the commitment are checked
    ///
    /// The ledger row is what was recorded when the intent was taken; the
    /// [`PendingEffect`] is what the committed batch carries. Checking only the
    /// row would leave exactly one case unguarded, and it is the case that
    /// matters: a worker that died between the intent commit and the fire left
    /// no row at all — see [`Self::disposition`] on why ledger silence is not
    /// evidence. The pending effect is the half that is always there.
    pub fn authorize_refire(
        &self,
        committed: &PendingEffect,
        rederived_tool: &str,
        rederived_arguments_fingerprint: &str,
    ) -> Result<(), EffectError> {
        // An absent fingerprint is not a fingerprint, and two absences must not
        // agree. `refuse_unless_same_dispatch` compares strings, so `"" == ""`
        // would hand back `Ok(())` — a re-fire licensed by the fact that nothing
        // was recorded on either side, which is the same defect as reading an
        // absent act record as *nothing was sent*.
        //
        // Here rather than inside `refuse_unless_same_dispatch`, which
        // `record_intent` also uses: the intent commit must go on recording a
        // dispatch the runtime could not fingerprint — refusing there would
        // refuse the *dispatch*, not the re-fire, and break every caller that
        // has no lineage identity to fingerprint with. This is the one caller
        // that turns a match into a licence to fire, so this is where the
        // absence has to be refused. See `PendingEffect::arguments_fingerprint`.
        if committed.arguments_fingerprint.is_empty() || rederived_arguments_fingerprint.is_empty()
        {
            return Err(EffectError::UnfingerprintedRefire {
                effect_id: committed.effect_id.to_string(),
                tool: committed.tool.clone(),
            });
        }
        if let Some(existing) = self.get(&committed.effect_id) {
            refuse_unless_same_dispatch(
                &committed.effect_id,
                &existing.tool,
                &existing.arguments_fingerprint,
                rederived_tool,
                rederived_arguments_fingerprint,
            )?;
        }
        refuse_unless_same_dispatch(
            &committed.effect_id,
            &committed.tool,
            &committed.arguments_fingerprint,
            rederived_tool,
            rederived_arguments_fingerprint,
        )
    }

    /// What to do about every member of a batch.
    ///
    /// Returned as a per-effect map rather than a cursor, because parallel
    /// members complete out of order — a cursor would resume from the wrong
    /// place the first time the second member finished before the first.
    pub fn plan(&self, batch: &PendingBatch) -> Vec<(EffectId, EffectDisposition)> {
        batch
            .effects
            .iter()
            .map(|pending| (pending.effect_id.clone(), self.disposition(pending)))
            .collect()
    }
}

/// The most rows one run's ledger may hold.
///
/// Bounds an execution that dispatches for a very long time. A run that exceeds
/// this is telling us the snapshot cadence or the iteration ceiling is wrong,
/// which is a configuration answer rather than a licence to grow without limit.
pub const MAX_LEDGER_ENTRIES: usize = 100_000;

/// Why an effect ledger refused an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectError {
    EmptyBatch,
    BatchTooLarge {
        count: usize,
        limit: usize,
    },
    ParallelSliceOverruns {
        admitted: usize,
        effects: usize,
    },
    /// Two records claimed one effect id where only one can be kept — two
    /// members of a batch, or two rows loaded into a map.
    DuplicateEffectId {
        effect_id: String,
    },
    /// One effect id was pointed at two different dispatches.
    IntentConflict {
        effect_id: String,
        recorded_tool: String,
        offered_tool: String,
    },
    /// One effect id was pointed at the same tool with different arguments.
    ///
    /// Separate from [`EffectError::IntentConflict`] because the tools agree:
    /// reporting it as a repointing would print one name in both halves of the
    /// message and send an operator looking for a mismatch that is not there.
    /// The mismatch that IS there is the fingerprint, so that is what is named.
    ArgumentsConflict {
        effect_id: String,
        tool: String,
        recorded_fingerprint: String,
        offered_fingerprint: String,
    },
    /// One effect id was offered with two different outward-record identities.
    ReconcileRefConflict {
        effect_id: String,
        recorded: String,
        offered: String,
    },
    /// An act ref that is not the shape `derive_act_ref` produces.
    MalformedReconcileRef {
        offered: String,
    },
    /// A re-fire was offered for an effect with no committed arguments
    /// fingerprint, or with no re-derived one to check it against.
    UnfingerprintedRefire {
        effect_id: String,
        tool: String,
    },
    /// Two holders settled one effect differently.
    OutcomeConflict {
        effect_id: String,
        recorded: String,
        offered: String,
    },
    UnknownEffect {
        effect_id: String,
    },
    LedgerFull {
        limit: usize,
    },
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectError::EmptyBatch => write!(
                f,
                "an effect batch with no members is an intent commit with nothing to fire"
            ),
            EffectError::BatchTooLarge { count, limit } => {
                write!(f, "an effect batch of {count} is over the limit of {limit}")
            },
            EffectError::ParallelSliceOverruns { admitted, effects } => write!(
                f,
                "a parallel slice of {admitted} overruns a batch of {effects} effects"
            ),
            EffectError::DuplicateEffectId { effect_id } => write!(
                f,
                "two records claim the effect id {effect_id}; it names one attempt at one thing, \
                 so one of the two is already lost"
            ),
            EffectError::IntentConflict {
                effect_id,
                recorded_tool,
                offered_tool,
            } => write!(
                f,
                "effect {effect_id} is recorded against {recorded_tool} and was re-offered \
                 against {offered_tool}; an effect id names one attempt at one thing"
            ),
            EffectError::ArgumentsConflict {
                effect_id,
                tool,
                recorded_fingerprint,
                offered_fingerprint,
            } => write!(
                f,
                "effect {effect_id} is recorded against {tool} with arguments \
                 {recorded_fingerprint} and was re-offered with {offered_fingerprint}; a \
                 re-derivation that does not reproduce the committed arguments is not the \
                 dispatch this effect id names"
            ),
            EffectError::ReconcileRefConflict {
                effect_id,
                recorded,
                offered,
            } => write!(
                f,
                "effect {effect_id} is recorded against outward act {recorded} and was \
                 re-offered against {offered}; one effect id cannot reconcile through two acts"
            ),
            EffectError::MalformedReconcileRef { offered } => write!(
                f,
                "{offered} is not an act ref: one is '{RECONCILE_REF_PREFIX}' followed by \
                 {RECONCILE_REF_DIGEST_HEX} lowercase hex characters, and the value addresses a \
                 file under the scope root, so anything else reads a record this run never wrote"
            ),
            EffectError::UnfingerprintedRefire { effect_id, tool } => write!(
                f,
                "effect {effect_id} against {tool} has no arguments fingerprint on one side of \
                 the re-fire check, so there is nothing to reproduce; two absent fingerprints \
                 comparing equal would license a re-fire on the strength of nothing having been \
                 recorded twice"
            ),
            EffectError::OutcomeConflict {
                effect_id,
                recorded,
                offered,
            } => write!(
                f,
                "effect {effect_id} already settled as {recorded} and was offered {offered}; two \
                 holders settled one effect differently"
            ),
            EffectError::UnknownEffect { effect_id } => write!(
                f,
                "effect {effect_id} has no recorded intent, so there is nothing to settle"
            ),
            EffectError::LedgerFull { limit } => {
                write!(f, "this run's effect ledger already holds {limit} entries")
            },
        }
    }
}

impl std::error::Error for EffectError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::evidence::OutwardActStatus;

    fn effect(id: &str, retry_safety: RetrySafety) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id must be well-formed"),
            tool: "gmail__send".to_string(),
            arguments_fingerprint: "fp-1".to_string(),
            retry_safety,
            reconcile_ref: None,
            // Deliberately absent even for a `Reattachable` fixture. The gate
            // mints this; a fixture that always carried one could not express
            // the row a pre-mint snapshot leaves behind, which is the case
            // `resolve_effects` still has to hold rather than resolve.
            reattach_ref: None,
        }
    }

    /// The scope every fixture in this module derives its refs under.
    const FIXTURE_PRINCIPAL: &str = "anonymous";
    const FIXTURE_WORKSPACE: &str = "default";

    /// A ref of the shape `derive_act_ref` mints: the prefix plus a 32-character
    /// lowercase hex digest. The bare string, for the wire-shape tests.
    fn raw_act_ref(seed: char) -> String {
        format!(
            "{RECONCILE_REF_PREFIX}{}",
            std::iter::repeat(seed)
                .take(RECONCILE_REF_DIGEST_HEX)
                .collect::<String>()
        )
    }

    /// The same ref, bound to the scope it was derived under — which is the only
    /// form [`PendingEffect::reconcile_ref`] accepts.
    fn act_ref(seed: char) -> CommittedActRef {
        CommittedActRef::new(raw_act_ref(seed), FIXTURE_PRINCIPAL, FIXTURE_WORKSPACE)
            .expect("a fixture act ref must be the derived shape")
    }

    #[test]
    fn an_effect_id_needs_both_halves() {
        // The failure this refuses is silent and expensive: a blank
        // model_tool_call_id composes to one key for every unidentified call in
        // a turn, and a remote honouring it answers the second request from the
        // first's cached response.
        assert!(EffectId::from_parts("llm-1", "call-1").is_ok());
        assert_eq!(
            EffectId::from_parts("llm-1", "  "),
            Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id"
            })
        );
        assert_eq!(
            EffectId::from_parts("", "call-1"),
            Err(EffectIdError::EmptyHalf {
                half: "llm_call_id"
            })
        );
        assert_eq!(
            EffectId::parse("llm-1:tool:"),
            Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id"
            })
        );
        assert_eq!(
            EffectId::parse("not-an-effect-id"),
            Err(EffectIdError::MissingSeparator)
        );
    }

    #[test]
    fn an_effect_id_composes_the_way_the_runtime_already_composes_it() {
        // Not a restatement of the format: this asserts that this module
        // delegates to the single composer rather than formatting its own
        // string, which is the property that keeps one attempt from having two
        // names.
        let id = EffectId::from_parts("llm-7", "call-9").expect("well-formed");
        assert_eq!(
            id.as_str(),
            LlmToolLineageIdentity::tool_execution_id_for("llm-7", "call-9")
        );
        assert_eq!(
            id.local_attempt_key(),
            LlmToolLineageIdentity::local_attempt_key(id.as_str())
        );
    }

    #[test]
    fn a_fanned_out_step_gets_its_own_id_anchored_at_the_first_separator() {
        let base = EffectId::from_parts("llm-1", "call-1").expect("well-formed");
        let step = base.sub_step("step-3").expect("a step id must compose");
        assert_eq!(step.as_str(), "llm-1:tool:call-1:step:step-3");
        assert!(base.sub_step("   ").is_err());

        // Anchoring on the FIRST separator is what stops a later one from
        // re-reading which part of the id is the call. Asserting on an accepted
        // string cannot show that: the id is stored verbatim, so it reads back
        // the same whichever separator the check split on. What distinguishes
        // them is an id whose first half is blank and whose second separator
        // hides it — split from the right, `:tool:call-1` becomes a non-empty
        // call id and a value that names nothing gets in.
        assert_eq!(
            EffectId::parse(":tool:call-1:tool:b"),
            Err(EffectIdError::EmptyHalf {
                half: "llm_call_id"
            })
        );
        assert_eq!(
            EffectId::parse("llm-1:tool::tool:b"),
            Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id"
            })
        );
        // A legitimate nested step id still loads.
        assert!(EffectId::parse("llm-1:tool:call-1:step:a:tool:b").is_ok());
    }

    #[test]
    fn an_effect_id_survives_a_json_round_trip_and_a_bad_one_does_not_parse() {
        let id = EffectId::from_parts("llm-1", "call-1").expect("well-formed");
        let encoded = serde_json::to_string(&id).expect("encode");
        assert_eq!(encoded, "\"llm-1:tool:call-1\"");
        let decoded: EffectId = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, id);

        // The validation is on the deserializer, not only on the constructor —
        // otherwise a hand-edited or older record would put an unusable id back
        // into circulation.
        let bad: Result<EffectId, _> = serde_json::from_str("\"llm-1:tool:\"");
        assert!(bad.is_err(), "a blank half must not survive a round trip");

        // The other two ways a string off the wire is not an id. Neither has a
        // write-side counterpart — nothing in the runtime composes an id that
        // long or with a control character in it — which is the point: a value
        // carrying one did not come from the runtime, and a reader that took it
        // would write it into a ledger row and a log line.
        let prefix = "llm-1:tool:";
        assert_eq!(
            EffectId::parse(&format!("{prefix}{}", "c".repeat(MAX_EFFECT_ID_BYTES))),
            Err(EffectIdError::TooLong {
                bytes: prefix.len() + MAX_EFFECT_ID_BYTES
            })
        );
        assert_eq!(
            EffectId::parse("llm-1:tool:call\n1"),
            Err(EffectIdError::ControlCharacter)
        );
    }

    #[test]
    fn an_undeclared_retry_safety_reads_as_not_safe() {
        // The default that matters. A ledger row written by an older build, or
        // by a capability that never declared its safety, must not become
        // silently re-firable.
        let row: EffectLedgerEntry = serde_json::from_str(
            r#"{"effect_id":"llm-1:tool:call-1","iteration":1,"phase":"apply",
                "tool":"gmail__send","arguments_fingerprint":"fp-1","intent_at_ms":0}"#,
        )
        .expect("a row without retry_safety must still load");
        assert_eq!(row.retry_safety, RetrySafety::NotRetrySafe);

        // And a row that DOES declare its safety is read rather than defaulted.
        // Without this half the test passes against a field that stopped being
        // deserialized at all — every effect in the system pinned to the default,
        // with the assertion above still green.
        let declared: EffectLedgerEntry = serde_json::from_str(
            r#"{"effect_id":"llm-1:tool:call-1","iteration":1,"phase":"apply",
                "tool":"browser__screenshot","arguments_fingerprint":"fp-1",
                "intent_at_ms":0,"retry_safety":"retry_safe"}"#,
        )
        .expect("a row that declares its safety must load");
        assert_eq!(declared.retry_safety, RetrySafety::RetrySafe);
    }

    #[test]
    fn a_missing_result_never_licenses_a_re_fire_of_an_unsafe_effect() {
        // The core rule. The ledger's silence is not evidence: the intent commit
        // and the fire are two operations, and a worker that died between them
        // left no row for an effect that may well have happened.
        let ledger = EffectLedger::new();
        assert_eq!(
            ledger.disposition(&effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe)),
            EffectDisposition::Reconcile
        );
        assert_eq!(
            ledger.disposition(&effect("llm-1:tool:call-2", RetrySafety::RetrySafe)),
            EffectDisposition::Refire
        );
        assert_eq!(
            ledger.disposition(&effect("llm-1:tool:call-3", RetrySafety::Reattachable)),
            EffectDisposition::Reattach { reattach_ref: None }
        );
    }

    #[test]
    fn an_indeterminate_row_is_treated_as_no_answer_rather_than_as_a_result() {
        // The distinction the whole module rests on. A transport that ran and
        // could not say whether the effect landed has told us nothing we can
        // settle against, so it must route the same way an absent row does.
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::Indeterminate {
                    at_ms: 20,
                    reason: "the CDP connection dropped mid-click".to_string(),
                },
            )
            .expect("outcome");

        assert_eq!(ledger.disposition(&pending), EffectDisposition::Reconcile);
    }

    #[test]
    fn a_recorded_failure_is_an_answer_and_a_recorded_success_is_too() {
        for outcome in [
            EffectOutcome::Succeeded { at_ms: 20 },
            EffectOutcome::Failed {
                at_ms: 20,
                reason: "the provider refused it".to_string(),
            },
        ] {
            let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
            let mut ledger = EffectLedger::new();
            ledger
                .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
                .expect("intent");
            ledger
                .record_outcome(&pending.effect_id, outcome)
                .expect("outcome");
            assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);
        }
    }

    #[test]
    fn still_unknown_never_becomes_a_re_fire() {
        // Reading "we don't know" as "nothing left" re-sends a live message.
        // This is the mapping that must not be collapsed.
        assert_eq!(
            ReconciledEffect::from(&EffectReconciliation::StillUnknown {
                reason: "the act rests at Dispatching".to_string(),
            }),
            ReconciledEffect::SurfaceToUser {
                reason: "the act rests at Dispatching".to_string()
            }
        );
        assert_eq!(
            ReconciledEffect::from(&EffectReconciliation::DidNotFire {
                status: OutwardActStatus::Failed,
            }),
            ReconciledEffect::SafeToRefire
        );
        assert_eq!(
            ReconciledEffect::from(&EffectReconciliation::Fired {
                by_this_attempt: false,
                status: OutwardActStatus::Delivered,
            }),
            ReconciledEffect::AlreadyFired {
                by_this_attempt: false
            },
            "a byte-identical send from another attempt still means the effect exists"
        );
    }

    #[test]
    fn one_effect_id_may_not_be_pointed_at_two_dispatches() {
        let mut ledger = EffectLedger::new();
        let first = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        ledger
            .record_intent(EffectLedgerEntry::intent(&first, 1, Phase::Apply, 10))
            .expect("intent");

        // The same intent again, as a crashed phase re-running would produce.
        ledger
            .record_intent(EffectLedgerEntry::intent(&first, 1, Phase::Apply, 11))
            .expect("re-committing an identical intent is expected after a crash");

        let mut repointed = EffectLedgerEntry::intent(&first, 1, Phase::Apply, 12);
        repointed.tool = "slack__post".to_string();
        let error = ledger
            .record_intent(repointed)
            .expect_err("a reconciliation would ask about one tool and answer about another");
        assert!(
            matches!(error, EffectError::IntentConflict { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_re_committed_intent_does_not_erase_a_result_that_landed_meanwhile() {
        // The decision job writes its response independently of whether the
        // worker that issued it is alive. A crashed phase re-running its intent
        // commit must not wipe that response and cause a re-spend.
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Decide, 10))
            .expect("intent");
        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("outcome");

        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Decide, 30))
            .expect("a re-run's intent commit must be accepted");

        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .and_then(|e| e.outcome.clone()),
            Some(EffectOutcome::Succeeded { at_ms: 20 })
        );
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);
    }

    #[test]
    fn two_holders_may_not_settle_one_effect_differently() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("outcome");
        // The identical outcome again is idempotent.
        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("re-recording the same outcome is idempotent");

        let error = ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::Failed {
                    at_ms: 30,
                    reason: "no".to_string(),
                },
            )
            .expect_err("the version that arrives second is not more likely to be right");
        assert!(
            matches!(error, EffectError::OutcomeConflict { .. }),
            "got {error:?}"
        );
    }

    /// The asymmetry [`EffectLedger::record_outcome`] carries, in the direction
    /// that would be a lie.
    ///
    /// `NotDispatched` is the only outcome `disposition` TRUSTS — it answers
    /// `Refire` on the strength of it, for a `NotRetrySafe` effect, without
    /// asking the outward record anything. So writing it over a recorded
    /// `Succeeded` would turn a send that happened into positive evidence that
    /// nothing left, and nothing downstream could catch it. This is the guard,
    /// and the invariant is worth a test rather than a comment precisely because
    /// the permissive direction below is the one somebody will reach for.
    #[test]
    fn nothing_may_be_overwritten_by_never_dispatched() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("outcome");

        let error = ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 30,
                    reason: "the batch bailed".to_string(),
                },
            )
            .expect_err("a send that happened may not be relabelled as one that never left");
        assert!(
            matches!(error, EffectError::OutcomeConflict { .. }),
            "got {error:?}"
        );
        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .and_then(|e| e.outcome.clone()),
            Some(EffectOutcome::Succeeded { at_ms: 20 }),
            "the recorded success must survive the refused overwrite"
        );
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);
    }

    /// The permissive direction, and why it is not the same defect.
    ///
    /// `NotDispatched` is a statement about ONE attempt: this run of `Apply`
    /// admitted the effect and stopped before firing it. A later attempt that
    /// actually dispatched knows strictly more about the same effect id, so
    /// letting it write is one holder finishing the job rather than two holders
    /// disagreeing.
    #[test]
    fn a_later_attempt_that_actually_dispatched_supersedes_never_dispatched() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 20,
                    reason: "the batch bailed".to_string(),
                },
            )
            .expect("outcome");
        // Until something supersedes it, this is the one row that licenses a
        // fire without asking anybody — including for a `NotRetrySafe` effect.
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Refire);

        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 30 })
            .expect("the attempt that really dispatched knows more");
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);

        // And a DIFFERENT `NotDispatched` is still a conflict rather than a
        // silent reason-rewrite: the guard is on the offered value too.
        let mut second = EffectLedger::new();
        second
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        second
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 20,
                    reason: "the batch bailed".to_string(),
                },
            )
            .expect("outcome");
        let error = second
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 40,
                    reason: "a different story".to_string(),
                },
            )
            .expect_err("two reasons for one non-dispatch is still a conflict");
        assert!(
            matches!(error, EffectError::OutcomeConflict { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn stronger_resolution_evidence_supersedes_an_indeterminate_outcome() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::Indeterminate {
                    at_ms: 20,
                    reason: "the transport disconnected after dispatch".to_string(),
                },
            )
            .expect("known unknown");

        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 30 })
            .expect("an explicit adoption is stronger evidence than a known unknown");

        assert_eq!(ledger.disposition(&pending), EffectDisposition::Adopt);
        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .and_then(|entry| entry.outcome.clone()),
            Some(EffectOutcome::Succeeded { at_ms: 30 })
        );
    }

    #[test]
    fn a_refire_rearms_never_dispatched_before_the_new_attempt_can_fire() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(
                &pending.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 20,
                    reason: "the first attempt stopped before this member".to_string(),
                },
            )
            .expect("not dispatched");
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Refire);

        // This is the new attempt's pre-fire intent write. If the worker dies
        // after the actual send, recovery must see unknown/reconcile — never the
        // stale proof that the *previous* attempt did not send.
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 30))
            .expect("re-arm intent");
        let row = ledger.get(&pending.effect_id).expect("row");
        assert_eq!(row.outcome, None);
        assert!(row.prepared_only);
        assert_eq!(ledger.disposition(&pending), EffectDisposition::Reconcile);
    }

    /// What `unsettled` answers, over every variant, so the definition and the
    /// resume cannot drift apart.
    ///
    /// `driver_worker::resolve_effects` iterates exactly this, so a variant that
    /// is wrongly excluded is an effect that may have fired and is never asked
    /// about — and one wrongly included holds a run that had its answer.
    #[test]
    fn unsettled_is_no_row_or_indeterminate_and_nothing_else() {
        let none = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let indeterminate = effect("llm-1:tool:call-2", RetrySafety::NotRetrySafe);
        let succeeded = effect("llm-1:tool:call-3", RetrySafety::NotRetrySafe);
        let failed = effect("llm-1:tool:call-4", RetrySafety::NotRetrySafe);
        let not_dispatched = effect("llm-1:tool:call-5", RetrySafety::NotRetrySafe);

        let mut ledger = EffectLedger::new();
        for pending in [&none, &indeterminate, &succeeded, &failed, &not_dispatched] {
            ledger
                .record_intent(EffectLedgerEntry::intent(pending, 1, Phase::Apply, 10))
                .expect("intent");
        }
        ledger
            .record_outcome(
                &indeterminate.effect_id,
                EffectOutcome::Indeterminate {
                    at_ms: 20,
                    reason: "the transport went away".to_string(),
                },
            )
            .expect("outcome");
        ledger
            .record_outcome(&succeeded.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("outcome");
        ledger
            .record_outcome(
                &failed.effect_id,
                EffectOutcome::Failed {
                    at_ms: 20,
                    reason: "the tool said no".to_string(),
                },
            )
            .expect("outcome");
        ledger
            .record_outcome(
                &not_dispatched.effect_id,
                EffectOutcome::NotDispatched {
                    at_ms: 20,
                    reason: "the batch bailed".to_string(),
                },
            )
            .expect("outcome");

        let unsettled: Vec<&str> = ledger
            .unsettled()
            .map(|entry| entry.effect_id.as_str())
            .collect();
        assert_eq!(
            unsettled,
            vec!["llm-1:tool:call-1", "llm-1:tool:call-2"],
            "a row with no outcome and a recorded `Indeterminate` are unsettled; \
             `Succeeded`, `Failed` and `NotDispatched` are answers"
        );
    }

    #[test]
    fn settling_an_effect_nobody_declared_is_refused() {
        let mut ledger = EffectLedger::new();
        let error = ledger
            .record_outcome(
                &EffectId::parse("llm-1:tool:call-1").expect("well-formed"),
                EffectOutcome::Succeeded { at_ms: 1 },
            )
            .expect_err("there is nothing to settle against");
        assert!(
            matches!(error, EffectError::UnknownEffect { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_later_declaration_cannot_widen_what_the_recorded_intent_narrowed() {
        // The fail-open this closes. A capability's declared safety can change
        // between the intent commit and the resume, and the resumed worker may
        // be running different configuration from the one that fired. Trusting
        // the caller's copy would re-fire an effect that was recorded as unsafe
        // when it actually left.
        let recorded = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&recorded, 1, Phase::Apply, 10))
            .expect("intent");

        let now_claimed_safe = PendingEffect {
            retry_safety: RetrySafety::RetrySafe,
            ..recorded.clone()
        };
        assert_eq!(
            ledger.disposition(&now_claimed_safe),
            EffectDisposition::Reconcile,
            "the recorded declaration is what was true when the effect left"
        );

        // And the rule is symmetric: a caller holding the stricter view wins too.
        let recorded_safe = effect("llm-1:tool:call-2", RetrySafety::RetrySafe);
        ledger
            .record_intent(EffectLedgerEntry::intent(
                &recorded_safe,
                1,
                Phase::Apply,
                10,
            ))
            .expect("intent");
        let now_claimed_unsafe = PendingEffect {
            retry_safety: RetrySafety::NotRetrySafe,
            ..recorded_safe.clone()
        };
        assert_eq!(
            ledger.disposition(&now_claimed_unsafe),
            EffectDisposition::Reconcile
        );

        assert_eq!(
            RetrySafety::stricter(RetrySafety::RetrySafe, RetrySafety::RetrySafe),
            RetrySafety::RetrySafe,
            "two safe declarations stay safe, or nothing could ever be re-fired"
        );
        assert_eq!(
            RetrySafety::stricter(RetrySafety::RetrySafe, RetrySafety::Reattachable),
            RetrySafety::Reattachable
        );
    }

    #[test]
    fn a_blank_call_id_is_refused_even_when_a_step_suffix_hides_it() {
        // `llm-1:tool::step:3` splits into a non-empty tail, so a naive check
        // accepts it — and the segment that is supposed to say WHICH of the
        // turn's tool calls this is says nothing.
        assert_eq!(
            EffectId::parse("llm-1:tool::step:3"),
            Err(EffectIdError::EmptyHalf {
                half: "model_tool_call_id"
            })
        );
        assert!(EffectId::parse("llm-1:tool:call-1:step:3").is_ok());
    }

    #[test]
    fn a_batch_plan_is_per_effect_because_parallel_members_settle_out_of_order() {
        let batch = PendingBatch {
            iteration: 3,
            phase: Phase::Apply,
            mode: BatchMode::Parallel { admitted: 2 },
            effects: vec![
                effect("llm-1:tool:call-1", RetrySafety::RetrySafe),
                effect("llm-1:tool:call-2", RetrySafety::RetrySafe),
                effect("llm-1:tool:call-3", RetrySafety::NotRetrySafe),
            ],
        };
        batch.validate().expect("a well-formed batch");

        let mut ledger = EffectLedger::new();
        // The SECOND member settled and the first did not — the out-of-order
        // case a cursor cannot represent.
        ledger
            .record_intent(EffectLedgerEntry::intent(
                &batch.effects[1],
                3,
                Phase::Apply,
                10,
            ))
            .expect("intent");
        ledger
            .record_outcome(
                &batch.effects[1].effect_id,
                EffectOutcome::Succeeded { at_ms: 20 },
            )
            .expect("outcome");

        let plan = ledger.plan(&batch);
        assert_eq!(plan[0].1, EffectDisposition::Refire);
        assert_eq!(plan[1].1, EffectDisposition::Adopt);
        assert_eq!(plan[2].1, EffectDisposition::Reconcile);
    }

    #[test]
    fn a_batch_off_the_wire_is_unchecked_until_validate_refuses_it() {
        let encoded = serde_json::json!({
            "iteration": 1,
            "phase": "apply",
            "mode": { "mode": "parallel", "admitted": 9 },
            "effects": [{
                "effect_id": "llm-1:tool:call-1",
                "tool": "browser__click",
                "arguments_fingerprint": "fp",
                "retry_safety": "not_retry_safe"
            }]
        });
        let batch: PendingBatch = serde_json::from_value(encoded).expect("it deserializes");
        // Deserializing is not validating. Nothing on this type refuses the row
        // on the way in — the stores are what call `validate` on load
        // (`store::fs::load_sync` and both `append_journal`s), and this is the
        // shape they are refusing on their callers' behalf.
        let error = batch
            .validate()
            .expect_err("a parallel slice cannot exceed the batch it slices");
        assert!(
            matches!(error, EffectError::ParallelSliceOverruns { .. }),
            "got {error:?}"
        );

        let empty = PendingBatch {
            iteration: 1,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects: Vec::new(),
        };
        assert_eq!(empty.validate(), Err(EffectError::EmptyBatch));

        // The count ceiling, which nothing else exercises: a row that grew
        // somewhere else is what would otherwise fan a worker out past its own
        // membership.
        let too_many = PendingBatch {
            iteration: 1,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects: (0..=MAX_BATCH_EFFECTS)
                .map(|index| {
                    effect(
                        &format!("llm-1:tool:call-{index}"),
                        RetrySafety::NotRetrySafe,
                    )
                })
                .collect(),
        };
        assert_eq!(
            too_many.validate(),
            Err(EffectError::BatchTooLarge {
                count: MAX_BATCH_EFFECTS + 1,
                limit: MAX_BATCH_EFFECTS,
            })
        );

        // And two members under one id, which would leave a worker unable to say
        // which of them a settlement belonged to.
        let collided = PendingBatch {
            iteration: 1,
            phase: Phase::Apply,
            mode: BatchMode::Sequential,
            effects: vec![
                effect("llm-1:tool:call-1", RetrySafety::RetrySafe),
                effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe),
            ],
        };
        assert_eq!(
            collided.validate(),
            Err(EffectError::DuplicateEffectId {
                effect_id: "llm-1:tool:call-1".to_string(),
            })
        );
    }

    #[test]
    fn a_re_committed_intent_does_not_erase_the_handle_the_effect_resumes_by() {
        // The failure this closes: a row names the coding invocation this effect
        // reattaches through, the worker dies, the phase re-runs and re-commits
        // its intent, and the ref is gone. The disposition still says "reattach"
        // — to nothing — so the engine silently starts a fresh session on a repo
        // the previous one was half way through.
        //
        // The gate now mints the same ref on every attempt, so a re-commit
        // carries one rather than arriving empty; this fixture drops it
        // deliberately, because the guard has to hold for a row that was written
        // before the mint existed too.
        let pending = effect("llm-1:tool:call-1", RetrySafety::Reattachable);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_reattach_ref(&pending.effect_id, "session-abc")
            .expect("the job reported its session");

        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 20))
            .expect("the re-running phase re-commits its intent");

        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reattach {
                reattach_ref: Some("session-abc".to_string())
            }
        );

        // And a re-run that DOES know a session replaces the recorded one, since
        // the newer attach is the live one.
        let mut newer = EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 30);
        newer.reattach_ref = Some("session-def".to_string());
        ledger.record_intent(newer).expect("intent");
        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reattach {
                reattach_ref: Some("session-def".to_string())
            }
        );
    }

    #[test]
    fn a_re_committed_intent_cannot_widen_the_safety_the_first_one_recorded() {
        // The door left open beside the one `disposition` closes. Taking the
        // stricter of the recorded and the offered declaration buys nothing if
        // the re-commit overwrites the recorded half on its way past: a worker
        // running a later build, where the capability now declares itself
        // read-only, would turn a send committed as unsafe into one it may fire
        // again — without ever asking the outward record.
        let recorded = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&recorded, 1, Phase::Apply, 10))
            .expect("intent");

        let now_claimed_safe = PendingEffect {
            retry_safety: RetrySafety::RetrySafe,
            ..recorded.clone()
        };
        ledger
            .record_intent(EffectLedgerEntry::intent(
                &now_claimed_safe,
                1,
                Phase::Apply,
                20,
            ))
            .expect("a re-run's intent commit is still accepted");

        assert_eq!(
            ledger.get(&recorded.effect_id).map(|row| row.retry_safety),
            Some(RetrySafety::NotRetrySafe),
            "the row keeps what was true when the effect left"
        );
        assert_eq!(
            ledger.disposition(&now_claimed_safe),
            EffectDisposition::Reconcile
        );

        // Narrowing on a re-commit is the direction that is allowed: a build that
        // has learned the effect is unsafe may say so.
        let narrowing = effect("llm-1:tool:call-2", RetrySafety::RetrySafe);
        ledger
            .record_intent(EffectLedgerEntry::intent(&narrowing, 1, Phase::Apply, 10))
            .expect("intent");
        let now_unsafe = PendingEffect {
            retry_safety: RetrySafety::NotRetrySafe,
            ..narrowing.clone()
        };
        ledger
            .record_intent(EffectLedgerEntry::intent(&now_unsafe, 1, Phase::Apply, 20))
            .expect("intent");
        assert_eq!(
            ledger.get(&narrowing.effect_id).map(|row| row.retry_safety),
            Some(RetrySafety::NotRetrySafe)
        );
    }

    #[test]
    fn a_reattachable_effect_resumes_by_its_session_rather_than_re_firing() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::Reattachable);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reattach { reattach_ref: None }
        );

        ledger
            .record_reattach_ref(&pending.effect_id, "session-abc")
            .expect("the job reported its session");
        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reattach {
                reattach_ref: Some("session-abc".to_string())
            }
        );
    }

    /// The ordinary path: the ref is on the INTENT, so it is on the row before
    /// anything fires.
    ///
    /// This is what makes the rule reach the case it exists for. `record_intent`
    /// runs before the dispatch, so a worker that dies mid-job leaves a row that
    /// already names the invocation — where a ref written after the job returned
    /// would be absent in exactly that case and present only when the outcome
    /// made it redundant.
    #[test]
    fn the_intent_carries_the_gates_reattach_ref_before_anything_fires() {
        let pending = PendingEffect {
            reattach_ref: Some("cinv-minted-by-the-gate".to_string()),
            ..effect("llm-1:tool:code-1", RetrySafety::Reattachable)
        };
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10))
            .expect("intent");
        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .and_then(|row| row.reattach_ref.as_deref()),
            Some("cinv-minted-by-the-gate"),
            "the intent is the only write that happens before the fire"
        );
        assert_eq!(
            ledger.disposition(&pending),
            EffectDisposition::Reattach {
                reattach_ref: Some("cinv-minted-by-the-gate".to_string())
            }
        );

        // And the row a resume rebuilds from the ledger alone carries it too —
        // `EffectLedgerEntry::pending` is total or a ledger-only resume answers
        // a question about a different dispatch.
        assert_eq!(
            ledger
                .get(&pending.effect_id)
                .expect("the row")
                .pending()
                .reattach_ref
                .as_deref(),
            Some("cinv-minted-by-the-gate")
        );
    }

    /// A batch in hand can name the invocation when the ledger cannot.
    ///
    /// The case is a host that does not split its `Apply`: it writes no intents
    /// at all, so the ledger is silent about an effect that really did dispatch,
    /// and `commit_failed_attempt` publishes the batch as the only record. The
    /// recorded value still wins where there is one — that is the same rule
    /// `retry_safety` follows, for the same reason.
    #[test]
    fn a_reattach_ref_falls_back_to_the_batch_and_the_recorded_one_still_wins() {
        let offered = PendingEffect {
            reattach_ref: Some("cinv-from-the-batch".to_string()),
            ..effect("llm-1:tool:code-1", RetrySafety::Reattachable)
        };
        let ledger = EffectLedger::new();
        assert_eq!(
            ledger.disposition(&offered),
            EffectDisposition::Reattach {
                reattach_ref: Some("cinv-from-the-batch".to_string())
            },
            "a run with no row still knows which job it gated"
        );

        let mut ledger = EffectLedger::new();
        let recorded = PendingEffect {
            reattach_ref: Some("cinv-from-the-row".to_string()),
            ..offered.clone()
        };
        ledger
            .record_intent(EffectLedgerEntry::intent(&recorded, 1, Phase::Apply, 10))
            .expect("intent");
        assert_eq!(
            ledger.disposition(&offered),
            EffectDisposition::Reattach {
                reattach_ref: Some("cinv-from-the-row".to_string())
            },
            "what was written down when the effect left is the authority"
        );
    }

    #[test]
    fn a_ledger_row_filed_under_the_wrong_key_never_loads() {
        // The map key and the row's own id are two statements of one fact, and a
        // file where they disagree is not cosmetic: `get` finds a row by key, so
        // a row filed under the wrong one is invisible to every disposition
        // while `entries` still reports it. The effect below SUCCEEDED — and
        // read through the wrong key it presents as an effect with no result,
        // which for a retry-safe declaration licenses firing it again.
        let row = serde_json::json!({
            "effect_id": "llm-1:tool:call-1",
            "iteration": 1,
            "phase": "apply",
            "tool": "gmail__send",
            "arguments_fingerprint": "fp-1",
            "intent_at_ms": 0,
            "outcome": { "outcome": "succeeded", "at_ms": 20 }
        });

        let honest: EffectLedger =
            serde_json::from_value(serde_json::json!({ "llm-1:tool:call-1": row.clone() }))
                .expect("a map whose keys agree with its rows loads");
        assert_eq!(
            honest.disposition(&effect("llm-1:tool:call-1", RetrySafety::RetrySafe)),
            EffectDisposition::Adopt
        );

        let error = serde_json::from_value::<EffectLedger>(
            serde_json::json!({ "llm-1:tool:someone-elses-call": row }),
        )
        .expect_err("a row no lookup could ever find must not load at all");
        assert!(error.to_string().contains("filed under"), "{error}");
    }

    #[test]
    fn two_rows_under_one_effect_id_are_a_lost_row_rather_than_a_repointing() {
        // Both rows name the same tool, so calling this an `IntentConflict` would
        // print "recorded against gmail__send and was re-offered against
        // gmail__send" and send an operator looking for a mismatch that is not
        // there.
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let error = EffectLedger::from_entries(vec![
            EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10),
            EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 20),
        ])
        .expect_err("a map cannot hold two rows under one key");
        assert_eq!(
            error,
            EffectError::DuplicateEffectId {
                effect_id: "llm-1:tool:call-1".to_string()
            }
        );

        // A genuine repointing still reads as one.
        let mut repointed = EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 10);
        repointed.tool = "slack__post".to_string();
        let mut ledger = EffectLedger::from_entries(vec![EffectLedgerEntry::intent(
            &pending,
            1,
            Phase::Apply,
            10,
        )])
        .expect("one row loads");
        assert!(matches!(
            ledger.record_intent(repointed),
            Err(EffectError::IntentConflict { .. })
        ));
    }

    #[test]
    fn intent_reconcile_refs_only_fill_absence_and_equal_the_recorded_act() {
        let mut committed = effect("llm-1:tool:call-1", RetrySafety::RetrySafe);
        committed.reconcile_ref = Some(act_ref('a'));
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&committed, 1, Phase::Apply, 10))
            .expect("initial intent");

        let mut omitted = committed.clone();
        omitted.reconcile_ref = None;
        ledger
            .record_intent(EffectLedgerEntry::intent(&omitted, 1, Phase::Apply, 20))
            .expect("an older worker may omit, but may not erase, the committed ref");
        assert_eq!(
            ledger
                .get(&committed.effect_id)
                .and_then(|row| row.reconcile_ref.as_ref()),
            committed.reconcile_ref.as_ref(),
        );
        assert_eq!(
            ledger.disposition(&omitted),
            EffectDisposition::Reconcile,
            "the recorded ref must outrank an older batch omission even when the capability \
             declares the effect retry-safe"
        );

        let mut conflicting = committed.clone();
        conflicting.reconcile_ref = Some(act_ref('b'));
        assert!(matches!(
            ledger.record_intent(EffectLedgerEntry::intent(&conflicting, 1, Phase::Apply, 30,)),
            Err(EffectError::ReconcileRefConflict { .. })
        ));
        assert_eq!(
            ledger
                .get(&committed.effect_id)
                .and_then(|row| row.reconcile_ref.as_ref()),
            committed.reconcile_ref.as_ref(),
            "a conflicting offer must not mutate the authoritative row"
        );

        let legacy = effect("llm-1:tool:call-2", RetrySafety::NotRetrySafe);
        ledger
            .record_intent(EffectLedgerEntry::intent(&legacy, 1, Phase::Apply, 10))
            .expect("legacy intent without a ref");
        ledger
            .record_outcome(&legacy.effect_id, EffectOutcome::Succeeded { at_ms: 15 })
            .expect("settled legacy row");
        let mut enriched = legacy.clone();
        enriched.reconcile_ref = Some(act_ref('c'));
        ledger
            .record_intent(EffectLedgerEntry::intent(&enriched, 1, Phase::Apply, 20))
            .expect("a newer gate may monotonically fill a missing ref");
        assert_eq!(
            ledger
                .get(&legacy.effect_id)
                .and_then(|row| row.reconcile_ref.as_ref()),
            enriched.reconcile_ref.as_ref(),
            "the equality/monotonicity check also runs before the settled-row early return"
        );
    }

    #[test]
    fn a_re_derivation_that_does_not_reproduce_the_committed_arguments_is_refused() {
        // The gate that makes option A safe. A worker re-derives its arguments
        // from the assistant turn, and a re-derivation that drifts is otherwise
        // silent — a tool call has no opinion about arguments that are merely
        // DIFFERENT from the ones a previous attempt used. Without this the
        // stateless driver can send a mail whose body nobody approved.
        let committed = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&committed, 1, Phase::Apply, 10))
            .expect("intent");

        ledger
            .authorize_refire(&committed, "gmail__send", "fp-1")
            .expect("a re-derivation that reproduces the commitment fires");

        let drifted = ledger
            .authorize_refire(&committed, "gmail__send", "fp-2")
            .expect_err("a re-derivation that drifts is not the dispatch this id names");
        assert!(
            matches!(drifted, EffectError::ArgumentsConflict { .. }),
            "got {drifted:?}"
        );
        // Named as an arguments mismatch rather than a repointing: the tools
        // agree, so `IntentConflict`'s message would print `gmail__send` in both
        // halves and send an operator hunting a mismatch that is not there.
        let rendered = drifted.to_string();
        assert!(
            rendered.contains("fp-1") && rendered.contains("fp-2"),
            "{rendered}"
        );

        let repointed = ledger
            .authorize_refire(&committed, "slack__post", "fp-1")
            .expect_err("and a different tool is still a repointing");
        assert!(
            matches!(repointed, EffectError::IntentConflict { .. }),
            "got {repointed:?}"
        );
    }

    #[test]
    fn the_re_fire_gate_still_holds_when_the_intent_commit_left_no_ledger_row() {
        // The case a row-only check would miss, and it is the case the whole
        // stateless design is about: a worker that died between committing its
        // intent and firing left no row at all, so the committed PendingEffect
        // is the only statement of what was authorised. Checking only
        // `self.get(..)` here would return Ok for any re-derivation whatsoever
        // on exactly that path.
        let committed = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let ledger = EffectLedger::new();
        assert!(
            ledger.get(&committed.effect_id).is_none(),
            "the fixture must be the no-row case for this test to mean anything"
        );

        ledger
            .authorize_refire(&committed, "gmail__send", "fp-1")
            .expect("the commitment itself still licenses a faithful re-derivation");
        assert!(matches!(
            ledger.authorize_refire(&committed, "gmail__send", "fp-2"),
            Err(EffectError::ArgumentsConflict { .. })
        ));
    }

    #[test]
    fn two_absent_fingerprints_do_not_agree_with_each_other() {
        // The `Gate` commits `""` for a dispatch it cannot fingerprint — a turn
        // with no trace receipt, a call the model gave no id, a scoped HMAC key
        // that will not load — and `WorkerHost::rederive_dispatch` answers the
        // same way for the same reasons. `refuse_unless_same_dispatch` compares
        // strings, so without the guard `"" == ""` licenses the re-fire: two
        // absences agree with each other and a live send goes out a second time
        // on the strength of nothing having been recorded on either side.
        let mut unfingerprinted = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        unfingerprinted.arguments_fingerprint = String::new();
        let ledger = EffectLedger::new();
        assert!(matches!(
            ledger.authorize_refire(&unfingerprinted, "gmail__send", ""),
            Err(EffectError::UnfingerprintedRefire { .. })
        ));
        // The other side alone is enough: a worker that could not fingerprint
        // its re-derivation has not reproduced the commitment either.
        assert!(matches!(
            ledger.authorize_refire(&unfingerprinted, "gmail__send", "fp-1"),
            Err(EffectError::UnfingerprintedRefire { .. })
        ));
        let committed = effect("llm-1:tool:call-2", RetrySafety::NotRetrySafe);
        assert!(matches!(
            ledger.authorize_refire(&committed, "gmail__send", ""),
            Err(EffectError::UnfingerprintedRefire { .. })
        ));

        // And a real pair still authorises, or the guard has eaten the case it
        // exists to leave alone — a refusal that refuses everything passes the
        // three assertions above without protecting anything.
        ledger
            .authorize_refire(&committed, "gmail__send", "fp-1")
            .expect("two real fingerprints that agree still license a re-fire");

        // The intent commit is deliberately NOT refused for the same row. An
        // unfingerprintable dispatch has to be *recorded* — refusing here would
        // refuse the dispatch itself, at a chokepoint every caller passes
        // through, to stop one caller's interpretation.
        //
        // Recorded TWICE, on purpose, and that is the whole value of these two
        // statements. `record_intent` reaches `refuse_unless_same_dispatch` only
        // inside its `if let Some(existing)` branch, so a single call to a fresh
        // ledger never touches the shared helper at all — an earlier cut of this
        // test made exactly one call and therefore stayed green under the one
        // regression it is named against. The second call is the one that lands
        // on `"" vs ""` inside the helper, which is where moving the
        // `UnfingerprintedRefire` guard out of `authorize_refire` would put it.
        // Move the guard and this `expect` fires.
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(
                &unfingerprinted,
                1,
                Phase::Apply,
                0,
            ))
            .expect("an unfingerprintable dispatch is still recordable");
        assert!(
            ledger.get(&unfingerprinted.effect_id).is_some(),
            "the second commit below must take the existing-row branch, or it tests nothing"
        );
        ledger
            .record_intent(EffectLedgerEntry::intent(
                &unfingerprinted,
                1,
                Phase::Apply,
                0,
            ))
            .expect(
                "re-committing the same unfingerprintable intent goes through \
                 `refuse_unless_same_dispatch`, and two absent fingerprints agreeing is correct \
                 THERE — the row is the same row. The refusal belongs to the re-fire, which is \
                 the only caller that turns a match into a licence to send",
            );
    }

    #[test]
    fn a_committed_act_ref_is_not_itself_a_licence_to_fire() {
        // The two licences, kept apart. The act ref licenses ASKING the outward
        // record; only that record's positive answer licenses a re-fire. A
        // `disposition` that shortcut to `Refire` because the row carried a ref
        // — or an `authorize_refire` that passed because it did — would re-send
        // a live message without ever reading one.
        let mut outward = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        outward.reconcile_ref = Some(act_ref('a'));
        let ledger = EffectLedger::new();
        assert_eq!(ledger.disposition(&outward), EffectDisposition::Reconcile);
        assert!(matches!(
            ledger.authorize_refire(&outward, "gmail__send", "fp-2"),
            Err(EffectError::ArgumentsConflict { .. })
        ));
    }

    #[test]
    fn a_retry_safe_declaration_cannot_make_an_outward_dispatch_fire_on_sight() {
        // The hole beside the one the whole module is about. Everything here
        // guards the path that reads the outward record and gets the answer
        // wrong; this is the path that never reads it. A `RetrySafe`
        // declaration answers `Refire` immediately, and an effect carrying a
        // committed act ref is by construction one the outward classifier said
        // can reach somebody — so the two together re-send a live message with
        // no evidence consulted at all.
        //
        // They can disagree: the safety comes from a capability's declaration
        // and the outward verdict also reads the arguments, which is why
        // `outward_dispatch_class` exists — a `raw` passthrough turns a
        // capability that declares itself read-only into a real send.
        let mut outward = effect("llm-1:tool:call-1", RetrySafety::RetrySafe);
        outward.reconcile_ref = Some(act_ref('d'));
        let ledger = EffectLedger::new();
        assert_eq!(
            ledger.disposition(&outward),
            EffectDisposition::Reconcile,
            "a committed act ref outranks a retry-safe declaration"
        );

        // The narrowing is exactly one declaration wide. Without this half,
        // folding `reconcile_ref` in through `stricter` — which maps
        // `Reattachable` to `NotRetrySafe` too — would pass the assertion above
        // while replacing a coding job's resume with a reconciliation, whose
        // `DidNotFire` runs the job a second time against a real repository.
        let mut reattachable = effect("llm-1:tool:call-2", RetrySafety::Reattachable);
        reattachable.reconcile_ref = Some(act_ref('e'));
        assert_eq!(
            ledger.disposition(&reattachable),
            EffectDisposition::Reattach { reattach_ref: None }
        );

        // And a retry-safe effect with no ref still fires, or the narrowing has
        // eaten the case it was supposed to leave alone.
        assert_eq!(
            ledger.disposition(&effect("llm-1:tool:call-3", RetrySafety::RetrySafe)),
            EffectDisposition::Refire
        );
    }

    #[test]
    fn a_committed_act_ref_survives_the_wire_and_an_absent_one_is_not_a_parse_failure() {
        // The field only removes the re-derivation if it actually crosses the
        // boundary. A ref that did not survive puts the re-derivation back —
        // and with it the silent false-`DidNotFire` that re-sends live mail.
        let mut outward = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let committed = act_ref('b');
        outward.reconcile_ref = Some(committed.clone());
        let encoded = serde_json::to_string(&outward).expect("encode");
        let decoded: PendingEffect = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.reconcile_ref.as_ref(), Some(&committed));
        // Named individually, because a `PartialEq` on the whole value would go
        // green if the scope halves had never been serialised at all — both
        // sides would simply be missing them, and the ref would be back to the
        // bare string that cannot name its own directory.
        let decoded_ref = decoded.reconcile_ref.as_ref().expect("a ref survived");
        assert_eq!(
            decoded_ref.act_ref_in_scope(FIXTURE_PRINCIPAL, FIXTURE_WORKSPACE),
            Some(raw_act_ref('b').as_str())
        );
        assert_eq!(decoded_ref.principal(), FIXTURE_PRINCIPAL);
        assert_eq!(decoded_ref.workspace(), FIXTURE_WORKSPACE);
        assert!(encoded.contains(FIXTURE_PRINCIPAL), "{encoded}");
        assert!(encoded.contains(FIXTURE_WORKSPACE), "{encoded}");

        // The refusal this type exists for, at the value level: a pickup under
        // any other scope gets nothing to address the record with. Before the
        // scope rode along, the same pickup rebuilt the root from its own
        // context, read an empty directory, and reported `DidNotFire` —
        // positive evidence nothing was sent.
        assert_eq!(
            decoded_ref.act_ref_in_scope("someone-else", FIXTURE_WORKSPACE),
            None
        );
        assert_eq!(
            decoded_ref.act_ref_in_scope(FIXTURE_PRINCIPAL, "another-workspace"),
            None
        );

        // The common case carries nothing, and a row that carries nothing must
        // load rather than fail: `None` is "not outward", which is most
        // dispatches. Paired with the assertion above so that a field which
        // stopped being deserialized at all would fail this test rather than
        // pass it twice.
        let not_outward = effect("llm-1:tool:call-2", RetrySafety::RetrySafe);
        let encoded = serde_json::to_string(&not_outward).expect("encode");
        assert!(!encoded.contains("reconcile_ref"), "{encoded}");
        let decoded: PendingEffect = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.reconcile_ref, None);
    }

    #[test]
    fn an_act_ref_that_is_not_the_derived_shape_never_loads() {
        // This value addresses a FILE: `OutwardAssertionStore::load_act` joins
        // it into the scope root unchecked. Until this field existed every act
        // ref reaching that join was minted in-process by `derive_act_ref`;
        // this one arrives off the wire.
        //
        // An escape is already refused one layer down —
        // `ArtifactV2Workspace::provider_relative_path_for_resolved_path`
        // rejects any `ParentDir` component — and that is not the case this
        // check is for. The case it is for stays *inside* the tree: an
        // arbitrary in-scope ref names some other act's record, or none, and
        // `reconciliation_from_act` reads no record as `DidNotFire`. So a ref
        // nobody derived does not fail; it answers, wrongly, that a message was
        // never sent.
        let row = |reconcile_ref: serde_json::Value| {
            serde_json::json!({
                "effect_id": "llm-1:tool:call-1",
                "tool": "gmail__send",
                "arguments_fingerprint": "fp-1",
                "retry_safety": "not_retry_safe",
                "reconcile_ref": reconcile_ref,
            })
        };
        let scoped = |act_ref: &str| {
            serde_json::json!({
                "act_ref": act_ref,
                "principal": FIXTURE_PRINCIPAL,
                "workspace": FIXTURE_WORKSPACE,
            })
        };

        let short = format!(
            "{RECONCILE_REF_PREFIX}{}",
            "a".repeat(RECONCILE_REF_DIGEST_HEX - 1)
        );
        let upper = format!(
            "{RECONCILE_REF_PREFIX}{}",
            "A".repeat(RECONCILE_REF_DIGEST_HEX)
        );
        let not_hex = format!(
            "{RECONCILE_REF_PREFIX}{}",
            "g".repeat(RECONCILE_REF_DIGEST_HEX)
        );
        for bad in [
            "../../../../etc/passwd",
            "act-../../../../etc/passwd",
            short.as_str(),
            upper.as_str(),
            not_hex.as_str(),
            "",
        ] {
            assert!(
                serde_json::from_value::<PendingEffect>(row(scoped(bad))).is_err(),
                "{bad} must not load as an act ref"
            );
        }

        // And the derived shape still does, or the field would be inert.
        let good = act_ref('c');
        let good_raw = raw_act_ref('c');
        let loaded: PendingEffect =
            serde_json::from_value(row(scoped(&good_raw))).expect("a derived ref must load");
        assert_eq!(loaded.reconcile_ref, Some(good.clone()));

        // A ref that lost either half of its scope must not load either. This
        // is the half the shape check cannot see: `act-<32 hex>` is perfectly
        // well-formed whether or not anybody recorded which directory it lives
        // in, and a row that loaded without one would send a worker looking in
        // whatever scope it happened to be running under — the absence that
        // reads as `DidNotFire`.
        for missing in ["principal", "workspace", "act_ref"] {
            let mut incomplete = scoped(&good_raw);
            incomplete
                .as_object_mut()
                .expect("the fixture is an object")
                .remove(missing);
            assert!(
                serde_json::from_value::<PendingEffect>(row(incomplete)).is_err(),
                "a committed ref missing {missing} must not load"
            );
        }

        // The in-process half of this bound is no longer a check at all — it is
        // the type. `CommittedActRef`'s fields are private and both ways in run
        // `validate_reconcile_ref`, so there is no `PendingEffect` to build
        // whose ref is malformed. Asserting on `PendingBatch::validate` here
        // would need a value nothing can construct; the constructor is the
        // statement, so the constructor is what is asserted on.
        assert!(matches!(
            CommittedActRef::new("act-not-a-digest", FIXTURE_PRINCIPAL, FIXTURE_WORKSPACE),
            Err(EffectError::MalformedReconcileRef { .. })
        ));
    }

    #[test]
    fn the_ledger_round_trips_as_a_status_map() {
        let pending = effect("llm-1:tool:call-1", RetrySafety::NotRetrySafe);
        let mut ledger = EffectLedger::new();
        ledger
            .record_intent(EffectLedgerEntry::intent(&pending, 2, Phase::Apply, 10))
            .expect("intent");
        ledger
            .record_outcome(&pending.effect_id, EffectOutcome::Succeeded { at_ms: 20 })
            .expect("outcome");

        let encoded = serde_json::to_string(&ledger).expect("encode");
        let decoded: EffectLedger = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, ledger);
        assert_eq!(decoded.len(), 1);
        assert!(!decoded.is_empty());
        assert_eq!(decoded.entries().count(), 1);
    }
}
