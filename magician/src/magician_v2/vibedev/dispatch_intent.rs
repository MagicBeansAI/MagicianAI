//! Durable dispatch intents — what makes admitting a VibeDev run idempotent
//! and recoverable.
//!
//! Starting a VibeDev run is expensive and long: a build can occupy an engineer
//! for hours. Creating the task and dispatching it are two writes, so without a
//! record between them two things go wrong. A retried call creates a *second*
//! task and a second multi-hour build; a crash between the two leaves a task
//! nobody will ever start. Both are the same missing thing — a durable record
//! that says "this request was admitted, and here is the run it became".
//!
//! ## The shape, and where it came from
//!
//! This deliberately reuses the verification controller's durability shape
//! (`execution/verification/{store,journal}.rs`), which solves the same problem
//! for code-verification gates:
//!
//! * an append-only **journal** is the source of truth, committed with a single
//!   atomic rename, so a crash either leaves the whole record or none of it;
//! * `intents/` and `live/` are **projections** — caches of the journal, always
//!   repairable by replay, never the other way round;
//! * `live/` is the **outbox**: an intent appears there while it is unfinished
//!   and is retired when it settles, so restart recovery costs O(unfinished)
//!   rather than O(every request ever admitted);
//! * a monotonic **generation** fences a stalled writer out of committing over
//!   its replacement, re-checked *under the lock* against the stored record.
//!
//! Three things are deliberately different, and the differences are the
//! interesting part:
//!
//! 1. **No TTL lease.** A verification pass runs for minutes and renews, so it
//!    needs an expiry a replacement can wait out. A dispatch claim spans one
//!    `create` plus one `start_execution` and never outlives its process — so
//!    any claim from *another* process is by definition abandoned, and there is
//!    nothing for a TTL to tell us that the previous process's death has not
//!    already. That also sidesteps the lesson recorded in `store.rs`: a lease
//!    that a settle path forgets to release locks the next worker out for its
//!    whole TTL. A claim with no TTL cannot be forgotten into a lockout, because
//!    the only thing that reclaims it is a process that has just started.
//!
//!    **The premise is enforced, not assumed.** It used to read "any `claimed`
//!    intent found at *startup* is abandoned", and the wiring did not make that
//!    true: `recover_pending_vibedev_dispatch` is spawned detached while the
//!    server is already serving turns, and it awaits a real `start_execution`
//!    per intent, so "startup" lasts as long as the reconciler runs. A claim it
//!    found could belong to a turn dispatching *right now*, in this process —
//!    and `Claimed -> Claimed` is a legal transition, so it would take it, both
//!    workers would dispatch, and the loser's rollback would physically delete
//!    the task the winner is running. So every claim now records the process
//!    that made it ([`dispatch_claim_holder`]), and
//!    `DispatchIntentStore::claim_for_recovery` refuses one that names the
//!    process asking. That is the design's own rule, checked: a claim from a
//!    previous instance stays reclaimable immediately, with no TTL to wait out.
//! 2. **No separate outbox object.** Verification needs one because a gate can
//!    outlive its work item. A dispatch intent *is* its work item, so `live/`
//!    holds the record itself and "retire" means "the record went terminal".
//! 3. **One payload kind.** Verification's journal has four payloads because a
//!    transaction can move a gate, an outbox entry and an attestation together.
//!    Here every transaction asserts exactly one thing: the record's next
//!    state.
//!
//! The two are close enough that sharing a generic journal is worth
//! considering — but not by importing this one into that one: verification
//! gates *code*, this admits *work*, and collapsing them now would couple a
//! chat verb to the evidence store.
//!
//! ## The task id is derived, not minted
//!
//! [`dispatch_task_id`] derives the run's task id from the idempotency key, so
//! the intent records *which task this will become* before the task exists.
//! That is what shrinks the crash window: creation goes through
//! `ArtifactV2Service::ensure_task_with_id`, which is itself idempotent, so a
//! crash after the task is created but before the intent advances still leaves
//! a record naming the orphan — recovery can find it.
//!
//! ## The intent carries what the run is, not only which run it is
//!
//! Naming the task is not enough for the *earliest* window — admitted, and the
//! process died before the task existed. A record that only names a
//! non-existent task can be settled but not finished, which silently drops a
//! request the caller was told was durably admitted. So the intent also carries
//! a [`DispatchTaskPlan`]: the assembled task, committed in the *same journal
//! transaction* as the admission. Recovery creates and dispatches from it
//! instead of guessing, and every crash window from admit onward is now
//! finishable.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};

use crate::magician_v2::execution::coding_engine::selection::CodingEngineConstraint;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use uuid::Uuid;

pub(crate) const DISPATCH_INTENT_SCHEMA_VERSION: u32 = 1;
pub(crate) const DISPATCH_INTENT_JOURNAL_SCHEMA_VERSION: u32 = 1;

/// Every derived value is domain-separated, so a digest can never be mistaken
/// for a key and a key can never be mistaken for a task id even though all
/// three are blake3 over strings.
const DISPATCH_KEY_DOMAIN: &str = "magician/vibedev/dispatch-intent/idempotency-key/v1";
const DISPATCH_DIGEST_DOMAIN: &str = "magician/vibedev/dispatch-intent/request-digest/v1";
const DISPATCH_TASK_ID_DOMAIN: &str = "magician/vibedev/dispatch-intent/task-id/v1";

/// Keys are used as directory names, so they carry a prefix and a fixed shape
/// that [`valid_intent_key`] enforces at every entry point. A derived key can
/// never be anything else, but the record on disk is not derived — it is read
/// back — and a key read from a file is untrusted input.
const DISPATCH_INTENT_KEY_PREFIX: &str = "vdi-";
const DISPATCH_INTENT_KEY_HEX_LEN: usize = 64;

/// Which kind of run was admitted. Part of the request digest, because a build
/// and a plan run over the same words are different work.
///
/// `Autopilot` is the cockpit's third studio mode, and it is a *mode* rather
/// than a flag beside `Build` for exactly one reason: the digest covers the mode
/// and nothing else about the studio, so an autopilot run and an attended build
/// over the same words under the same key **conflict** instead of silently
/// swapping one for the other. An autopilot run is still a build — it stages no
/// `plan` tag and takes the build layout — so every `is_plan()` test stays false
/// for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    Build,
    Plan,
    Autopilot,
}

impl DispatchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Plan => "plan",
            Self::Autopilot => "autopilot",
        }
    }

    /// `#discuss` (typed) / "start a vibedev plan" (spoken) is the plan run.
    pub fn from_discuss(discuss: bool) -> Self {
        if discuss {
            Self::Plan
        } else {
            Self::Build
        }
    }
}

/// Which coding engine a VibeDev request runs on, as the *client* is allowed to
/// say it.
///
/// ## Reconciling the two plans that define it
///
/// [`2026-08-07-vibedev-coding-handoff.md`][handoff] §4 and
/// [`2026-08-08-vibedev-codex-app-server-support-plan.md`][codex] §2/§6.1 both
/// spell this type, and they spell it **identically**: two variants, `Auto` and
/// `Profile { profile_id }`. Neither existed in code until now. Where they
/// differ is only in what they say *around* it, and this definition takes both
/// halves:
///
/// * the handoff plan owns the semantics — "omitted choice means the configured
///   default", every surface sends the same type, and the service resolves it
///   before the run is committed;
/// * the Codex plan owns the wire form — `{"kind":"auto"}` or
///   `{"kind":"profile","profile_id":"…"}`, an *engine-neutral* choice that
///   never carries an engine enum, a model, an executable or a repository path,
///   and one immutable choice per request.
///
/// So: the handoff plan's shape, serialized the Codex plan's way. There is no
/// third spelling to reconcile away.
///
/// ## What it deliberately is **not**, yet
///
/// It is not `CodingEngineConstraint`. The Codex plan's immutable, digest-pinned
/// per-task constraint — with its escalation pins and readiness receipts — is
/// that plan's to build, and inventing half of it here would be a schema this
/// module could not honour. What lands now is the *request-side* choice: it is
/// carried on the input, folded into the idempotency digest so the same key with
/// a different engine conflicts rather than silently swapping one for the other,
/// and stored on the dispatch intent so a recovered run is the run that was
/// asked for.
///
/// ## `deny_unknown_fields` lives on
/// [`ClientVibeDevCodingChoice`](crate::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice)
///
/// Unknown *variants* are rejected by serde already. Unknown *fields* on an
/// internally tagged enum are the Codex plan's clause, and they belong on the
/// wire boundary that first deserializes this from a browser —
/// [`ClientVibeDevCodingChoice`](crate::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice).
/// This internal type stays open so a journaled record can still be read if a
/// later field is added.
///
/// [handoff]: ../../../../../docs/plans/2026-08-07-vibedev-coding-handoff.md
/// [codex]: ../../../../../docs/archive/plans/2026-08-08-vibedev-codex-app-server-support-plan.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VibeDevCodingChoice {
    /// The coordinator may pick any eligible profile per coding invocation.
    Auto,
    /// A named profile is the floor for this whole request.
    Profile { profile_id: String },
}

impl VibeDevCodingChoice {
    /// The canonical string the request digest absorbs.
    ///
    /// Variant-prefixed so a profile literally called `auto` cannot digest the
    /// same as `Auto` itself, and so a future variant cannot collide with an
    /// existing profile id.
    pub fn digest_token(&self) -> String {
        match self {
            Self::Auto => "auto".to_string(),
            Self::Profile { profile_id } => format!("profile:{profile_id}"),
        }
    }
}

/// Everything the digest covers, named so a future field is added *here* rather
/// than smuggled into one of these strings.
///
/// What it deliberately does **not** cover is as load-bearing as what it does:
///
/// * the task title and description — both derived from `request`, and the
///   description also embeds store-backed prose. A prompt-store edit between a
///   call and its retry must not manufacture a conflict out of an identical
///   request;
/// * the project's `repo_path` and name — the project *identity* is what the
///   caller chose; its contents are the project's business, and a repo path
///   edited between a call and its retry describes the same target;
/// * the owner agent — read from `coding.lead_agent_id` at start time, so a
///   config reload would otherwise turn a retry into a conflict;
/// * anything time-derived. A digest that moves on its own is not a digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DispatchRequestFacts<'a> {
    /// The user's request, verbatim, with the invoke and mode flag stripped.
    pub request: &'a str,
    pub mode: DispatchMode,
    /// The typed parent link a follow-up continues, derived server-side from
    /// the conversation's own admitted runs. Two turns that differ only by
    /// parent are different requests, so the same key with a different parent
    /// conflicts rather than mutating the run it already started.
    pub parent_task_id: Option<&'a str>,
    pub project_id: &'a str,
    /// The canonical token of a [`VibeDevCodingChoice`],
    /// borrowed because the caller owns it (a `Profile` token is built rather
    /// than named). `None` means "the deployment default", which is a different
    /// request from an explicit choice that happens to name the same engine —
    /// hence the presence byte in [`absorb_optional`].
    pub coding_choice: Option<&'a str>,
}

/// Absorb a labelled field with an explicit length prefix.
///
/// Length-prefixing is what stops one field's content from impersonating the
/// next field's boundary — without it, a request ending in the project id would
/// digest the same as a shorter request with a longer project id.
fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Absorb an optional field so that absent and present-but-empty differ.
fn absorb_optional(hasher: &mut blake3::Hasher, label: &str, value: Option<&str>) {
    absorb(hasher, label, value.unwrap_or_default());
    hasher.update(&[u8::from(value.is_some())]);
}

/// The canonical digest of a request. Same key + same digest is a retry; same
/// key + a different digest is a conflict.
pub(crate) fn dispatch_request_digest(facts: &DispatchRequestFacts<'_>) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", DISPATCH_DIGEST_DOMAIN);
    absorb(&mut hasher, "request", facts.request);
    absorb(&mut hasher, "mode", facts.mode.as_str());
    absorb_optional(&mut hasher, "parent_task_id", facts.parent_task_id);
    absorb(&mut hasher, "project_id", facts.project_id);
    absorb_optional(&mut hasher, "coding_choice", facts.coding_choice);
    hasher.finalize().to_hex().to_string()
}

/// The server-owned idempotency key for one chat turn.
///
/// **Never model-supplied, and never taken from a tool argument.** A model that
/// invented a key could split one request into two runs; a model that repeated
/// one could merge two unrelated requests into the first one's answer. So the
/// key is derived here from what identifies the turn:
///
/// * `chat_turn_id` is the per-request correlation id a chat turn already
///   carries — the UI subscribes to `/events?chat_turn_id=…` with it, so a
///   client retrying the same turn necessarily re-sends the same value, and a
///   new turn either sends a new one or gets a freshly minted
///   `chat-turn-<uuid>` from the server;
/// * the scope and the chat session are folded in because `chat_turn_id` is
///   only unique *within* a scope — the same demux rule `chat_api`'s turn-event
///   tail already applies, so a collision across scopes cannot merge two runs.
///
/// One honest limit: when a client omits `chat_turn_id` entirely the server
/// mints a fresh one per call, so a retry that also omits it derives a
/// different key and is not deduplicated. That is a transport gap, not a
/// storage one — the record is doing exactly what it was asked.
pub fn dispatch_idempotency_key(
    principal: &str,
    workspace: &str,
    chat_session_id: &str,
    chat_turn_id: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", DISPATCH_KEY_DOMAIN);
    absorb(&mut hasher, "principal", principal);
    absorb(&mut hasher, "workspace", workspace);
    absorb(&mut hasher, "chat_session_id", chat_session_id);
    absorb(&mut hasher, "chat_turn_id", chat_turn_id);
    format!("{DISPATCH_INTENT_KEY_PREFIX}{}", hasher.finalize().to_hex())
}

/// The task id an admitted key will become.
///
/// Deterministic on purpose: the intent can name the task *before* the task
/// exists, which is what lets recovery tell "created and orphaned" apart from
/// "never created". Shaped `task_` + 32 hex like every other task id, so
/// nothing downstream — including the started reply, which reads the id aloud —
/// sees anything unusual.
pub fn dispatch_task_id(idempotency_key: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", DISPATCH_TASK_ID_DOMAIN);
    absorb(&mut hasher, "idempotency_key", idempotency_key);
    let hex = hasher.finalize().to_hex().to_string();
    format!("task_{}", &hex[..32])
}

/// A key is a directory name, so it is validated rather than trusted.
pub(crate) fn valid_intent_key(key: &str) -> bool {
    let Some(hex) = key.strip_prefix(DISPATCH_INTENT_KEY_PREFIX) else {
        return false;
    };
    hex.len() == DISPATCH_INTENT_KEY_HEX_LEN
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// This process's identity, minted once and stable for as long as it runs.
///
/// See the module header: the no-TTL claim is only sound if "a claim that
/// outlived its maker" can be told from "a claim someone is holding right now",
/// and the only durable difference between the two is which process wrote it.
/// A uuid rather than a pid because a pid is reused, and a reused pid would
/// make a dead process's claim look live — the one direction that strands work
/// forever.
static DISPATCH_PROCESS_INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Separates the worker's role from the process instance inside `claimed_by`.
/// `@` because neither half can contain one: the roles are compiled constants
/// and the instance is hex.
const DISPATCH_CLAIM_INSTANCE_SEPARATOR: char = '@';

/// The id this process stamps on every claim it makes.
pub(crate) fn dispatch_process_instance_id() -> &'static str {
    DISPATCH_PROCESS_INSTANCE_ID
        .get_or_init(|| Uuid::new_v4().simple().to_string())
        .as_str()
}

/// What a claim records: which worker took it, **and which process that worker
/// was running in**.
///
/// Stamped inside the store rather than by the caller, so a new claiming path
/// cannot forget it — and forgetting it would silently reopen the window where
/// a reconciler steals a live turn's claim.
pub(crate) fn dispatch_claim_holder(holder: &str) -> String {
    format!(
        "{holder}{DISPATCH_CLAIM_INSTANCE_SEPARATOR}{}",
        dispatch_process_instance_id()
    )
}

/// Whether a stored `claimed_by` was written by the process asking.
///
/// A value with no separator — a claim written before claims carried an
/// instance — reads as *another* process's. That is the safe direction: such a
/// claim stays reclaimable exactly as it was before this existed, and the only
/// claim that must never be reclaimed is one this process made, which now
/// always carries the instance.
fn dispatch_claim_is_this_process(claimed_by: Option<&str>) -> bool {
    claimed_by
        .and_then(|value| value.rsplit_once(DISPATCH_CLAIM_INSTANCE_SEPARATOR))
        .is_some_and(|(_, instance)| instance == dispatch_process_instance_id())
}

/// Where an admitted request has got to.
///
/// `settled` and `failed` are terminal, and a terminal record is the *answer*
/// to every later call under the same key. A failed attempt is not silently
/// re-attempted: the caller is told the same thing it was told the first time,
/// which is what "idempotent" means. A genuinely new attempt is a new turn, and
/// therefore a new key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DispatchIntentState {
    /// Durably admitted. The task may or may not exist yet.
    Pending,
    /// Some worker owns finishing this one.
    Claimed,
    /// The run is dispatched and `execution_id` is set.
    Settled,
    /// This attempt terminally did not happen, and nothing was left running.
    Failed,
}

impl DispatchIntentState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Failed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Settled => "settled",
            Self::Failed => "failed",
        }
    }

    /// Legal transitions. `claimed -> claimed` is deliberate: that is recovery
    /// taking over a claim whose owner is gone.
    ///
    /// It is legal here and refused one level up. This table says what a
    /// *record* may do; whether the owner is actually gone is not a property of
    /// the state machine but of the claim's stamped process, which
    /// [`DispatchIntentStore::claim_for_recovery`] checks under the lock. Making
    /// the transition itself illegal would also forbid the case it exists for.
    fn may_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Claimed)
                | (Self::Pending, Self::Failed)
                | (Self::Claimed, Self::Claimed)
                | (Self::Claimed, Self::Settled)
                | (Self::Claimed, Self::Failed)
        )
    }
}

/// Everything the admitted run needs in order to be **created**, captured at
/// admit time and committed in the same journal transaction as the admission.
///
/// This is what closes the earliest crash window. A record that only *names* a
/// task can be settled but not finished, so before this existed an intent whose
/// process died between admit and create was terminally settled — the caller
/// asked for a build, was told it was durably admitted, and nothing left in the
/// system could say what to build.
///
/// It carries the **assembled** title and description rather than the inputs
/// they were assembled from, for two reasons:
///
/// * `ensure_task_with_id` compares an existing manifest field-by-field against
///   the input and rejects a mismatch as `caller_task_id_conflict`. Re-deriving
///   store-backed prose at recovery time would turn an ordinary prompt-store
///   edit into a failed recovery for a task that already exists;
/// * a request is fixed at the moment it is made. A template edited between
///   admission and restart describes a different run.
///
/// What it deliberately does **not** carry is everything the rail re-derives
/// from its own constants — the thread id, the tags, the lifecycle, the output
/// mode, the `created_by` marker. Those are the *current* definition of a
/// VibeDev run; `vibedev_run_task_tags` stays the one place that decides them,
/// and a recovered run should land on today's definition rather than on a
/// resurrected copy of the one that was current when it was admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DispatchTaskPlan {
    /// The scope **as the caller passed it**, which is what the live path puts
    /// on the manifest. Deliberately not the same strings as the record's own
    /// `principal`/`workspace`: those are the sanitised directory segments that
    /// identify the store, and creating the recovered task from them would give
    /// it a different manifest scope than the same run created live. The rail
    /// re-checks that these two sanitise to the record's pair before rebuilding.
    pub principal: String,
    pub workspace: String,
    pub title: String,
    pub description: String,
    /// Resolved from `coding.lead_agent_id` at admit time. Pinned here rather
    /// than re-read at recovery for the same reason it is excluded from the
    /// digest: a config reload must not silently re-own a run.
    pub owner_agent_id: String,
    /// Selects the tag set the rail re-derives, so a plan run recovers as a plan
    /// run.
    pub mode: DispatchMode,
    /// The VibeDev project this run was admitted for.
    ///
    /// Stored so the **next** turn in the same conversation can ask "was the
    /// last run I started for the project I am starting one for now?" without
    /// reading it back out of a task description — the one place a user's own
    /// words are embedded verbatim. It is the typed half of the follow-up
    /// parent link.
    ///
    /// `#[serde(skip_serializing_if)]` is load-bearing, not tidiness: the
    /// journal's integrity check re-serializes a record and compares the hash,
    /// so a field that appeared in the bytes of a record admitted before it
    /// existed would fail that check and take the whole key's replay down.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_id: String,
    /// The run this one continues — **server-derived**, never read from the
    /// turn's text (see `vibedev::rail::resolve_vibedev_rail_parent`). Selects
    /// the follow-up tag set at recovery, so a recovered follow-up is still a
    /// follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// Continuation references the created task carries as `depends_on` — the
    /// parent when it is a clean completed run, mirroring the cockpit's
    /// `continuationReferenceTaskIds`.
    ///
    /// Stored rather than re-derived because `ensure_task_with_id` compares
    /// `depends_on` field-by-field: a parent that finished between admission
    /// and a restart would otherwise make recovery reject its own task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<String>,
    /// Which coding engine the request asked for, or `None` for the configured
    /// default.
    ///
    /// Part of the *request*, so it is stored rather than re-derived: a
    /// deployment default that moved between admission and a restart would
    /// otherwise make recovery start a different run than the one that was
    /// admitted. The digest-pinned [`coding_constraint`] is derived from this
    /// choice plus the submission catalog.
    ///
    /// `skip_serializing_if` for the same reason `project_id` has it: the
    /// journal re-serializes a record and compares the hash, so a field
    /// appearing in the bytes of a record admitted before it existed would fail
    /// that check and take the whole key's replay down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_choice: Option<VibeDevCodingChoice>,
    /// Digest-pinned engine constraint for this request. `None` on records
    /// admitted before Stage 1, and on tests that do not supply a catalog.
    /// Present values are immutable after admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_constraint: Option<CodingEngineConstraint>,
    /// The cockpit's half of an admitted run, or `None` for a rail run.
    ///
    /// Present exactly when the caller was the VibeDev cockpit. It is the
    /// smallest set of things the cockpit decides that the rail's constants
    /// cannot re-derive — everything else about the task (thread, base tags,
    /// output mode) is still the one definition in `vibedev_run_task_tags` and
    /// `vibedev_run_create_task_input`.
    ///
    /// `skip_serializing_if` for the same reason `project_id` and
    /// `coding_choice` have it: the journal re-serializes a record and compares
    /// the hash, so a field appearing in the bytes of a record admitted before
    /// it existed would fail that check and take the whole key's replay down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cockpit: Option<DispatchCockpitPlan>,
}

/// The cockpit-only half of an admitted task plan.
///
/// Stored rather than re-derived for the same reason the assembled prose is:
/// `ensure_task_with_id` compares an existing manifest field-by-field, so a
/// recovered task must be built from the same decisions the live path used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct DispatchCockpitPlan {
    /// The composer's Send (fold this turn into the in-view run) rather than the
    /// Run button (open a separate run row). Selects `vibedev-threaded`.
    #[serde(default)]
    pub threaded: bool,
    /// "Save as task" — promote the run from `Internal` (cockpit-only) to the
    /// user-visible `Persistent` lifecycle.
    #[serde(default)]
    pub save_as_task: bool,
    /// The cron schedule, as the **already-serialized** JSON the create input
    /// takes, or `None` for a run that starts now.
    ///
    /// A `String` and not a `serde_json::Value` because `Value` is not `Eq` and
    /// this plan is compared for equality all the way up through
    /// [`DispatchIntent`]. The bytes are the client's own
    /// `serializeScheduleForApi` output, re-parsed once at create time, so the
    /// schedule a restart rebuilds is the schedule that was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_json: Option<String>,
    /// `created_by` on the manifest. The cockpit's runs are `user`-created; the
    /// rail's are `chat_vibedev_rail`. Stored because it is the caller's
    /// identity, not this module's constant.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    /// Pin this run as the project's `active_root_task_id`.
    ///
    /// **Caller-controlled, never unconditional.** The cockpit pins (the spine
    /// and preview scope to the pointer); the rail deliberately does not, so a
    /// chat aside cannot move what the cockpit is looking at.
    #[serde(default)]
    pub pin_project_pointer: bool,
    /// The project the pointer is pinned on. Held here rather than read from
    /// `DispatchTaskPlan::project_id` so a rollback after a restart unwinds the
    /// same pointer the admission pinned even if the two ever diverge.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pinned_project_id: String,
}

/// `skip_serializing_if` for a defaulted integer, which has no inherent
/// predicate. Free-standing rather than a closure because `serde` takes a path.
fn is_zero_attempts(value: &u32) -> bool {
    *value == 0
}

/// The durable record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIntent {
    pub schema_version: u32,
    pub idempotency_key: String,
    pub principal: String,
    pub workspace: String,
    /// The conversation the run belongs to — not used for matching (the key
    /// already binds it), but load-bearing at recovery: it is the session the
    /// rebuilt task is bound to, so ordinary chat-session cleanup still owns a
    /// run a restart created.
    pub chat_session_id: String,
    pub request_digest: String,
    /// Derived from the key by [`dispatch_task_id`], and therefore known before
    /// the task exists.
    pub task_id: String,
    /// The assembled task, captured at admit time.
    ///
    /// `Option` only for records written before this field existed: those name
    /// a task and nothing else, and recovery must settle them terminally rather
    /// than refuse to parse them and take the whole scope's recovery down with
    /// them. Every record this build admits carries one.
    ///
    /// `skip_serializing_if` for the same reason [`DispatchTaskPlan`]'s newer
    /// fields have it, and it was missing here — the one place the hazard bites
    /// hardest. [`DispatchIntentTransaction::verify`] re-serializes the record
    /// and compares the hash to the one stored beside it, so without this a
    /// record admitted before the plan existed would come back as
    /// `"task_plan": null`, fail its integrity check, and take the whole key's
    /// replay down. The `Option` that exists precisely to tolerate those records
    /// was what made them unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_plan: Option<DispatchTaskPlan>,
    pub execution_id: Option<String>,
    pub(crate) state: DispatchIntentState,
    /// How many times startup recovery has **claimed** this intent.
    ///
    /// Incremented in the same durable commit as the claim, before any work, so
    /// an attempt that kills the process still counts. That is the point: a
    /// clean create/dispatch failure already settles terminally, so the only way
    /// to retry forever is a crash loop, and a counter written before the work
    /// is the only kind a crash cannot forget to write.
    ///
    /// `skip_serializing_if` for the same reason [`Self::task_plan`] has it: a
    /// record admitted before this field existed carries no key for it, and
    /// emitting `"recovery_attempts": 0` on re-serialization would fail the
    /// journal's integrity check and take the key's replay down. The absent form
    /// and the default form must produce the same bytes, or `#[serde(default)]`
    /// is only half a migration.
    #[serde(default, skip_serializing_if = "is_zero_attempts")]
    pub recovery_attempts: u32,
    /// Fencing token. Bumped by every commit and re-checked under the lock, so
    /// a stalled writer cannot commit over its replacement.
    pub generation: u64,
    pub claimed_by: Option<String>,
    /// Set only on `failed`, and carries the ORIGINAL failure, never a
    /// cleanup's.
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DispatchIntent {
    /// A freshly admitted intent, carrying the task it is admission *for*.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admitted(
        idempotency_key: &str,
        principal: &str,
        workspace: &str,
        chat_session_id: &str,
        request_digest: &str,
        task_plan: DispatchTaskPlan,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: DISPATCH_INTENT_SCHEMA_VERSION,
            idempotency_key: idempotency_key.to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            chat_session_id: chat_session_id.to_string(),
            request_digest: request_digest.to_string(),
            task_id: dispatch_task_id(idempotency_key),
            task_plan: Some(task_plan),
            execution_id: None,
            state: DispatchIntentState::Pending,
            generation: 0,
            claimed_by: None,
            failure_reason: None,
            recovery_attempts: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Replay is held to the same rules as a live write, so a journal that was
    /// edited cannot walk a record into a state the typed API would refuse.
    fn validate_successor(&self, prior: &Self) -> Result<()> {
        if self.idempotency_key != prior.idempotency_key {
            return Err(anyhow!(
                "dispatch intent {} cannot succeed {}; the key is immutable",
                self.idempotency_key,
                prior.idempotency_key
            ));
        }
        if self.principal != prior.principal || self.workspace != prior.workspace {
            return Err(anyhow!(
                "dispatch intent {} cannot change scope",
                self.idempotency_key
            ));
        }
        // The whole point of the digest is that it pins what was admitted. A
        // successor that changes it would be the mutation the conflict rule
        // exists to prevent, arriving through the back door.
        if self.request_digest != prior.request_digest {
            return Err(anyhow!(
                "dispatch intent {} cannot change its request digest",
                self.idempotency_key
            ));
        }
        if self.task_id != prior.task_id {
            return Err(anyhow!(
                "dispatch intent {} cannot change its task id",
                self.idempotency_key
            ));
        }
        // The plan is what a restart builds the run from, so it is as immutable
        // as the digest that pinned the request: a successor that rewrote it
        // could turn an admitted request into a different one, which is exactly
        // what admitting durably was supposed to prevent.
        if self.task_plan != prior.task_plan {
            return Err(anyhow!(
                "dispatch intent {} cannot change its task plan",
                self.idempotency_key
            ));
        }
        if self.recovery_attempts < prior.recovery_attempts {
            return Err(anyhow!(
                "dispatch intent {} has {} recovery attempts but the successor claims {}; \
                 the attempt count only moves forward",
                self.idempotency_key,
                prior.recovery_attempts,
                self.recovery_attempts
            ));
        }
        if self.created_at != prior.created_at {
            return Err(anyhow!(
                "dispatch intent {} cannot change its admission time",
                self.idempotency_key
            ));
        }
        if self.generation <= prior.generation {
            return Err(anyhow!(
                "dispatch intent {} is at generation {} but the successor holds {}; \
                 refusing a superseded write",
                self.idempotency_key,
                prior.generation,
                self.generation
            ));
        }
        if !prior.state.may_transition_to(self.state) {
            return Err(anyhow!(
                "dispatch intent {} cannot move from {} to {}",
                self.idempotency_key,
                prior.state.as_str(),
                self.state.as_str()
            ));
        }
        Ok(())
    }
}

/// What [`DispatchIntentStore::admit`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmitOutcome {
    /// Nothing was admitted under this key before. The caller owns creating and
    /// dispatching the run.
    Admitted(DispatchIntent),
    /// The same key and the same request were already admitted. **No admission
    /// was written**, and the caller must not create or dispatch anything.
    ///
    /// Not quite "nothing was written": if the record was found in the journal
    /// with its projection missing, the projection is repaired on the way past.
    /// That is a cache write, not a state change — the record it re-projects is
    /// the one the journal already holds.
    AlreadyAdmitted(DispatchIntent),
}

impl AdmitOutcome {
    /// Test-only accessors: production callers match on the variants
    /// directly (run_service), so these exist for the in-file admission
    /// tests and are gated the same way as the other test-only items in
    /// this module.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn intent(&self) -> &DispatchIntent {
        match self {
            Self::Admitted(intent) | Self::AlreadyAdmitted(intent) => intent,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn is_replay(&self) -> bool {
        matches!(self, Self::AlreadyAdmitted(_))
    }
}

/// The parts of a conflict a caller needs in order to log it or answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictDetails<'a> {
    pub idempotency_key: &'a str,
    /// The run that already exists under this key, and that a conflict leaves
    /// completely alone.
    pub existing_task_id: &'a str,
    pub existing_digest: &'a str,
    pub incoming_digest: &'a str,
}

/// Why a dispatch-intent call did not do what was asked.
#[derive(Debug)]
pub enum DispatchIntentError {
    /// The key was reused for a different request. **The existing record and
    /// its run are untouched** — that is the whole contract of this variant.
    Conflict {
        idempotency_key: String,
        existing_task_id: String,
        existing_digest: String,
        incoming_digest: String,
    },
    /// Startup recovery tried to take over a claim held by a worker **in this
    /// process**, which means the claim is live rather than abandoned.
    ///
    /// A refusal, not a failure: nothing is written and the record is left
    /// exactly as it was. Only [`DispatchIntentStore::claim_for_recovery`]
    /// produces it, and the honest outcome for the reconciler is to leave the
    /// intent alone — the holder is still working on it, and if the holder dies
    /// the claim becomes another process's the moment this one restarts.
    HeldByThisProcess {
        idempotency_key: String,
        claimed_by: String,
    },
    Storage(anyhow::Error),
}

impl DispatchIntentError {
    /// `Some` only for a conflict. The caller logs these — the digests are the
    /// only way to tell "the client reused a turn id" apart from "the request
    /// really did change", and neither belongs in prose shown to a user.
    pub fn conflict(&self) -> Option<ConflictDetails<'_>> {
        match self {
            Self::Conflict {
                idempotency_key,
                existing_task_id,
                existing_digest,
                incoming_digest,
            } => Some(ConflictDetails {
                idempotency_key: idempotency_key.as_str(),
                existing_task_id: existing_task_id.as_str(),
                existing_digest: existing_digest.as_str(),
                incoming_digest: incoming_digest.as_str(),
            }),
            Self::HeldByThisProcess { .. } | Self::Storage(_) => None,
        }
    }

    /// The intent is alive in this process and must be left alone. Callers use
    /// this to tell "a live turn owns it" apart from "the store is broken",
    /// which are the same `Err` but opposite operational stories.
    pub fn is_held_by_this_process(&self) -> bool {
        matches!(self, Self::HeldByThisProcess { .. })
    }
}

impl std::fmt::Display for DispatchIntentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict {
                existing_task_id, ..
            } => write!(
                f,
                "this turn already started a different VibeDev run ({existing_task_id}); \
                 that run is untouched and still going"
            ),
            Self::HeldByThisProcess {
                idempotency_key,
                claimed_by,
            } => write!(
                f,
                "dispatch intent {idempotency_key} is claimed by {claimed_by} in this same \
                 process, so it is being worked on rather than abandoned"
            ),
            Self::Storage(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DispatchIntentError {}

impl From<anyhow::Error> for DispatchIntentError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}

/// Held for the duration of a read-modify-write.
pub(crate) struct IntentLock {
    file: std::fs::File,
}

impl Drop for IntentLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

// ---------------------------------------------------------------- journal

/// One committed transaction: the record's next state, and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchIntentTransaction {
    pub schema_version: u32,
    pub txn_id: String,
    /// Monotonic within a key. Replay applies in this order.
    pub sequence: u64,
    pub idempotency_key: String,
    pub committed_at: DateTime<Utc>,
    pub intent: DispatchIntent,
    /// Integrity check over the serialized record. A torn or edited
    /// transaction is refused rather than half-applied.
    pub payload_digest: String,
}

impl DispatchIntentTransaction {
    pub fn new(sequence: u64, intent: DispatchIntent) -> Result<Self> {
        let payload_digest = digest_intent(&intent)?;
        Ok(Self {
            schema_version: DISPATCH_INTENT_JOURNAL_SCHEMA_VERSION,
            txn_id: format!("vditxn-{}", Uuid::new_v4()),
            sequence,
            idempotency_key: intent.idempotency_key.clone(),
            committed_at: Utc::now(),
            intent,
            payload_digest,
        })
    }

    pub fn verify(&self) -> Result<()> {
        if self.schema_version != DISPATCH_INTENT_JOURNAL_SCHEMA_VERSION {
            return Err(anyhow!(
                "dispatch intent txn {} has schema version {}; this build understands {}",
                self.txn_id,
                self.schema_version,
                DISPATCH_INTENT_JOURNAL_SCHEMA_VERSION
            ));
        }
        if self.intent.schema_version != DISPATCH_INTENT_SCHEMA_VERSION {
            return Err(anyhow!(
                "dispatch intent {} has record schema version {}; this build understands {}",
                self.idempotency_key,
                self.intent.schema_version,
                DISPATCH_INTENT_SCHEMA_VERSION
            ));
        }
        if self.intent.idempotency_key != self.idempotency_key {
            return Err(anyhow!(
                "dispatch intent txn {} is filed under {} but carries {}",
                self.txn_id,
                self.idempotency_key,
                self.intent.idempotency_key
            ));
        }
        if digest_intent(&self.intent)? != self.payload_digest {
            return Err(anyhow!(
                "dispatch intent txn {} failed its integrity check",
                self.txn_id
            ));
        }
        Ok(())
    }
}

fn digest_intent(intent: &DispatchIntent) -> Result<String> {
    let bytes = serde_json::to_vec(intent).context("serialize dispatch intent")?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Append-only transaction log for one scope's dispatch intents.
///
/// Layout, under the scope root:
///
/// ```text
/// vibedev/dispatch_intents/
///   journal/<key>/<sequence>.json   committed transactions (the truth)
///   intents/<key>.json              projected record
///   live/<key>.json                 projected outbox: present iff unfinished
/// ```
///
/// Under `vibedev/` because that is already where VibeDev's scope-owned state
/// lives (`vibedev/projects.json`), so a scope's VibeDev data stays in one
/// place and a scope delete takes all of it.
pub struct DispatchIntentJournal {
    root: PathBuf,
}

impl DispatchIntentJournal {
    pub fn new(intents_root: impl Into<PathBuf>) -> Self {
        Self {
            root: intents_root.into().join("journal"),
        }
    }

    fn key_dir(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn txn_path(&self, key: &str, sequence: u64) -> PathBuf {
        // Zero-padded so lexical order matches numeric order.
        self.key_dir(key).join(format!("{sequence:020}.json"))
    }

    pub fn next_sequence(&self, key: &str) -> Result<u64> {
        Ok(self.last_sequence(key)?.map_or(0, |seq| seq + 1))
    }

    pub fn last_sequence(&self, key: &str) -> Result<Option<u64>> {
        let dir = self.key_dir(key);
        if !dir.is_dir() {
            return Ok(None);
        }
        let mut max: Option<u64> = None;
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read dispatch intent journal dir {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            // In-flight temp files carry a uuid in the stem and never parse, so
            // only committed records count.
            let Ok(seq) = stem.parse::<u64>() else {
                continue;
            };
            max = Some(max.map_or(seq, |current: u64| current.max(seq)));
        }
        Ok(max)
    }

    /// Commit with a single atomic rename. Before this returns, either the
    /// whole record is durable or none of it is.
    pub fn commit(&self, txn: &DispatchIntentTransaction) -> Result<()> {
        txn.verify()?;
        if !valid_intent_key(&txn.idempotency_key) {
            return Err(anyhow!(
                "refusing to commit dispatch intent under a malformed key"
            ));
        }
        let dir = self.key_dir(&txn.idempotency_key);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create dispatch intent journal dir {}", dir.display()))?;
        let path = self.txn_path(&txn.idempotency_key, txn.sequence);
        if path.exists() {
            return Err(anyhow!(
                "dispatch intent journal already has sequence {} for {}",
                txn.sequence,
                txn.idempotency_key
            ));
        }
        let bytes = serde_json::to_vec_pretty(txn).context("serialize dispatch intent txn")?;
        let tmp = dir.join(format!(
            "{:020}.{}.json.tmp",
            txn.sequence,
            Uuid::new_v4().simple()
        ));
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("write dispatch intent tmp {}", tmp.display()))?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                Err(error).with_context(|| format!("commit dispatch intent -> {}", path.display()))
            },
        }
    }

    /// Every committed transaction for a key, in sequence order.
    ///
    /// A corrupt record aborts the read rather than being skipped — silently
    /// skipping one is how a record ends up projected into a state nothing
    /// committed.
    pub fn read_all(&self, key: &str) -> Result<Vec<DispatchIntentTransaction>> {
        let dir = self.key_dir(key);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<(u64, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read dispatch intent journal dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            let Ok(seq) = stem.parse::<u64>() else {
                continue;
            };
            paths.push((seq, path));
        }
        paths.sort_by_key(|(seq, _)| *seq);

        let mut out = Vec::with_capacity(paths.len());
        for (seq, path) in paths {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read dispatch intent txn {}", path.display()))?;
            let txn: DispatchIntentTransaction = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse dispatch intent txn {}", path.display()))?;
            txn.verify()
                .with_context(|| format!("verify dispatch intent txn {}", path.display()))?;
            if txn.sequence != seq {
                return Err(anyhow!(
                    "dispatch intent txn {} claims sequence {} but is filed at {}",
                    txn.txn_id,
                    txn.sequence,
                    seq
                ));
            }
            out.push(txn);
        }
        Ok(out)
    }

    /// Fold a history into the current record.
    pub fn fold(txns: &[DispatchIntentTransaction]) -> Result<Option<DispatchIntent>> {
        let mut current: Option<DispatchIntent> = None;
        for txn in txns {
            let next = txn.intent.clone();
            if let Some(prior) = &current {
                next.validate_successor(prior).with_context(|| {
                    format!(
                        "dispatch intent txn {} is not a legal successor",
                        txn.txn_id
                    )
                })?;
            }
            current = Some(next);
        }
        Ok(current)
    }

    pub fn replay(&self, key: &str) -> Result<Option<DispatchIntent>> {
        Self::fold(&self.read_all(key)?)
    }

    /// The on-disk path of one committed transaction.
    ///
    /// `#[cfg(any(test, feature = "test-fixtures"))]` and crate-visible so tests in the *calling* modules can
    /// reproduce an on-disk state no API can produce — a torn transaction, or a
    /// record written by a build that had never heard of one of today's fields.
    /// Production code addresses transactions through [`Self::commit`] and
    /// [`Self::read_all`] and has no business knowing where they live.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn committed_txn_path(&self, key: &str, sequence: u64) -> PathBuf {
        self.txn_path(key, sequence)
    }

    /// Every key with journal history in this scope. Used at startup.
    pub fn keys(&self) -> Result<Vec<String>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("read dispatch intent journal {}", self.root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // A stray directory is ignored rather than fatal: it must not take
            // recovery down for every other intent in the scope.
            if valid_intent_key(name) {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}

// ------------------------------------------------------------------ store

/// What one [`DispatchIntentStore::recover`] pass saw.
///
/// Two lists rather than a count because the keys are the useful half: a key
/// that will not replay is a thing an operator has to go and look at, and
/// "3 keys were unreadable" without saying which is a metric nobody can act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchProjectionRepair {
    /// Replayed, scope-checked and projected. The projections for these keys
    /// now match the journal.
    pub repaired: Vec<String>,
    /// Could not be replayed or could not be projected, and was skipped so the
    /// rest of the scope could still be recovered. **Nothing about these keys
    /// is known to be current**, including whether they are in the outbox.
    pub unreadable: Vec<String>,
}

/// Scope-scoped persistence for dispatch intents.
///
/// Constructed against one scope's root, so cross-scope confusion is
/// structurally impossible — and on top of that physical isolation every loaded
/// record is re-checked against the store's own scope, because a record copied
/// between directories would otherwise be honoured.
///
/// All methods are sync; the records are one small JSON file each and the
/// callers are the rail's dispatch path and a startup reconciler, neither of
/// which is a hot loop. This matches `VerificationStore`, whose
/// `recover_pending_verification` caller also calls it directly from async.
pub struct DispatchIntentStore {
    root: PathBuf,
    principal: String,
    workspace: String,
    journal: DispatchIntentJournal,
}

impl DispatchIntentStore {
    pub fn new(scope_root: impl AsRef<Path>, principal: &str, workspace: &str) -> Self {
        let root = scope_root.as_ref().join("vibedev/dispatch_intents");
        let journal = DispatchIntentJournal::new(&root);
        Self {
            root,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            journal,
        }
    }

    pub fn journal(&self) -> &DispatchIntentJournal {
        &self.journal
    }

    fn intent_path(&self, key: &str) -> PathBuf {
        self.root.join("intents").join(format!("{key}.json"))
    }

    fn live_path(&self, key: &str) -> PathBuf {
        self.root.join("live").join(format!("{key}.json"))
    }

    /// The two projection paths, for tests in other modules that reproduce a
    /// crash between the journal commit and the projection write.
    ///
    /// `#[cfg(any(test, feature = "test-fixtures"))]` for the same reason
    /// [`DispatchIntentJournal::committed_txn_path`] is: the projections are a
    /// cache this type owns, and any production caller that addressed them by
    /// path would be writing a second, unfenced write path.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn projected_record_path(&self, key: &str) -> PathBuf {
        self.intent_path(key)
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn outbox_record_path(&self, key: &str) -> PathBuf {
        self.live_path(key)
    }

    fn lock(&self, name: &str) -> Result<IntentLock> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create dispatch intent root {}", self.root.display()))?;
        let path = self.root.join(format!("{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open dispatch intent lock {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("lock dispatch intents {}", path.display()))?;
        Ok(IntentLock { file })
    }

    fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(value).context("serialize dispatch intent")?;
        let tmp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4().simple()));
        std::fs::write(&tmp, bytes).with_context(|| format!("write tmp {}", tmp.display()))?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                Err(error).with_context(|| format!("rename tmp -> {}", path.display()))
            },
        }
    }

    fn assert_scope(&self, intent: &DispatchIntent) -> Result<()> {
        if intent.principal != self.principal || intent.workspace != self.workspace {
            return Err(anyhow!(
                "dispatch intent belongs to scope {}/{} but this store is {}/{}; \
                 cross-scope access refused",
                intent.principal,
                intent.workspace,
                self.principal,
                self.workspace
            ));
        }
        Ok(())
    }

    /// Read the projected record for a key. `None` when nothing was admitted.
    ///
    /// **`None` means absent, never unreadable.** It used to be decided by
    /// `Path::exists()`, which answers `false` for every reason a `stat` can
    /// fail — EIO, EACCES, ENOTDIR, an unmounted volume — and not only for "the
    /// file is not there". [`Self::admit`] turns that answer into "nothing was
    /// admitted under this key", which on a scope whose storage is misbehaving
    /// is the one wrong answer: it admits a *second* time under a key the
    /// journal already holds. So the read itself decides, and only a genuine
    /// `NotFound` is absence.
    pub fn find(&self, key: &str) -> Result<Option<DispatchIntent>> {
        if !valid_intent_key(key) {
            return Ok(None);
        }
        let path = self.intent_path(key);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read dispatch intent {}", path.display()));
            },
        };
        let intent: DispatchIntent = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse dispatch intent {}", path.display()))?;
        if intent.schema_version != DISPATCH_INTENT_SCHEMA_VERSION {
            return Err(anyhow!(
                "dispatch intent {key} has schema version {}; this build understands {}",
                intent.schema_version,
                DISPATCH_INTENT_SCHEMA_VERSION
            ));
        }
        if intent.idempotency_key != key {
            return Err(anyhow!(
                "dispatch intent filed at {key} carries key {}",
                intent.idempotency_key
            ));
        }
        self.assert_scope(&intent)?;
        Ok(Some(intent))
    }

    /// Admit a request, or report what was already admitted under this key.
    ///
    /// This is the whole idempotency decision, and it is one call because
    /// lookup and insert must not be separable — two concurrent retries that
    /// both looked and then both inserted would be exactly the duplicate run
    /// this exists to prevent.
    ///
    /// A conflict writes **nothing**: the stored record and the run it names
    /// are left exactly as they were.
    ///
    /// `task_plan` is committed **with** the admission, in the same journal
    /// transaction: after this returns, the request is not only recorded as
    /// admitted, it is recoverable — a restart can build exactly the run that
    /// was asked for. A conflicting or replayed call keeps the plan the FIRST
    /// call stored; the incoming one is discarded, which is what makes a retry
    /// after a prompt-store edit still describe the original request.
    ///
    /// ## The idempotency decision is taken against the JOURNAL
    ///
    /// The projection is a cache, and this used to consult only the cache. The
    /// gap it left is the exact crash this machinery exists for: `admit`
    /// commits the journal transaction and *then* writes the projection, so a
    /// process that dies between the two — or a projection write that fails —
    /// leaves a key that is durably admitted and invisible to [`Self::find`].
    /// The retry then admitted a **second** time, at sequence 1, with a
    /// different `created_at`; `validate_successor` refuses that as a successor,
    /// and the journal is append-only with no compaction, so the key was
    /// permanently unreplayable — and one such key used to take its whole
    /// scope's recovery down with it.
    ///
    /// So the journal decides whether anything was admitted, and the projection
    /// only decides what to answer with. A key the journal knows and the
    /// projection does not is replayed here and re-projected on the way past:
    /// the retry gets the idempotent answer it came for, and the cache is
    /// repaired as a side effect rather than waiting for the next startup.
    pub(crate) fn admit(
        &self,
        idempotency_key: &str,
        chat_session_id: &str,
        request_digest: &str,
        task_plan: DispatchTaskPlan,
    ) -> std::result::Result<AdmitOutcome, DispatchIntentError> {
        if !valid_intent_key(idempotency_key) {
            return Err(DispatchIntentError::Storage(anyhow!(
                "refusing a malformed dispatch idempotency key"
            )));
        }
        let _lock = self.lock("intents")?;

        let projected = self.find(idempotency_key)?;
        let existing = match projected {
            Some(existing) => Some(existing),
            // Nothing projected. Before concluding "never admitted" — the one
            // conclusion that writes a second admission — ask the truth.
            None => match self.journal.last_sequence(idempotency_key)? {
                None => None,
                Some(last_sequence) => {
                    let replayed = self.journal.replay(idempotency_key)?.ok_or_else(|| {
                        anyhow!(
                            "dispatch intent {idempotency_key} has journal history through \
                             sequence {last_sequence} but replays to nothing; refusing to admit \
                             it a second time"
                        )
                    })?;
                    self.assert_scope(&replayed)?;
                    tracing::warn!(
                        idempotency_key = %idempotency_key,
                        task_id = %replayed.task_id,
                        "[VIBEDEV-RAIL] dispatch intent was admitted but not projected; \
                         answering from the journal and repairing the projection"
                    );
                    // Best-effort: the answer above is already correct without
                    // it, and the whole reason we are here is that this write
                    // may be the thing that is failing.
                    if let Err(error) = self.project(&replayed) {
                        tracing::warn!(
                            idempotency_key = %idempotency_key,
                            %error,
                            "[VIBEDEV-RAIL] could not repair the dispatch intent projection"
                        );
                    }
                    Some(replayed)
                },
            },
        };

        if let Some(existing) = existing {
            if existing.request_digest != request_digest {
                return Err(DispatchIntentError::Conflict {
                    idempotency_key: idempotency_key.to_string(),
                    existing_task_id: existing.task_id.clone(),
                    existing_digest: existing.request_digest.clone(),
                    incoming_digest: request_digest.to_string(),
                });
            }
            return Ok(AdmitOutcome::AlreadyAdmitted(existing));
        }

        let intent = DispatchIntent::admitted(
            idempotency_key,
            &self.principal,
            &self.workspace,
            chat_session_id,
            request_digest,
            task_plan,
            Utc::now(),
        );
        let txn = DispatchIntentTransaction::new(
            self.journal.next_sequence(idempotency_key)?,
            intent.clone(),
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn.intent)?;
        Ok(AdmitOutcome::Admitted(intent))
    }

    /// Commit the next state of a record, fenced against the *stored*
    /// generation.
    fn commit(
        &self,
        next: DispatchIntent,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        self.commit_guarded(next, |_| Ok(()))
    }

    /// [`Self::commit`] with one extra precondition, evaluated against the
    /// **stored** record while the lock is held.
    ///
    /// The generation fence already guarantees that a successful commit's
    /// `prior` is the caller's own view, so a caller could in principle check
    /// its snapshot itself. This exists so a precondition that decides whether
    /// a claim may be *taken over* is read from the same bytes the write is
    /// fenced against, rather than from a snapshot that was true when it was
    /// taken — the two agree today, and this makes them unable to stop agreeing.
    fn commit_guarded<G>(
        &self,
        next: DispatchIntent,
        guard: G,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError>
    where
        G: FnOnce(&DispatchIntent) -> std::result::Result<(), DispatchIntentError>,
    {
        self.assert_scope(&next)?;
        let _lock = self.lock("intents")?;
        let prior = self
            .find(&next.idempotency_key)?
            .ok_or_else(|| anyhow!("dispatch intent {} does not exist", next.idempotency_key))?;
        guard(&prior)?;
        // The fence: the writer's view of the record must still be the stored
        // one. A worker that stalled, lost its turn and woke up holds an older
        // generation and is refused here rather than overwriting its successor.
        if prior.generation + 1 != next.generation {
            return Err(DispatchIntentError::Storage(anyhow!(
                "dispatch intent {} is at generation {} but the writer built {}; \
                 refusing a superseded write",
                prior.idempotency_key,
                prior.generation,
                next.generation
            )));
        }
        next.validate_successor(&prior)?;

        let txn = DispatchIntentTransaction::new(
            self.journal.next_sequence(&next.idempotency_key)?,
            next.clone(),
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn.intent)?;
        Ok(next)
    }

    /// Take ownership of finishing an intent.
    ///
    /// The claim records **this process** as well as the worker
    /// ([`dispatch_claim_holder`]), which is what lets
    /// [`Self::claim_for_recovery`] tell a live claim from an abandoned one.
    pub fn claim(
        &self,
        prior: &DispatchIntent,
        holder: &str,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        let mut next = prior.clone();
        next.state = DispatchIntentState::Claimed;
        next.claimed_by = Some(dispatch_claim_holder(holder));
        next.generation = prior.generation + 1;
        next.updated_at = Utc::now();
        self.commit(next)
    }

    /// Take ownership on behalf of startup recovery, and **charge an attempt**
    /// for it.
    ///
    /// Separate from [`Self::claim`] because the count means "restarts that have
    /// tried to finish this", not "claims" — the turn that admitted the run has
    /// a live caller and is not a retry of anything. The increment rides the
    /// same commit as the claim so it is durable before any work begins; a
    /// process that dies mid-attempt has still spent one, which is the only
    /// reason the bound can stop a crash loop at all.
    ///
    /// ## It refuses a claim this process is holding
    ///
    /// This is where the module header's no-TTL premise is actually enforced.
    /// `Claimed -> Claimed` is legal so that recovery can take over an abandoned
    /// claim, and nothing used to check that the claim *was* abandoned: the
    /// reconciler runs detached while the server serves turns, so it could take
    /// a claim from a turn that was in the middle of `start_execution`. Both
    /// would then dispatch, one would lose with `task_execution_in_progress`,
    /// and its rollback would physically delete the task the other is running.
    ///
    /// A claim stamped with *this* process is therefore refused outright —
    /// nothing is written and the record is left alone. A claim from any other
    /// process is still reclaimable immediately, because the only way a claim
    /// outlives its process is for that process to have died. That is the
    /// design's stated rule, and it costs no lease and no expiry to wait out.
    ///
    /// Two things it deliberately does not cover, both of which need a real
    /// lease: **two processes over one data directory** (the second's instance
    /// differs, so it will take the first's live claim), and a claim this
    /// process abandoned without dying (it stays refused until a restart, which
    /// leaves the intent in the outbox — the safe direction).
    pub fn claim_for_recovery(
        &self,
        prior: &DispatchIntent,
        holder: &str,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        let mut next = prior.clone();
        next.state = DispatchIntentState::Claimed;
        next.claimed_by = Some(dispatch_claim_holder(holder));
        next.recovery_attempts = prior.recovery_attempts.saturating_add(1);
        next.generation = prior.generation + 1;
        next.updated_at = Utc::now();
        self.commit_guarded(next, |stored| {
            if dispatch_claim_is_this_process(stored.claimed_by.as_deref()) {
                return Err(DispatchIntentError::HeldByThisProcess {
                    idempotency_key: stored.idempotency_key.clone(),
                    claimed_by: stored.claimed_by.clone().unwrap_or_default(),
                });
            }
            Ok(())
        })
    }

    /// A claim written by a **previous** process, for tests that reproduce the
    /// state a crashed turn leaves behind.
    ///
    /// [`Self::claim_for_recovery`] refuses a claim carrying this process's
    /// instance, so a test that wants the abandoned-claim case cannot produce
    /// one with [`Self::claim`] — it would be reproducing the *live* case and
    /// asserting the wrong thing. `#[cfg(any(test, feature = "test-fixtures"))]` because no production path has
    /// any business forging another process's identity.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn claim_as_a_previous_process(
        &self,
        prior: &DispatchIntent,
        holder: &str,
        charge_attempt: bool,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        let mut next = prior.clone();
        next.state = DispatchIntentState::Claimed;
        next.claimed_by = Some(format!(
            "{holder}{DISPATCH_CLAIM_INSTANCE_SEPARATOR}a-process-that-is-gone"
        ));
        if charge_attempt {
            next.recovery_attempts = prior.recovery_attempts.saturating_add(1);
        }
        next.generation = prior.generation + 1;
        next.updated_at = Utc::now();
        self.commit(next)
    }

    /// The run is dispatched. Terminal, and the record leaves the outbox.
    pub fn settle(
        &self,
        prior: &DispatchIntent,
        execution_id: &str,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        let mut next = prior.clone();
        next.state = DispatchIntentState::Settled;
        next.execution_id = Some(execution_id.to_string());
        next.generation = prior.generation + 1;
        next.updated_at = Utc::now();
        self.commit(next)
    }

    /// This attempt terminally did not happen. `reason` is the ORIGINAL
    /// failure — a cleanup error must never mask the root cause, which is the
    /// same rule the rail's rollback follows.
    pub fn fail(
        &self,
        prior: &DispatchIntent,
        reason: &str,
    ) -> std::result::Result<DispatchIntent, DispatchIntentError> {
        let mut next = prior.clone();
        next.state = DispatchIntentState::Failed;
        next.failure_reason = Some(reason.to_string());
        next.generation = prior.generation + 1;
        next.updated_at = Utc::now();
        self.commit(next)
    }

    /// Every unfinished intent, oldest first. This is what a restart walks.
    pub fn list_live(&self) -> Result<Vec<DispatchIntent>> {
        let dir = self.root.join("live");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read dispatch intent outbox {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read dispatch intent outbox {}", path.display()))?;
            let intent: DispatchIntent = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse dispatch intent outbox {}", path.display()))?;
            if self.assert_scope(&intent).is_err() {
                continue;
            }
            out.push(intent);
        }
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.idempotency_key.cmp(&b.idempotency_key))
        });
        Ok(out)
    }

    /// Every run this conversation durably admitted, newest first.
    ///
    /// Reads `intents/` rather than `live/`: the outbox holds only *unfinished*
    /// work, and the run a follow-up continues is almost always a finished one.
    /// That makes this O(runs this scope ever admitted) rather than
    /// O(unfinished) — paid once per `@vibedev` turn, which is a rare event that
    /// is about to start a multi-hour build, against one small JSON per record.
    ///
    /// Ordered by admission time, tie-broken by key so the answer is stable
    /// rather than filesystem-dependent. `chat_session_id` is compared exactly:
    /// it is a server-minted session id, never anything a turn's text supplies.
    /// The admitted run for this task, if this scope started it.
    ///
    /// Used by `run_coding_task` to load the request constraint. Skips
    /// unreadable records the same way [`Self::list_for_chat_session`] does —
    /// a corrupt sibling must not fail an unrelated coding turn.
    pub fn find_by_task_id(&self, task_id: &str) -> Result<Option<DispatchIntent>> {
        let dir = self.root.join("intents");
        if !dir.is_dir() {
            return Ok(None);
        }
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read dispatch intents {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(intent) = serde_json::from_slice::<DispatchIntent>(&bytes) else {
                continue;
            };
            if self.assert_scope(&intent).is_err() {
                continue;
            }
            if intent.task_id == task_id {
                return Ok(Some(intent));
            }
        }
        Ok(None)
    }

    pub fn list_for_chat_session(&self, chat_session_id: &str) -> Result<Vec<DispatchIntent>> {
        let dir = self.root.join("intents");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read dispatch intents {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // A record this build cannot parse is skipped rather than fatal:
            // failing here would stop a conversation from starting a run at all,
            // and the honest degradation is "this turn starts a root run".
            let Ok(intent) = serde_json::from_slice::<DispatchIntent>(&bytes) else {
                continue;
            };
            if self.assert_scope(&intent).is_err() {
                continue;
            }
            if intent.chat_session_id != chat_session_id {
                continue;
            }
            out.push(intent);
        }
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.idempotency_key.cmp(&a.idempotency_key))
        });
        Ok(out)
    }

    /// Rebuild every projection from the journal.
    ///
    /// Run at startup, before walking the outbox. The journal is authoritative,
    /// so this repairs a projection a crash left behind and is a no-op when
    /// everything already agrees. Idempotent by construction: every write is a
    /// whole-value overwrite keyed by the intent's key.
    ///
    /// ## One bad key does not take the scope down
    ///
    /// This used to `?` on the replay, so a single unreplayable key — a torn
    /// transaction, a record this build cannot parse, a permission error on one
    /// directory — aborted the whole loop. The caller then skipped the scope
    /// entirely, and *every* unfinished intent in it became invisible to
    /// recovery, permanently, behind one `warn!`. The blast radius of one
    /// damaged key was every run the scope had ever admitted and not finished.
    ///
    /// So a key that will not replay is logged, counted and skipped — the same
    /// instinct [`DispatchIntentJournal::keys`] already applies to a stray
    /// directory. The `Result` is kept for the failure that genuinely *is*
    /// scope-wide: not being able to enumerate the journal at all.
    pub fn recover(&self) -> Result<DispatchProjectionRepair> {
        let _lock = self.lock("intents")?;
        let mut repair = DispatchProjectionRepair::default();
        for key in self.journal.keys()? {
            let intent = match self.journal.replay(&key) {
                Ok(Some(intent)) => intent,
                Ok(None) => continue,
                Err(error) => {
                    tracing::error!(
                        principal = %self.principal,
                        workspace = %self.workspace,
                        idempotency_key = %key,
                        %error,
                        "[VIBEDEV-RAIL] dispatch intent could not be replayed; skipping the key \
                         rather than the scope"
                    );
                    repair.unreadable.push(key);
                    continue;
                },
            };
            if self.assert_scope(&intent).is_err() {
                continue;
            }
            if let Err(error) = self.project(&intent) {
                tracing::error!(
                    principal = %self.principal,
                    workspace = %self.workspace,
                    idempotency_key = %key,
                    %error,
                    "[VIBEDEV-RAIL] dispatch intent replayed but could not be projected"
                );
                repair.unreadable.push(key);
                continue;
            }
            repair.repaired.push(key);
        }
        Ok(repair)
    }

    /// Apply a committed record to the projections.
    ///
    /// The outbox half is a presence test, not a separate object: an unfinished
    /// record is in `live/`, a terminal one is not. That is why "retire" here is
    /// a delete and cannot drift from the record's own state.
    fn project(&self, intent: &DispatchIntent) -> Result<()> {
        Self::write_atomic(&self.intent_path(&intent.idempotency_key), intent)?;
        let live = self.live_path(&intent.idempotency_key);
        if intent.state.is_terminal() {
            if live.exists() {
                std::fs::remove_file(&live)
                    .with_context(|| format!("retire dispatch intent {}", live.display()))?;
            }
            return Ok(());
        }
        Self::write_atomic(&live, intent)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> DispatchIntentStore {
        DispatchIntentStore::new(dir.path(), "user", "workspace")
    }

    fn facts<'a>(request: &'a str, project: &'a str) -> DispatchRequestFacts<'a> {
        DispatchRequestFacts {
            request,
            mode: DispatchMode::Build,
            parent_task_id: None,
            project_id: project,
            coding_choice: None,
        }
    }

    fn plan(request: &str) -> DispatchTaskPlan {
        DispatchTaskPlan {
            principal: "user".to_string(),
            workspace: "workspace".to_string(),
            title: format!("VibeDev · {request}"),
            description: format!("VibeDev coding request:\n{request}"),
            owner_agent_id: "cto".to_string(),
            mode: DispatchMode::Build,
            project_id: "proj-1".to_string(),
            parent_task_id: None,
            reference_task_ids: Vec::new(),
            coding_choice: None,
            coding_constraint: None,
            cockpit: None,
        }
    }

    /// The follow-up fields — and the coding choice — must be invisible on a
    /// record that has none.
    ///
    /// Not cosmetics: [`DispatchIntentTransaction::verify`] re-serializes the
    /// record and compares the hash against the one stored with it, so a field
    /// that started appearing in the bytes of records written before it existed
    /// would fail that check — and `read_all` treats a failed check as fatal for
    /// the whole key, which would take a scope's recovery down over an upgrade.
    #[test]
    fn the_follow_up_fields_do_not_change_the_bytes_of_a_record_without_them() {
        let mut root = plan("fix the footer");
        root.project_id = String::new();
        let json = serde_json::to_string(&root).expect("serialize");
        assert!(!json.contains("project_id"), "{json}");
        assert!(!json.contains("parent_task_id"), "{json}");
        assert!(!json.contains("reference_task_ids"), "{json}");
        assert!(!json.contains("coding_choice"), "{json}");
        assert!(!json.contains("coding_constraint"), "{json}");
        assert!(!json.contains("cockpit"), "{json}");

        // …and a record that HAS them still round-trips them.
        let mut follow_up = plan("fix the footer");
        follow_up.parent_task_id = Some("task_parent".to_string());
        follow_up.reference_task_ids = vec!["task_parent".to_string()];
        follow_up.coding_choice = Some(VibeDevCodingChoice::Profile {
            profile_id: "codex-default".to_string(),
        });
        let round_tripped: DispatchTaskPlan =
            serde_json::from_str(&serde_json::to_string(&follow_up).expect("serialize"))
                .expect("parse");
        assert_eq!(round_tripped, follow_up);

        // An old record parses into today's shape with the fields absent.
        let legacy = serde_json::json!({
            "principal": "user",
            "workspace": "workspace",
            "title": "VibeDev · fix the footer",
            "description": "VibeDev coding request:\nfix the footer",
            "owner_agent_id": "cto",
            "mode": "build"
        });
        let parsed: DispatchTaskPlan = serde_json::from_value(legacy).expect("parse legacy");
        assert_eq!(parsed.project_id, "");
        assert_eq!(parsed.parent_task_id, None);
        assert!(parsed.reference_task_ids.is_empty());
        assert_eq!(parsed.coding_choice, None);
    }

    /// The lookup a follow-up is built on: this conversation's own admitted
    /// runs, newest first, and nobody else's.
    #[test]
    fn admitted_runs_are_listed_per_conversation_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        for (session, turn) in [
            ("session-1", "turn-1"),
            ("session-1", "turn-2"),
            ("session-2", "turn-1"),
        ] {
            let key = dispatch_idempotency_key("user", "workspace", session, turn);
            store
                .admit(
                    &key,
                    session,
                    &dispatch_request_digest(&facts("fix the footer", "proj-1")),
                    plan("fix the footer"),
                )
                .expect("admitted");
        }

        let first_session = store.list_for_chat_session("session-1").expect("listed");
        assert_eq!(
            first_session.len(),
            2,
            "another conversation's run is not this one's"
        );
        assert_eq!(
            first_session[0].task_id,
            dispatch_task_id(&dispatch_idempotency_key(
                "user",
                "workspace",
                "session-1",
                "turn-2"
            )),
            "newest first — the run a follow-up continues is the last one started"
        );
        assert_eq!(
            store
                .list_for_chat_session("session-3")
                .expect("listed")
                .len(),
            0
        );

        // A store rooted at another scope sees none of it, before any field is
        // compared: the record's scope is re-checked on load.
        let elsewhere = DispatchIntentStore::new(dir.path(), "user", "other-workspace");
        assert!(elsewhere
            .list_for_chat_session("session-1")
            .expect("listed")
            .is_empty());
    }

    #[test]
    fn a_key_is_stable_for_one_turn_and_distinct_across_turns() {
        let first = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        assert_eq!(
            first,
            dispatch_idempotency_key("user", "workspace", "session-1", "turn-1"),
            "a retry of the same turn must derive the same key"
        );
        assert!(valid_intent_key(&first));

        // Every component participates, so nothing collapses two turns into one.
        for other in [
            dispatch_idempotency_key("user", "workspace", "session-1", "turn-2"),
            dispatch_idempotency_key("user", "workspace", "session-2", "turn-1"),
            dispatch_idempotency_key("user", "other", "session-1", "turn-1"),
            dispatch_idempotency_key("other", "workspace", "session-1", "turn-1"),
        ] {
            assert_ne!(first, other);
        }
    }

    /// Length prefixing is the reason: without it, moving characters across a
    /// field boundary would produce the same key.
    #[test]
    fn adjacent_fields_cannot_impersonate_each_other() {
        assert_ne!(
            dispatch_idempotency_key("user", "workspace", "session", "1turn"),
            dispatch_idempotency_key("user", "workspace", "session1", "turn")
        );
        assert_ne!(
            dispatch_request_digest(&facts("ab", "c")),
            dispatch_request_digest(&facts("a", "bc"))
        );
    }

    #[test]
    fn the_digest_covers_every_fact_the_conflict_rule_names() {
        let base = facts("fix the footer", "proj-1");
        let digest = dispatch_request_digest(&base);

        let mut request = base;
        request.request = "fix the header";
        assert_ne!(digest, dispatch_request_digest(&request));

        let mut mode = base;
        mode.mode = DispatchMode::Plan;
        assert_ne!(digest, dispatch_request_digest(&mode));

        let mut parent = base;
        parent.parent_task_id = Some("task_parent");
        assert_ne!(digest, dispatch_request_digest(&parent));

        let mut project = base;
        project.project_id = "proj-2";
        assert_ne!(digest, dispatch_request_digest(&project));

        let mut choice = base;
        choice.coding_choice = Some("codex");
        assert_ne!(digest, dispatch_request_digest(&choice));

        // Absent and present-but-empty are different requests.
        let mut empty_choice = base;
        empty_choice.coding_choice = Some("");
        assert_ne!(digest, dispatch_request_digest(&empty_choice));
    }

    #[test]
    fn a_derived_task_id_is_deterministic_and_ordinary_looking() {
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let task_id = dispatch_task_id(&key);
        assert_eq!(task_id, dispatch_task_id(&key));
        assert!(task_id.starts_with("task_"));
        assert_eq!(task_id.len(), "task_".len() + 32);
        assert!(task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'));
        assert_ne!(
            task_id,
            dispatch_task_id(&dispatch_idempotency_key(
                "user",
                "workspace",
                "session-1",
                "turn-2"
            ))
        );
    }

    #[test]
    fn find_by_task_id_returns_the_admitted_run_and_not_a_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let first_key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let second_key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-2");
        store
            .admit(
                &first_key,
                "session-1",
                &dispatch_request_digest(&facts("fix the footer", "proj-1")),
                plan("fix the footer"),
            )
            .expect("first");
        store
            .admit(
                &second_key,
                "session-1",
                &dispatch_request_digest(&facts("fix the header", "proj-1")),
                plan("fix the header"),
            )
            .expect("second");
        let first_id = dispatch_task_id(&first_key);
        let found = store
            .find_by_task_id(&first_id)
            .expect("lookup")
            .expect("present");
        assert_eq!(found.task_id, first_id);
        assert_eq!(
            found.task_plan.as_ref().map(|plan| plan.title.as_str()),
            Some("VibeDev · fix the footer")
        );
        assert!(store
            .find_by_task_id("task_does_not_exist")
            .expect("missing")
            .is_none());
    }

    #[test]
    fn a_malformed_key_is_refused_rather_than_used_as_a_directory_name() {
        let too_short = format!("vdi-{}", "a".repeat(63));
        let upper_case = format!("vdi-{}", "A".repeat(64));
        for key in [
            "",
            "vdi-",
            "vdi-../../etc",
            "../../etc",
            "vdi-ZZZZ",
            too_short.as_str(),
            upper_case.as_str(),
        ] {
            assert!(!valid_intent_key(key), "{key}");
        }
        assert!(valid_intent_key(&format!("vdi-{}", "0f".repeat(32))));
    }

    #[test]
    fn admitting_twice_with_the_same_request_writes_nothing_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));

        let first = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();
        assert!(!first.is_replay());
        let second = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();
        assert!(second.is_replay());
        assert_eq!(first.intent(), second.intent());

        // One admission, one transaction. A second write here would be a second
        // run.
        assert_eq!(store.journal().read_all(&key).unwrap().len(), 1);
    }

    #[test]
    fn reusing_a_key_for_a_different_request_conflicts_and_mutates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let admitted = store
            .admit(
                &key,
                "session-1",
                &dispatch_request_digest(&facts("fix the footer", "proj-1")),
                plan("fix the footer"),
            )
            .unwrap()
            .intent()
            .clone();

        let error = store
            .admit(
                &key,
                "session-1",
                &dispatch_request_digest(&facts("delete the footer", "proj-1")),
                plan("delete the footer"),
            )
            .expect_err("a different request under the same key is a conflict");
        assert!(matches!(error, DispatchIntentError::Conflict { .. }));

        // Field-by-field, not "an error came back". Including the plan: the
        // conflicting call's task must not overwrite the admitted one, or a
        // restart would build what was refused.
        let stored = store.find(&key).unwrap().unwrap();
        assert_eq!(stored, admitted);
        assert_eq!(stored.task_plan, Some(plan("fix the footer")));
        assert_eq!(store.journal().read_all(&key).unwrap().len(), 1);
    }

    #[test]
    fn the_lifecycle_projects_and_retires_the_outbox_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));

        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        assert_eq!(admitted.state, DispatchIntentState::Pending);
        assert_eq!(store.list_live().unwrap().len(), 1);

        let claimed = store.claim(&admitted, "worker-1").unwrap();
        assert_eq!(claimed.state, DispatchIntentState::Claimed);
        assert_eq!(claimed.generation, 1);
        assert_eq!(store.list_live().unwrap().len(), 1);

        let settled = store.settle(&claimed, "exec-1").unwrap();
        assert_eq!(settled.state, DispatchIntentState::Settled);
        assert_eq!(settled.execution_id.as_deref(), Some("exec-1"));
        assert!(
            store.list_live().unwrap().is_empty(),
            "a settled intent leaves the outbox, so nothing re-offers it"
        );
        // Terminal, but still answerable — that is what a retry reads.
        assert_eq!(store.find(&key).unwrap().unwrap(), settled);
    }

    #[test]
    fn a_superseded_writer_cannot_commit_over_its_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();

        let claimed = store.claim(&admitted, "worker-1").unwrap();
        // The stalled worker still holds the pre-claim view.
        let stale = store.claim(&admitted, "worker-0");
        assert!(stale.is_err(), "a stale generation must be refused");
        assert_eq!(store.find(&key).unwrap().unwrap(), claimed);
    }

    #[test]
    fn a_terminal_intent_refuses_any_successor() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        let claimed = store.claim(&admitted, "worker-1").unwrap();
        let settled = store.settle(&claimed, "exec-1").unwrap();

        assert!(store.claim(&settled, "worker-2").is_err());
        assert!(store.fail(&settled, "too late").is_err());
        assert_eq!(store.find(&key).unwrap().unwrap(), settled);
    }

    #[test]
    fn recovery_rebuilds_a_projection_a_crash_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        let claimed = store.claim(&admitted, "worker-1").unwrap();

        // Simulate a crash between the journal commit and the projection by
        // throwing the projections away. The journal is the truth.
        std::fs::remove_file(store.intent_path(&key)).unwrap();
        std::fs::remove_file(store.live_path(&key)).unwrap();
        assert!(store.find(&key).unwrap().is_none());

        assert_eq!(store.recover().unwrap().repaired, vec![key.clone()]);
        assert!(store.recover().unwrap().unreadable.is_empty());
        assert_eq!(store.find(&key).unwrap().unwrap(), claimed);
        assert_eq!(store.list_live().unwrap().len(), 1);

        // And recovery over a consistent tree changes nothing.
        store.recover().unwrap();
        assert_eq!(store.find(&key).unwrap().unwrap(), claimed);
    }

    #[test]
    fn recovery_does_not_re_offer_a_settled_intent() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        let claimed = store.claim(&admitted, "worker-1").unwrap();
        store.settle(&claimed, "exec-1").unwrap();

        // Even if a stale outbox file survives, replay retires it.
        DispatchIntentStore::write_atomic(&store.live_path(&key), &claimed).unwrap();
        assert_eq!(store.list_live().unwrap().len(), 1);
        store.recover().unwrap();
        assert!(store.list_live().unwrap().is_empty());
    }

    #[test]
    fn a_torn_or_edited_transaction_is_refused_not_half_applied() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();

        let path = store.journal().txn_path(&key, 0);
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["intent"]["state"] = serde_json::json!("settled");
        std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        assert!(store.journal().read_all(&key).is_err());
        // The edited key is refused — and reported — but `recover` itself still
        // succeeds. Aborting here is what used to make one damaged record hide
        // every other unfinished intent in the scope.
        let repair = store.recover().expect("one bad key is not a scope failure");
        assert_eq!(repair.unreadable, vec![key]);
        assert!(repair.repaired.is_empty());
    }

    /// **The blast radius, asserted.** One unreplayable key must not hide the
    /// scope's other intents: the healthy ones are still projected and still in
    /// the outbox, and the damaged one is named rather than swallowed.
    #[test]
    fn one_unreplayable_key_does_not_hide_the_rest_of_the_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let poisoned = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let healthy = dispatch_idempotency_key("user", "workspace", "session-1", "turn-2");
        for key in [&poisoned, &healthy] {
            store
                .admit(key, "session-1", &digest, plan("fix the footer"))
                .unwrap();
        }

        // Damage exactly one key's journal.
        let path = store.journal().txn_path(&poisoned, 0);
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["intent"]["request_digest"] = serde_json::json!("tampered");
        std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        // …and throw the healthy key's projections away, so recovering it is
        // something that has to actually happen rather than already be true.
        std::fs::remove_file(store.intent_path(&healthy)).unwrap();
        std::fs::remove_file(store.live_path(&healthy)).unwrap();

        let repair = store.recover().expect("the scope is still recoverable");
        assert_eq!(repair.unreadable, vec![poisoned.clone()]);
        assert_eq!(repair.repaired, vec![healthy.clone()]);
        let outbox = store
            .list_live()
            .unwrap()
            .into_iter()
            .map(|intent| intent.idempotency_key)
            .collect::<Vec<_>>();
        assert!(
            outbox.contains(&healthy),
            "the healthy intent is back in the outbox where recovery can find it: {outbox:?}"
        );
    }

    /// **The crash the whole module exists for, on the admit path itself.**
    ///
    /// The journal commits before the projection, so a process that dies between
    /// them leaves a key that is durably admitted and invisible to `find`. The
    /// retry must not admit a second time: a second admission lands at sequence
    /// 1 with its own `created_at`, which `validate_successor` refuses forever —
    /// the key would be permanently unreplayable.
    #[test]
    fn an_admitted_but_unprojected_key_is_not_admitted_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();

        // The crash: the journal transaction is durable, the projections are
        // not.
        std::fs::remove_file(store.intent_path(&key)).unwrap();
        std::fs::remove_file(store.live_path(&key)).unwrap();

        let retry = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .expect("the retry is answered, not refused");
        assert!(retry.is_replay(), "the journal already holds this key");
        assert_eq!(retry.intent(), &admitted);

        // ONE admission. A second would be the append that poisons the chain.
        let history = store.journal().read_all(&key).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].sequence, 0);
        // …and the chain still folds, which is the property the second
        // admission would have destroyed.
        assert_eq!(
            store.journal().replay(&key).unwrap().as_ref(),
            Some(&admitted)
        );

        // The projection was repaired on the way past, so the next call needs
        // no journal read at all.
        assert_eq!(store.find(&key).unwrap().as_ref(), Some(&admitted));
        assert_eq!(store.list_live().unwrap().len(), 1);

        // And the conflict rule still applies through the journal, not only
        // through the cache.
        std::fs::remove_file(store.intent_path(&key)).unwrap();
        let conflict = store
            .admit(
                &key,
                "session-1",
                &dispatch_request_digest(&facts("delete the footer", "proj-1")),
                plan("delete the footer"),
            )
            .expect_err("a different request under an admitted key is still a conflict");
        assert!(matches!(conflict, DispatchIntentError::Conflict { .. }));
        assert_eq!(store.journal().read_all(&key).unwrap().len(), 1);
    }

    /// A projection that cannot be READ is not a projection that is absent.
    /// `Path::exists()` answered `false` for both, and `admit` turned the second
    /// into "nothing was admitted under this key".
    #[test]
    fn an_unreadable_projection_is_an_error_rather_than_an_absent_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();

        // A directory where the record's file belongs: `read` fails with
        // something that is not `NotFound`, which is the whole class
        // `Path::exists()` used to fold into "absent" (EIO, EACCES, ENOTDIR, an
        // unmounted volume). Reproducing the class portably is the point; which
        // errno gets there is not.
        let path = store.intent_path(&key);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();

        assert!(
            store.find(&key).is_err(),
            "a record that cannot be read must not read as a record that is not there"
        );
    }

    /// The plan is committed **with** the admission and survives every later
    /// transition, because it is the only thing a restart can rebuild the run
    /// from. A successor that rewrote it is refused.
    #[test]
    fn the_admitted_task_plan_is_durable_and_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();

        // Durable from the first transaction, not from the claim: the whole
        // point is the window before anything else exists.
        assert_eq!(admitted.task_plan, Some(plan("fix the footer")));
        assert_eq!(
            store.journal().replay(&key).unwrap().unwrap().task_plan,
            Some(plan("fix the footer"))
        );

        let claimed = store.claim(&admitted, "worker-1").unwrap();
        assert_eq!(claimed.task_plan, Some(plan("fix the footer")));

        let mut rewritten = claimed.clone();
        rewritten.task_plan = Some(plan("delete the footer"));
        rewritten.generation = claimed.generation + 1;
        assert!(
            store.commit(rewritten).is_err(),
            "a successor may not rewrite what was admitted"
        );
        assert_eq!(store.find(&key).unwrap().unwrap(), claimed);
    }

    /// A record written before the plan existed still loads, and reads as
    /// "nothing to rebuild from". Refusing to parse it instead would take the
    /// whole scope's recovery down over one legacy record.
    #[test]
    fn a_record_without_a_plan_still_loads_and_reports_none() {
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let mut raw = serde_json::to_value(DispatchIntent::admitted(
            &key,
            "user",
            "workspace",
            "session-1",
            "digest",
            plan("fix the footer"),
            Utc::now(),
        ))
        .unwrap();
        let object = raw.as_object_mut().unwrap();
        object.remove("task_plan");
        object.remove("recovery_attempts");

        let legacy: DispatchIntent = serde_json::from_value(raw).unwrap();
        assert!(legacy.task_plan.is_none());
        assert_eq!(legacy.recovery_attempts, 0);
    }

    /// Only recovery charges an attempt. The turn that admitted the run has a
    /// live caller and is not a retry of anything, so its claim must not spend
    /// one of the crash-loop budget.
    ///
    /// Each pass claims what a **previous** process left behind, which is the
    /// only shape recovery can take: a claim carrying this process's instance is
    /// live, and `claim_for_recovery` refuses it. So the attempt count advances
    /// once per restart, exactly as its name says.
    #[test]
    fn only_recovery_claims_charge_an_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        assert_eq!(admitted.recovery_attempts, 0);

        let claimed = store
            .claim_as_a_previous_process(&admitted, "worker-1", false)
            .unwrap();
        assert_eq!(claimed.recovery_attempts, 0);

        let first_recovery = store.claim_for_recovery(&claimed, "recovery").unwrap();
        assert_eq!(first_recovery.recovery_attempts, 1);
        // The next restart: this process's own claim is gone with it.
        let abandoned = store
            .claim_as_a_previous_process(&first_recovery, "recovery", false)
            .unwrap();
        let second_recovery = store.claim_for_recovery(&abandoned, "recovery").unwrap();
        assert_eq!(second_recovery.recovery_attempts, 2);

        // And it is durable, so a process that died mid-attempt still spent it.
        assert_eq!(store.find(&key).unwrap().unwrap().recovery_attempts, 2);

        // The count only moves forward: a writer holding an older view cannot
        // hand back the attempts it already spent.
        let mut rewound = second_recovery.clone();
        rewound.recovery_attempts = 0;
        rewound.generation = second_recovery.generation + 1;
        assert!(store.commit(rewound).is_err());
    }

    /// **The claim the reconciler must not steal.**
    ///
    /// `Claimed -> Claimed` is legal so recovery can take over an abandoned
    /// claim, and the reconciler runs while the server is already serving turns
    /// — so without this check it could take the claim of a turn that is inside
    /// `start_execution` right now. Both would dispatch, one would lose, and the
    /// loser's rollback deletes the winner's task.
    #[test]
    fn recovery_refuses_a_claim_this_process_is_holding() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();

        // A live turn in THIS process owns finishing it.
        let live = store.claim(&admitted, "chat_vibedev_rail").unwrap();
        assert!(
            live.claimed_by
                .as_deref()
                .expect("a claim records its holder")
                .ends_with(dispatch_process_instance_id()),
            "the claim must record the process that made it: {:?}",
            live.claimed_by
        );

        let refused = store
            .claim_for_recovery(&live, "vibedev_rail_startup_recovery")
            .expect_err("a live claim is not an abandoned one");
        assert!(refused.is_held_by_this_process(), "{refused}");
        assert!(refused.conflict().is_none());

        // A refusal writes NOTHING: same generation, same holder, and no
        // attempt charged against a run that is going fine.
        let stored = store.find(&key).unwrap().unwrap();
        assert_eq!(stored, live);
        assert_eq!(stored.recovery_attempts, 0);
        assert_eq!(store.journal().read_all(&key).unwrap().len(), 2);
    }

    /// …and the case the refusal must NOT swallow: a claim from a process that
    /// is gone is still reclaimable, immediately, with no TTL to wait out.
    #[test]
    fn recovery_still_reclaims_a_claim_from_a_previous_process() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        let admitted = store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        let abandoned = store
            .claim_as_a_previous_process(&admitted, "chat_vibedev_rail", false)
            .unwrap();

        let reclaimed = store
            .claim_for_recovery(&abandoned, "vibedev_rail_startup_recovery")
            .expect("a claim outlives its process only by crashing");
        assert_eq!(reclaimed.state, DispatchIntentState::Claimed);
        assert_eq!(reclaimed.recovery_attempts, 1);
        assert!(reclaimed
            .claimed_by
            .as_deref()
            .expect("holder")
            .ends_with(dispatch_process_instance_id()));

        // A never-claimed intent is reclaimable too — there is no holder to be
        // suspicious of.
        let other = dispatch_idempotency_key("user", "workspace", "session-1", "turn-2");
        let pending = store
            .admit(&other, "session-1", &digest, plan("fix the footer"))
            .unwrap()
            .intent()
            .clone();
        assert!(store
            .claim_for_recovery(&pending, "vibedev_rail_startup_recovery")
            .is_ok());
    }

    /// **The forward-compat hazard, from disk.**
    ///
    /// The existing round-trip test builds the legacy record in memory and
    /// digests it with today's serializer, so its bytes and its digest agree by
    /// construction — it could not have caught this. The failure is a record
    /// whose *stored bytes* lack the key: `verify` re-serializes and compares,
    /// so a field that started appearing where it was absent fails the integrity
    /// check and takes the whole key's replay down.
    #[test]
    fn a_record_stored_without_the_newer_fields_still_replays_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();

        // Rewrite the committed transaction the way a build that predates
        // `task_plan` and `recovery_attempts` wrote it.
        let path = store.journal().txn_path(&key, 0);
        let stored: DispatchIntentTransaction =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut legacy = stored.intent;
        legacy.task_plan = None;
        legacy.recovery_attempts = 0;
        let legacy_txn = DispatchIntentTransaction::new(0, legacy).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy_txn).unwrap()).unwrap();

        // THE precondition, and the half an in-memory round-trip cannot check:
        // the keys are absent from the BYTES. Without `skip_serializing_if` they
        // would be here as `null` and `0`, the record would not match a legacy
        // digest, and this whole class of record would be unreadable.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("task_plan"), "{on_disk}");
        assert!(!on_disk.contains("recovery_attempts"), "{on_disk}");

        let history = store
            .journal()
            .read_all(&key)
            .expect("a record written before these fields existed must still read");
        assert_eq!(history.len(), 1);
        assert!(history[0].intent.task_plan.is_none());
        assert_eq!(history[0].intent.recovery_attempts, 0);
        assert!(store.journal().replay(&key).unwrap().is_some());

        // …and the scope-level walk sees it as ordinary, not as damage.
        let repair = store.recover().expect("recovered");
        assert_eq!(repair.repaired, vec![key]);
        assert!(repair.unreadable.is_empty());
    }

    #[test]
    fn a_record_from_another_scope_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let key = dispatch_idempotency_key("user", "workspace", "session-1", "turn-1");
        let digest = dispatch_request_digest(&facts("fix the footer", "proj-1"));
        store
            .admit(&key, "session-1", &digest, plan("fix the footer"))
            .unwrap();

        let other = DispatchIntentStore::new(dir.path(), "someone-else", "workspace");
        assert!(other.find(&key).is_err());
        assert!(other.list_live().unwrap().is_empty());
    }
}
