//! Tracing subscriber layer that turns spans and INFO+ events into the
//! `Activity` family of [`RuntimeTransportEvent`]s.
//!
//! This is the sibling of [`super::tracing_layer::AnalyticsTracingLayer`]
//! and deliberately reads like it: same `Layer` shape, same INFO+ filter,
//! the same shared noisy-target skip list, the same message visitor.
//! What it adds is the part that layer does not have — span hooks. A span
//! open becomes [`RuntimeTransportEvent::ActivityStarted`], the matching
//! close becomes [`RuntimeTransportEvent::ActivityFinished`] with a
//! duration, and every INFO+ log line becomes
//! [`RuntimeTransportEvent::ActivityProgress`] hung off whichever span it
//! was emitted inside.
//!
//! ## Why a layer rather than hand-emitted events
//!
//! The failure this exists to fix is "the worker did not emit". Under a
//! layer, being observable is a property of running inside a span rather
//! than something each author has to remember. Background work —
//! distillation, classification, memory consolidation, taste profiling —
//! shows up because it inherits context.
//!
//! ## The span floor
//!
//! Work that runs outside any instrumented span still produces progress
//! rows; they simply have no `activity_id` and render loose at the root.
//! That is honest, not a bug. The tree is only as good as the boundary
//! set, and [`RuntimeTransportEvent::ActivityProgress::activity_id`] is
//! optional for exactly this reason.
//!
//! ## Interaction with the process log filter
//!
//! This layer is registered with its **own** per-layer filter
//! (`Layer::with_filter`), pinned to the process's baseline directives —
//! not with the operator's `--log-level`. An `EnvFilter` added as a bare
//! *layer* filters the whole subscriber rather than just the layers after
//! it, so sharing one would make this view's completeness a property of
//! how somebody launched the process: `--log-level warn` would empty it
//! with no error and no gap marker. See `init_tracing` in
//! `magician/src/bin/magician.rs`, which spells out the trade.
//!
//! Two things follow. The "all spans" rule below is in practice "all INFO+
//! spans", because the pinned filter admits nothing lower, and a target
//! pinned to `warn` in the baseline (`lance`, `lance_index`) contributes
//! no activity rows. And this layer sees events a quieted console does
//! not — intended, not a leak: absence here should mean the work did not
//! happen, never that somebody passed a flag.
//!
//! ## Privacy
//!
//! This family crosses a websocket to a browser with no redaction pass in
//! between. `name`, `target` and `kind` are code identifiers by
//! construction — the first two come from `Metadata`, and `kind` is
//! normalised onto the closed [`ACTIVITY_KINDS`] set so a span field
//! cannot inject free text into it. `outcome` is likewise normalised to
//! one of four tokens, so an error *body* can never ride out on it.
//!
//! `message` is the one free-form field, and this layer forwards exactly
//! what `tracing_layer::MessageVisitor` already forwards — the `message`
//! field and no other. Do not widen it: every additional structured field
//! captured here becomes browser-visible.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, SpanRef};
use tracing_subscriber::{Layer, Registry};

use crate::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

use super::tracing_layer::{is_noisy_target, MessageVisitor};

/// Depth of the layer's bounded queue.
///
/// Sized against the measured event rate this runtime actually produces —
/// ~26 INFO lines/minute average, 496 in the busiest observed minute — so
/// 4096 is several minutes of headroom for a consumer that briefly stalls
/// and still a hard ceiling on memory.
pub const ACTIVITY_CHANNEL_CAPACITY: usize = 4096;

/// Cap on the bytes of a log message that ride to the browser.
///
/// This narrows what the precedent layer forwards rather than widening
/// it: the queue holds up to [`ACTIVITY_CHANNEL_CAPACITY`] records, so an
/// uncapped message length makes "bounded queue" bounded only in the count
/// dimension. Truncation is marked, never silent.
const MAX_MESSAGE_BYTES: usize = 2048;

/// This module's own tracing target. Written as `module_path!()` so a
/// rename or move cannot silently un-guard the layer against itself.
const SELF_TARGET: &str = module_path!();

/// Targets on this layer's *own emit path*, which must never be turned
/// back into activity events.
///
/// Every prefix here was read out of the code rather than guessed, and
/// each one handles **every** event the layer emits, so a log line from it
/// is a per-event loop, not an occasional one:
///
/// - `analytics` — the explicit target the analytics dispatcher and sink
///   log under (`analytics/mod.rs` `pool_for_scope`). Inherited from
///   [`super::tracing_layer::AnalyticsTracingLayer`]'s guard; the
///   observability plane's own failures are not runtime activity.
/// - `…::realtime_events` — the broadcaster itself. `emit_transport_only`
///   and `emit` both `error!` on a rejected HITL payload and `warn!` on a
///   dropped chat presentation, on the same call this layer makes.
/// - `…::transport_log` — subscribes to the broadcaster and persists
///   *every* event, `warn!`-ing per event on an append failure. With a
///   failing disk this is a tight one-in-one-out loop.
/// - `…::api::websocket_handler` — the socket fan-out, which sends every
///   scope-visible event and logs at INFO/WARN/ERROR throughout the send
///   and heartbeat paths.
/// - this module — its own forwarder's failures.
///
/// **Deliberately not listed:** the ~18 other broadcaster subscribers
/// (`mobile_push`, `execution_panel::projector`, `chat::service`,
/// `feed::materializer`, …) that `warn!` on `RecvError::Lagged`. Those
/// fire once per *lag episode*, not once per event, so they cannot
/// multiply — and bus lag is exactly the kind of degradation an operator
/// watching this view should see rather than have hidden.
const EMIT_PATH_TARGET_PREFIXES: &[&str] = &[
    "analytics",
    "magician::magician_v2::realtime_events",
    "magician::magician_v2::transport_log",
    "magician::magician_v2::api::websocket_handler",
    SELF_TARGET,
];

/// Whether a target must not produce activity events.
///
/// Split out as a free function so the guard can be asserted directly
/// rather than only through the side effect of an emitted event.
pub fn should_skip_target(target: &str) -> bool {
    // Allowlist first: anything this runtime did not emit never becomes an
    // activity event, whatever its level.
    if !is_first_party_target(target) {
        return true;
    }
    // Then the two first-party exclusions. `is_noisy_target` is largely
    // subsumed by the allowlist — no third-party crate root is on it — but
    // it stays because it is shared with the analytics layer.
    if is_noisy_target(target) {
        return true;
    }
    EMIT_PATH_TARGET_PREFIXES
        .iter()
        .any(|prefix| target.starts_with(prefix))
}

/// Target roots this runtime's own code emits under.
///
/// An **allowlist**, and the reason is the wire: `message` is free-form and
/// reaches a browser with no redaction pass. Admitting every target at INFO
/// put every dependency's INFO output — `reqwest`, `duckdb`, `sqlx`, the
/// provider SDKs — on that wire, guarded only by allowlist-by-exception
/// (`rmcp::transport::auth` capped because it was *known* to log
/// credentials). That is backwards: it protects only the leaks somebody
/// already found.
///
/// Two kinds of entry, because this runtime emits under two kinds of
/// target:
///
/// - Crate roots, in `module_path!()` form with underscores. These cover
///   every span and event that does not name a target explicitly.
/// - Explicit `target: "…"` strings. These are first-party *by
///   construction* — only this workspace's source contains them — but they
///   are arbitrary words (`meet_bot`, `screen_capture`, `entities`) with
///   nothing structural marking them as ours, so they must be listed.
///
/// Derived by enumerating `target:` literals across the workspace's Rust
/// sources and taking the root segment of each; regenerate the same way
/// when adding one. **A target absent from this list produces no activity
/// at all**, which is a silent loss — the failure mode this view exists to
/// remove. Adding a new custom target means adding it here.
///
/// Sorted; `is_first_party_target` binary-searches it.
const FIRST_PARTY_TARGET_ROOTS: &[&str] = &[
    "agentic",
    "ambient",
    "analytics",
    "archive",
    "artifact_v2",
    "browser_cleanup",
    "coding",
    "coding_engine",
    "config",
    "contribute_to_project",
    "dashboard_themes",
    "document_to_markdown_cli",
    "entities",
    "environment_knowledge",
    "failure_context",
    "feed",
    "flat_loop",
    "insights",
    "knowledge",
    "local_tier",
    "magic_supervisor",
    "magician",
    "magician_core",
    "magician_event_taxonomy",
    "magician_mcp_client",
    "magician_pty",
    "magician_vector_index",
    "magicllm",
    "magicutor",
    "mcp_oauth_lifecycle",
    "meet_bot",
    "memory_index",
    "notes",
    "ollama_lifecycle",
    "outer_loop_multiturn",
    "outer_prompt_projection",
    "primitive_cli",
    "recent_activity",
    "report",
    "runtime_core",
    "screen_capture",
    "screen_observe",
    "skills",
    "storage_governance",
    "success_patterns",
    "task",
    "task_api_v3",
    "task_progress",
    "tool_runtime_core",
    "user",
    "voice_cost",
];

/// The leading segment of a target, up to the first `:` or `.`.
///
/// Matched as a whole segment rather than by `starts_with` so an entry
/// cannot accidentally admit an unrelated crate that merely shares a
/// prefix — `magician` must not vouch for a third-party `magicianfoo`.
fn target_root(target: &str) -> &str {
    let end = target
        .find(|c| c == ':' || c == '.')
        .unwrap_or(target.len());
    &target[..end]
}

/// Whether this runtime — rather than one of its dependencies — emitted
/// under `target`.
pub fn is_first_party_target(target: &str) -> bool {
    FIRST_PARTY_TARGET_ROOTS
        .binary_search(&target_root(target))
        .is_ok()
}

// ─────────────────────────────────────────────────────────────────────
// Kinds
// ─────────────────────────────────────────────────────────────────────

pub const KIND_AGENT: &str = "agent";
pub const KIND_BACKGROUND: &str = "background";
pub const KIND_CAPABILITY: &str = "capability";
pub const KIND_LLM: &str = "llm";
pub const KIND_PROCESS: &str = "process";
pub const KIND_RUNTIME: &str = "runtime";

/// The closed set of coarse families the view groups by.
///
/// Closed on purpose: `kind` is documented as a code identifier that
/// crosses to the browser, and normalising against this list makes that
/// literally true instead of true by convention. A span declaring an
/// unrecognised `activity_kind` falls back to the target heuristic rather
/// than putting its own string on the wire.
pub const ACTIVITY_KINDS: &[&str] = &[
    KIND_AGENT,
    KIND_BACKGROUND,
    KIND_CAPABILITY,
    KIND_LLM,
    KIND_PROCESS,
    KIND_RUNTIME,
];

/// Map a declared `activity_kind` field onto [`ACTIVITY_KINDS`].
fn canonical_kind(declared: &str) -> Option<&'static str> {
    ACTIVITY_KINDS
        .iter()
        .find(|kind| kind.eq_ignore_ascii_case(declared.trim()))
        .copied()
}

/// Workload classes, mirroring the dispatch layer's own taxonomy.
///
/// **These strings are a join key, not a display label.** They are the same
/// values `llm_dispatch_batch.workload_class` already stores, so a live
/// activity row and an analytical dispatch row group under the same name and
/// can be joined without a translation table. Renaming one without the other
/// silently breaks that join while erroring nowhere — the same failure mode
/// the `activity_id` note in the archived plan warns about for span ids.
///
/// Adding a value here without adding it to `magicllm::LlmWorkloadClass`
/// produces a class the analytical store can never contain.
///
/// The values are exactly what [`magicllm::LlmWorkloadClass::as_str`] returns —
/// snake_case — because that is the function whose output `llm_dispatch_rows`
/// writes into the `workload_class` parquet column. Reading the Rust *variant*
/// names instead is the mistake that has already been made here: it put
/// `ForegroundChat` on the live event beside a stored `foreground_chat`, which
/// groups correctly in the view and joins against nothing, with nothing
/// erroring. Case folding cannot rescue it either — the difference is an
/// underscore, not a capital. `every_workload_class_matches_the_dispatch_taxonomy`
/// holds the two together from here on, and fails to compile rather than fails
/// to match if a variant is ever added.
pub const WORKLOAD_FOREGROUND_CHAT: &str = "foreground_chat";
pub const WORKLOAD_INTERACTIVE_TASK: &str = "interactive_task";
pub const WORKLOAD_AMBIENT: &str = "ambient";
pub const WORKLOAD_SCHEDULED: &str = "scheduled";
pub const WORKLOAD_AUTONOMOUS_TASK: &str = "autonomous_task";
pub const WORKLOAD_MEMORY: &str = "memory";
pub const WORKLOAD_SYSTEM: &str = "system";
pub const WORKLOAD_EVALUATION: &str = "evaluation";
pub const WORKLOAD_COMMS_ASSIST: &str = "comms_assist";

pub const ACTIVITY_WORKLOAD_CLASSES: &[&str] = &[
    WORKLOAD_FOREGROUND_CHAT,
    WORKLOAD_INTERACTIVE_TASK,
    WORKLOAD_AMBIENT,
    WORKLOAD_SCHEDULED,
    WORKLOAD_AUTONOMOUS_TASK,
    WORKLOAD_MEMORY,
    WORKLOAD_SYSTEM,
    WORKLOAD_EVALUATION,
    WORKLOAD_COMMS_ASSIST,
];

/// Map a declared `workload_class` field onto [`ACTIVITY_WORKLOAD_CLASSES`].
///
/// Case-insensitive on the way in but canonical on the way out, so
/// `Foreground_Chat` and `foreground_chat` group together instead of becoming
/// two lanes. Case is all it folds: `foregroundchat` is *not* accepted, because
/// bridging the underscore would mean accepting a spelling the analytical store
/// does not use and silently rewriting it into one that it does.
fn canonical_workload_class(declared: &str) -> Option<&'static str> {
    ACTIVITY_WORKLOAD_CLASSES
        .iter()
        .find(|class| class.eq_ignore_ascii_case(declared.trim()))
        .copied()
}

/// Coarse default when a span does not declare its own `activity_kind`.
///
/// A heuristic over the module path, and only a default — the boundary
/// spans that matter are expected to say what they are. Ordered so the
/// more specific families win: a consolidator living under `agents::` is
/// background work, not an agent step.
fn default_kind_for_target(target: &str) -> &'static str {
    const LLM: &[&str] = &["magicllm", "llm_dispatch", "llm_chunking", "::llm"];
    const PROCESS: &[&str] = &["governed_runtime", "primitive_dispatch", "child_process"];
    const CAPABILITY: &[&str] = &["capability_invoker", "capabilities", "tool_runtime"];
    const BACKGROUND: &[&str] = &[
        "memory",
        "taste",
        "attention",
        "distill",
        "consolidat",
        "classif",
        "content_sources",
        "observation",
    ];
    const AGENT: &[&str] = &["agents::", "agentic", "orchestrat"];

    let matches = |needles: &[&str]| needles.iter().any(|needle| target.contains(needle));

    if matches(LLM) {
        KIND_LLM
    } else if matches(PROCESS) {
        KIND_PROCESS
    } else if matches(CAPABILITY) {
        KIND_CAPABILITY
    } else if matches(BACKGROUND) {
        KIND_BACKGROUND
    } else if matches(AGENT) {
        KIND_AGENT
    } else {
        KIND_RUNTIME
    }
}

// ─────────────────────────────────────────────────────────────────────
// Outcomes
// ─────────────────────────────────────────────────────────────────────

const OUTCOME_UNSET: u8 = 0;
const OUTCOME_SUCCESS: u8 = 1;
const OUTCOME_ERROR: u8 = 2;
const OUTCOME_CANCELLED: u8 = 3;

/// Normalise a declared `activity_outcome` onto a status *token*.
///
/// The wire field is documented as "a token, not an error body" precisely
/// because error bodies carry user content and this event reaches a
/// browser. Passing every declared outcome through this function is what
/// enforces that: anything unrecognised collapses to `closed`.
fn outcome_code(declared: &str) -> u8 {
    let declared = declared.trim();
    const SUCCESS: &[&str] = &[
        "success",
        "succeeded",
        "ok",
        "done",
        "complete",
        "completed",
    ];
    const ERROR: &[&str] = &["error", "err", "failed", "failure"];
    const CANCELLED: &[&str] = &["cancelled", "canceled", "cancel", "aborted"];

    let matches = |tokens: &[&str]| {
        tokens
            .iter()
            .any(|token| declared.eq_ignore_ascii_case(token))
    };

    if matches(SUCCESS) {
        OUTCOME_SUCCESS
    } else if matches(ERROR) {
        OUTCOME_ERROR
    } else if matches(CANCELLED) {
        OUTCOME_CANCELLED
    } else {
        OUTCOME_UNSET
    }
}

/// The wire token for a stored outcome code.
///
/// `closed` — not `success` — is what an undeclared outcome becomes. The
/// layer watched a span open and close; it did not watch it succeed, and
/// saying otherwise would make every abandoned unit look healthy.
fn outcome_token(code: u8) -> &'static str {
    match code {
        OUTCOME_SUCCESS => "success",
        OUTCOME_ERROR => "error",
        OUTCOME_CANCELLED => "cancelled",
        _ => "closed",
    }
}

// ─────────────────────────────────────────────────────────────────────
// Queued records
// ─────────────────────────────────────────────────────────────────────

/// What the layer enqueues. Deliberately close to the wire shape without
/// being it, so the hot path does the minimum: `name`/`target`/`kind`/
/// `outcome` are `&'static str` borrowed straight from `Metadata` and the
/// constant tables above, and the only allocations are the optional scope
/// strings and the (length-capped) message.
#[derive(Debug, Clone)]
pub enum ActivityRecord {
    Started {
        activity_id: u64,
        parent_activity_id: Option<u64>,
        name: &'static str,
        target: &'static str,
        kind: &'static str,
        principal: Option<String>,
        workspace: Option<String>,
        /// Closed set, so `&'static str` costs nothing to carry.
        workload_class: Option<&'static str>,
        /// `Arc<str>` all the way to the drain: these are inherited, so the
        /// same identifier is typically shared by every span in a subtree and
        /// re-allocating per record would undo that.
        agent_id: Option<Arc<str>>,
        thread_id: Option<Arc<str>>,
        task_id: Option<Arc<str>>,
        model: Option<Arc<str>>,
        /// What this span asked for, on the spans that dispatch something.
        ///
        /// Alone among these, it is **not** inherited and therefore not held on
        /// [`ActivityState`]: an operation names this call, not the subtree,
        /// and handing it to a child would claim the child made a request it
        /// never made. It rides `Started` only — a span declares it at open or
        /// not at all.
        operation: Option<Arc<str>>,
        timestamp: i64,
    },
    /// A span that closed.
    ///
    /// This variant carries the span's whole identity, not just the fields the
    /// wire event needs. The wire only wants `activity_id`, `duration_ms` and
    /// `outcome` — the browser already saw the rest on `Started` and keeps it
    /// client-side. The **durable** consumer
    /// ([`super::activity_rows_sink`]) has no such memory: it writes one row
    /// per completed span and must therefore be handed a self-sufficient
    /// record.
    ///
    /// The alternative — pairing `Started` with `Finished` in the sink — was
    /// rejected. [`ActivityChannel`] evicts *oldest* under pressure, so a
    /// dropped `Started` whose `Finished` survived would silently lose a
    /// completed row, and the pending-start map would need its own bound and
    /// its own eviction policy. Carrying the fields costs a handful of
    /// `&'static str` copies and `Arc` refcount bumps at close; none of it
    /// allocates, and none of it happens inside the queue's critical section.
    Finished {
        activity_id: u64,
        parent_activity_id: Option<u64>,
        /// The top of this span's tracked subtree — itself when it is a root.
        /// Denormalised here so a subtree rollup is a flat `GROUP BY` rather
        /// than a recursive walk over the parent edge on every query.
        root_activity_id: u64,
        name: &'static str,
        target: &'static str,
        kind: &'static str,
        workload_class: Option<&'static str>,
        agent_id: Option<Arc<str>>,
        thread_id: Option<Arc<str>>,
        task_id: Option<Arc<str>>,
        model: Option<Arc<str>>,
        /// Wall-clock open time, recorded at open rather than derived at close.
        /// `timestamp - duration_ms` looks equivalent and is not: the duration
        /// comes from a monotonic `Instant` and the timestamps from the wall
        /// clock, so a clock step between them would produce a start that
        /// disagrees with the `ActivityStarted` row for the same span.
        started_at_ms: i64,
        duration_ms: u64,
        outcome: &'static str,
        principal: Option<String>,
        workspace: Option<String>,
        timestamp: i64,
    },
    Progress {
        activity_id: Option<u64>,
        level: &'static str,
        message: String,
        target: &'static str,
        principal: Option<String>,
        workspace: Option<String>,
        timestamp: i64,
    },
}

impl ActivityRecord {
    /// Convert to the wire event, stamping the current cumulative drop
    /// count and this emission's identity.
    ///
    /// Called by the consumer at drain time rather than by the producer at
    /// enqueue time, so the freshest total rides the newest row.
    ///
    /// `seq` is minted here, at the single point every record passes
    /// through on its way to the bus. It is stamped once and then travels
    /// with the serialized event, so the backfill and live-tail deliveries
    /// of the same event carry the same value — which is the whole point
    /// of having it, since that is what lets a consumer tell "this event
    /// again" from "another event that looks the same".
    pub fn into_transport_event(self, dropped: u64) -> RuntimeTransportEvent {
        let seq = next_record_seq().to_string();
        match self {
            ActivityRecord::Started {
                activity_id,
                parent_activity_id,
                name,
                target,
                kind,
                principal,
                workspace,
                workload_class,
                agent_id,
                thread_id,
                task_id,
                model,
                operation,
                timestamp,
            } => RuntimeTransportEvent::ActivityStarted {
                activity_id: activity_id.to_string(),
                parent_activity_id: parent_activity_id.map(|id| id.to_string()),
                name: name.to_string(),
                target: target.to_string(),
                kind: kind.to_string(),
                principal,
                workspace,
                // The `Arc` is shared up to here and materialised into an owned
                // `String` only at the wire boundary, where serde needs one.
                workload_class: workload_class.map(str::to_string),
                agent_id: agent_id.as_deref().map(str::to_string),
                thread_id: thread_id.as_deref().map(str::to_string),
                task_id: task_id.as_deref().map(str::to_string),
                model: model.as_deref().map(str::to_string),
                operation: operation.as_deref().map(str::to_string),
                dropped,
                seq,
                timestamp,
            },
            // `..` covers the durable-only fields. They exist for
            // [`super::activity_rows_sink`]; the browser already learned them
            // from this span's `ActivityStarted` row, and repeating them here
            // would widen the wire for no reader.
            ActivityRecord::Finished {
                activity_id,
                duration_ms,
                outcome,
                principal,
                workspace,
                timestamp,
                ..
            } => RuntimeTransportEvent::ActivityFinished {
                activity_id: activity_id.to_string(),
                duration_ms,
                outcome: outcome.to_string(),
                principal,
                workspace,
                dropped,
                seq,
                timestamp,
            },
            ActivityRecord::Progress {
                activity_id,
                level,
                message,
                target,
                principal,
                workspace,
                timestamp,
            } => RuntimeTransportEvent::ActivityProgress {
                activity_id: activity_id.map(|id| id.to_string()),
                level: level.to_string(),
                message,
                target: target.to_string(),
                principal,
                workspace,
                dropped,
                seq,
                timestamp,
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// The bounded channel
// ─────────────────────────────────────────────────────────────────────

/// Bounded, drop-oldest queue between the tracing hot path and the event
/// broadcaster.
///
/// [`push`](ActivityChannel::push) is a **synchronous, non-blocking**
/// function by construction: it cannot `await`, and it never waits on
/// capacity — a full queue evicts its oldest record instead. That is the
/// whole point. Observability must never stall the thing it observes, so
/// the layer's back pressure story is "lose the oldest rows and say how
/// many", never "hold up the caller".
///
/// The eviction is *oldest-first* because this feeds a live view: the row
/// describing what the machine is doing right now is worth more than the
/// row describing what it was doing four thousand events ago.
///
/// Nothing inside the critical section logs — a `tracing` call made while
/// holding the queue lock would re-enter this layer on the same thread and
/// deadlock `parking_lot`'s non-reentrant mutex. Keep it that way.
#[derive(Debug)]
pub struct ActivityChannel {
    queue: Mutex<VecDeque<ActivityRecord>>,
    capacity: usize,
    dropped: AtomicU64,
    notify: Notify,
}

impl ActivityChannel {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            dropped: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.queue.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Process-cumulative count of records evicted before they could be
    /// emitted. Monotonic; never reset.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Enqueue a record, evicting the oldest if the queue is full.
    ///
    /// Never blocks on capacity and never fails.
    pub fn push(&self, record: ActivityRecord) {
        {
            let mut queue = self.queue.lock();
            if queue.len() >= self.capacity {
                // `>=` rather than `==` so a capacity that ever shrinks
                // still converges instead of parking above the ceiling.
                let overflow = queue.len() - self.capacity + 1;
                for _ in 0..overflow {
                    queue.pop_front();
                }
                self.dropped.fetch_add(overflow as u64, Ordering::Relaxed);
            }
            queue.push_back(record);
        }
        self.notify.notify_one();
    }

    /// Take the oldest queued record, if any.
    pub fn try_recv(&self) -> Option<ActivityRecord> {
        self.queue.lock().pop_front()
    }

    /// Take the oldest queued record already converted to its wire event,
    /// stamped with the current drop count.
    pub fn try_recv_event(&self) -> Option<RuntimeTransportEvent> {
        let record = self.try_recv()?;
        Some(record.into_transport_event(self.dropped()))
    }

    /// Wait for the next record.
    ///
    /// Race-free without the usual `enable()` dance because
    /// `Notify::notify_one` stores a permit when no waiter is registered:
    /// a `push` landing between the `try_recv` miss and the `await` leaves
    /// the permit behind, so `notified()` completes immediately.
    pub async fn recv(&self) -> ActivityRecord {
        loop {
            if let Some(record) = self.try_recv() {
                return record;
            }
            self.notify.notified().await;
        }
    }
}

impl Default for ActivityChannel {
    fn default() -> Self {
        Self::new(ACTIVITY_CHANNEL_CAPACITY)
    }
}

/// The process-wide channel the registered layer writes into.
///
/// A global because the layer is constructed in `init_tracing`, long
/// before the broadcaster exists; the channel is the seam that lets the
/// producer start before its consumer does. Tests construct their own via
/// [`RuntimeActivityLayer::with_channel`] rather than sharing this one.
static GLOBAL_ACTIVITY_CHANNEL: OnceCell<Arc<ActivityChannel>> = OnceCell::new();

pub fn global_activity_channel() -> Arc<ActivityChannel> {
    Arc::clone(GLOBAL_ACTIVITY_CHANNEL.get_or_init(|| Arc::new(ActivityChannel::default())))
}

/// Drain the channel onto the event bus until `shutdown` fires.
///
/// Uses `emit_transport_only`: activity rows are live telemetry, not
/// durable execution facts, so they do not belong in a per-execution
/// canonical `events.jsonl`. They still survive a refresh — `transport_log`
/// subscribes to the broadcaster and persists every event it sees.
///
/// # The durable tap
///
/// `rows`, when present, is the retrospective store's sink
/// ([`super::activity_rows_sink`]). It is fed **here**, at the drain, rather
/// than inside `on_new_span` / `on_close`: the layer's span hooks run on
/// whatever thread happened to be doing the work, under the registry's
/// extensions lock, and a durable store's buffering — let alone its flush —
/// has no business on that path. What happens here is a field copy and a
/// non-blocking `try_send`; the buffering, the parquet encode and the write
/// all happen on the sink's own task.
///
/// The tap sees only [`ActivityRecord::Finished`]. A row is written once, for
/// a span that has closed, so it is never half-populated — which is also why
/// the finish record carries the span's whole identity rather than just its
/// duration.
///
/// # THESE SENDS ARE NOT JOURNALLED BY THE LOOP OUTBOX, AND THAT IS A REFUSAL
///
/// Recorded here on 2026-08-28. `execution::agentic::run_loop::phases::outbox`
/// keeps a census of *phase-reachable emission sites*, and the third of its
/// three preconditions for cutting inline emission is that every one of them
/// either journals or carries a written-down refusal. This forwarder was in
/// neither half — and could not have been, because that census is built from a
/// call graph and **no phase calls this**.
///
/// A phase causes it, once per instrumented span it opens. The `Layer` half of
/// this module writes an [`ActivityRecord`] into a process-global channel from
/// inside `on_new_span` / `on_close`, on whatever thread held the span; this
/// task drains that channel and emits. Every phase body that runs under an
/// instrumented span is a producer, which makes this — by volume — very likely
/// the largest single emitter the loop drives.
///
/// **Refused rather than owed a diff.** The address is the whole problem, and
/// two facts make it worse than the LLM-queue case in
/// `super::llm_trace_activation::emit_activity_cost` (which carries the same
/// refusal for the same reason):
///
/// - **The channel is process-global and shared by every run.** There is no
///   per-execution seam to hang `(execution_id, iteration, phase)` on;
///   `ActivityRecord` would have to carry all three, from a `Layer` callback
///   that knows only the span.
/// - **An ambient address cannot supply them.** The loop's own
///   *AN AMBIENT ADDRESS CANNOT WORK HERE* shows a `thread_local!` or a
///   `task_local!` carrying `(iteration, phase)` does not survive the
///   `tokio::spawn` inside `SchedulerRootLane` — and this producer is one
///   further hop out than that.
///
/// And the same categorical reason as the cost rows: an activity span is
/// telemetry **about** the process that ran a phase, not an event **of** the
/// run. Replaying a journal is meant to restore the run's timeline; it is not
/// meant to re-narrate the observability of the process that crashed.
///
/// The events still go out on the bus, and `transport_log` still persists them.
/// Only the outbox's population excludes them.
pub fn spawn_activity_forwarder(
    channel: Arc<ActivityChannel>,
    broadcaster: &RuntimeTransportBroadcaster,
    rows: Option<super::activity_rows_sink::ActivityRowsHandle>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let broadcaster = broadcaster.clone();
    tokio::spawn(async move {
        let forward = |record: ActivityRecord| {
            if let Some(rows) = rows.as_ref() {
                rows.submit(&record);
            }
            let dropped = channel.dropped();
            broadcaster.emit_transport_only(record.into_transport_event(dropped));
        };
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                record = channel.recv() => forward(record),
            }
        }

        // Shutdown drain, mirroring `activity_rows_sink::run_sink`.
        //
        // Cancellation used to `break` and leave whatever was still queued in
        // the channel — up to its whole 4096-slot capacity of spans that had
        // already closed. Those losses were invisible: `channel.dropped()`
        // counts capacity evictions in `push`, and a record abandoned at
        // shutdown was never evicted, so the count a viewer reads said the
        // stream was complete while a shutdown had just silently taken the
        // tail off it — including the `Finished` rows the durable spine writes
        // its only rows from.
        //
        // `try_recv` rather than `recv`: a producer still running must not be
        // able to hold shutdown open indefinitely. This drains what the
        // producer has already handed over and stops.
        while let Some(record) = channel.try_recv() {
            forward(record);
        }
    })
}

// ─────────────────────────────────────────────────────────────────────
// Per-span state
// ─────────────────────────────────────────────────────────────────────

/// What the layer stores in a span's extensions.
///
/// The minimum needed to close the span honestly: an id that outlives
/// `tracing`'s recycled span ids, the open instant for the duration, the
/// scope so the finish routes to the same workspace as the start, and one
/// byte of outcome. Nothing here grows with the span's field count or
/// lifetime.
#[derive(Debug)]
struct ActivityState {
    /// Process-monotonic, unlike `tracing::span::Id`, which the registry
    /// recycles after close — recycled ids would silently merge unrelated
    /// units in a long-lived view.
    activity_id: u64,
    parent_activity_id: Option<u64>,
    /// The nearest tracked ancestor's root, or this span's own id when it has
    /// none. Resolved once at open, so the walk is O(1) per span rather than
    /// O(depth): an ancestor's root was itself resolved this way when it
    /// opened, exactly like scope inheritance.
    root_activity_id: u64,
    /// Borrowed from `Metadata`, so holding them costs nothing beyond the fat
    /// pointer. They are here for the durable close record — the live wire
    /// already carried them on `ActivityStarted`.
    name: &'static str,
    target: &'static str,
    kind: &'static str,
    /// Wall-clock open time. Kept beside `opened_at` rather than instead of
    /// it: the monotonic instant is what makes `duration_ms` immune to a clock
    /// step, and this is what makes the row's start agree with the start the
    /// live view already saw.
    started_at_ms: i64,
    opened_at: Instant,
    principal: Option<String>,
    workspace: Option<String>,
    outcome: AtomicU8,
    /// Closed set, so `&'static str` — copying one is free.
    workload_class: Option<&'static str>,
    /// `Arc<str>` rather than `String` because these are **inherited**: every
    /// span beneath a declaring root copies them, so a deep subtree under one
    /// agent turn would otherwise pay an allocation per identifier per span.
    /// A refcount bump is the whole cost instead. Unlike `outcome` these are
    /// fixed at open and never mutated, so no atomics.
    agent_id: Option<Arc<str>>,
    thread_id: Option<Arc<str>>,
    task_id: Option<Arc<str>>,
    model: Option<Arc<str>>,
}

/// Activity ids are unique across process restarts, not just within one
/// run.
///
/// A counter starting at `1` every boot is not enough. The id is a join key
/// in two places that both outlive the process that minted it:
///
/// - the runtime activity view backfills a 24h window off the on-disk
///   transport log, so one page legitimately contains spans from several
///   earlier runs — every one of which had reused `1, 2, 3, …`. Colliding
///   ids merge unrelated spans into one node: the second span's name,
///   target, kind and parent are discarded, its log lines pile into the
///   first span's list, and its finish overwrites the first's duration and
///   outcome. Silent, with no visual cue.
/// - `LlmDispatchRow::activity_id` persists this id to parquet
///   (`analytics/llm_dispatch_rows.rs`) precisely so a durable row can be
///   joined back to the live span. Across a restart that join silently
///   matches the wrong span rather than failing.
///
/// So the counter starts at a per-process random base instead of at `1`.
/// The id stays a plain `u64` in decimal form — that is the join contract
/// `current_activity_id` documents and the parquet column stores — and
/// stays strictly increasing within a run, which is what lets a child's id
/// always exceed its parent's.
///
/// The base is drawn from the low 63 bits so the counter has 2^63 of
/// headroom before it could wrap into another process's range; at one id
/// per nanosecond that is nearly 300 years. Two runs collide only if their
/// bases land within a few million of each other, which at 63 bits of
/// entropy is not a case worth engineering against.
static NEXT_ACTIVITY_ID: LazyLock<AtomicU64> = LazyLock::new(|| {
    use rand::Rng;
    AtomicU64::new(rand::thread_rng().gen::<u64>() >> 1)
});

fn next_activity_id() -> u64 {
    NEXT_ACTIVITY_ID.fetch_add(1, Ordering::Relaxed)
}

/// Identity for one emitted activity event, from the same counter that
/// mints span ids.
///
/// Sharing the counter is deliberate: both need to be unique across
/// process restarts for the same reason (the view backfills a 24h window
/// that spans earlier runs), and drawing from one source means a `seq` can
/// never collide with an `activity_id` either.
fn next_record_seq() -> u64 {
    NEXT_ACTIVITY_ID.fetch_add(1, Ordering::Relaxed)
}

/// A unit this layer is tracking: the span itself when it is tracked,
/// otherwise the closest enclosing span that is.
struct TrackedUnit {
    activity_id: u64,
    root_activity_id: u64,
    principal: Option<String>,
    workspace: Option<String>,
    workload_class: Option<&'static str>,
    agent_id: Option<Arc<str>>,
    thread_id: Option<Arc<str>>,
    task_id: Option<Arc<str>>,
    model: Option<Arc<str>>,
}

/// Whether resolving a unit should also record that it failed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkError {
    No,
    Yes,
}

/// Read one span's tracked state, marking it failed when asked.
///
/// The extensions read guard lives and dies inside this function, so a
/// caller walking a chain never holds one span's guard while taking the
/// next span's.
fn read_tracked_unit<S>(span: &SpanRef<'_, S>, mark: MarkError) -> Option<TrackedUnit>
where
    S: for<'lookup> LookupSpan<'lookup>,
{
    let extensions = span.extensions();
    let state = extensions.get::<ActivityState>()?;
    if mark == MarkError::Yes {
        // Only *infer* failure — a declared outcome wins.
        let _ = state.outcome.compare_exchange(
            OUTCOME_UNSET,
            OUTCOME_ERROR,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
    Some(TrackedUnit {
        activity_id: state.activity_id,
        root_activity_id: state.root_activity_id,
        principal: state.principal.clone(),
        workspace: state.workspace.clone(),
        workload_class: state.workload_class,
        agent_id: state.agent_id.clone(),
        thread_id: state.thread_id.clone(),
        task_id: state.task_id.clone(),
        model: state.model.clone(),
    })
}

/// The unit a span or a log line belongs to — the span itself if this layer
/// tracks it, else the nearest ancestor it does.
///
/// Walks past untracked ancestors (a span whose target was skipped)
/// instead of orphaning the subtree beneath them, and carries that
/// ancestor's scope so a child that does not repeat `principal` /
/// `workspace` still routes to the workspace that saw its parent start.
/// Without that inheritance a scoped parent with unscoped children shows a
/// viewer half a tree. Inheritance is transitive by induction: an
/// ancestor's own state was already resolved this way when it opened.
///
/// Deliberately one function rather than "check self, then call a separate
/// one for the ancestors": that shape let attribution and error inference
/// land on *different* units. An ERROR logged inside a span whose target
/// was skipped hung its row off the nearest tracked ancestor while marking
/// nothing, so that ancestor closed `closed` while visibly containing a red
/// row. Resolving once means whichever `ActivityState` the row hangs off is
/// the one [`MarkError::Yes`] marks failed.
///
/// Safe to call from `on_new_span` before the new span's own state is
/// inserted: the registry clears a span's extensions when it recycles the
/// slot, so a reused span id never carries the previous span's state, and
/// the self check is simply a miss that falls through to the parent walk.
fn tracked_unit<S>(span: &SpanRef<'_, S>, mark: MarkError) -> Option<TrackedUnit>
where
    S: for<'lookup> LookupSpan<'lookup>,
{
    if let Some(unit) = read_tracked_unit(span, mark) {
        return Some(unit);
    }
    let mut cursor = span.parent();
    while let Some(ancestor) = cursor {
        if let Some(unit) = read_tracked_unit(&ancestor, mark) {
            return Some(unit);
        }
        cursor = ancestor.parent();
    }
    None
}

/// Declared wins, else the nearest tracked ancestor's value, else `None`.
///
/// The counterpart to [`resolve_scope`] for fields that have no default. Scope
/// falls back to `anonymous/default` because a span always belongs to someone;
/// an agent id or a workload class has no equivalent — absence is a fact about
/// the instrumentation, and the view is meant to show it rather than paper it
/// over. Kept separate from `resolve_scope` for exactly that reason: routing
/// these through it would force a defaulting rule onto fields that must not
/// have one.
///
/// Takes the ancestor accessor as a closure so the ancestor's `Arc` is cloned
/// only when a value is actually inherited, and takes the declared value
/// already as an `Arc<str>` so a declaring span allocates once — at the
/// visitor — rather than once for the `String` and again for the `Arc`.
fn inherit_identifier(
    declared: Option<Arc<str>>,
    ancestor: Option<&TrackedUnit>,
    field: impl Fn(&TrackedUnit) -> &Option<Arc<str>>,
) -> Option<Arc<str>> {
    declared.or_else(|| ancestor.and_then(|unit| field(unit).clone()))
}

/// The scope a span routes under.
///
/// Both fields declared wins outright. Neither declared inherits — a child
/// that does not repeat its parent's scope still routes to the workspace
/// that saw the parent start, which is what keeps a tree from half-building.
///
/// Half declared is the case worth spelling out. A lone `principal` or a
/// lone `workspace` cannot route on its own, and the obvious fallback —
/// take the ancestor's pair — is wrong when the declared half *disagrees*
/// with the ancestor: rows a span explicitly labelled as one principal's
/// work would be shown to a different principal, and transitively so for
/// every descendant, since each inherits the resolved pair. Half a scope is
/// reachable in practice because most emit sites feed `Option<&str>` off an
/// execution context whose `principal` and `workspace` are independently
/// optional, and `tracing` records nothing at all for a `None`.
///
/// So inheritance here requires agreement, and a conflict falls back to
/// the default scope rather than adopting a pair it contradicts.
///
/// # Where undeclared work lands
///
/// A span that declares nothing, and inherits nothing, routes to the
/// **default scope** (`anonymous`/`default`) — not to `system`/`system`.
///
/// `system`/`system` means something specific: cron tickers, capability
/// bootstrap, work that is genuinely runtime-wide. Memory consolidation,
/// taste distillation, LLM dispatch and capability invocation are none of
/// those. They are work done *for* a principal that simply failed to say
/// which one. Filing them under `system` mislabels them, and — because a
/// viewer can only see the system bucket by subscribing to it — forced
/// every activity view to open a second subscription to a bucket shared
/// across principals just to see ordinary background work. The default
/// scope is where undeclared work already belongs everywhere else in the
/// runtime, so one subscription now covers it.
///
/// Genuinely cross-scope work says so **positively**, by declaring
/// `principal`/`workspace` as `system`/`system` on its span (see
/// `attention_rank_recompute_pass`, which leases across every scope). It is
/// not inferred from silence: inferring "runtime-wide" from "said nothing"
/// is exactly what conflated the two.
///
/// The fallback is a signal, not a home. Every span landing in the default
/// scope is one that forgot to declare who it was for. If that bucket ever
/// starts carrying real volume, the fix is to go instrument those spans —
/// never to widen the bucket.
/// Where a resolved scope came from.
///
/// The *decision*, not the values. Comparing the resolved scope back against
/// the ancestor's looked equivalent and was not: when the ancestor itself sits
/// in `anonymous/default`, the default-scope fallback produces exactly the
/// ancestor's pair, so a span whose half-declaration CONTRADICTED that ancestor
/// compared equal to it and inherited its identity anyway — from the very
/// ancestor it had just contradicted. Not a cross-principal leak, since both
/// sides are the default bucket, but a false statement about whose work a row
/// is, and it would become a leak the moment the fallback changed. Reporting
/// the decision removes the coincidence entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeSource {
    /// The span declared both components itself.
    Declared,
    /// Taken from the nearest tracked ancestor, which the span either agreed
    /// with or said nothing about.
    Ancestor,
    /// Neither: the default bucket, because there was no ancestor or because
    /// the span's half-declaration contradicted the one there was.
    Default,
}

fn resolve_scope(
    declared_principal: Option<String>,
    declared_workspace: Option<String>,
    ancestor: Option<&TrackedUnit>,
) -> (Option<String>, Option<String>, ScopeSource) {
    let default_scope = || {
        (
            Some(DEFAULT_SCOPE_PRINCIPAL.to_string()),
            Some(DEFAULT_SCOPE_WORKSPACE.to_string()),
            ScopeSource::Default,
        )
    };
    let inherited = |ancestor: Option<&TrackedUnit>| match ancestor {
        Some(ancestor) => (
            ancestor.principal.clone(),
            ancestor.workspace.clone(),
            ScopeSource::Ancestor,
        ),
        None => default_scope(),
    };

    match (declared_principal, declared_workspace) {
        (Some(principal), Some(workspace)) => {
            (Some(principal), Some(workspace), ScopeSource::Declared)
        },
        (None, None) => inherited(ancestor),
        (declared_principal, declared_workspace) => {
            let Some(unit) = ancestor else {
                return default_scope();
            };
            let agrees = |declared: Option<&str>, inherited: Option<&str>| {
                declared.is_none_or(|declared| inherited == Some(declared))
            };
            if agrees(declared_principal.as_deref(), unit.principal.as_deref())
                && agrees(declared_workspace.as_deref(), unit.workspace.as_deref())
            {
                inherited(ancestor)
            } else {
                default_scope()
            }
        },
    }
}

/// The activity id of the unit the caller is running inside, if any.
///
/// This is the only reader of an [`ActivityState`] from outside the layer,
/// and it exists so there is never a second one. An activity id is a
/// process-monotonic counter this module hands out in [`next_activity_id`]
/// and parks in the span's registry extensions; it is **not** derivable from
/// `tracing::span::Id`, which the registry recycles after close. A caller
/// that computed its own id would get a different number for the same span,
/// and a join across the two would return no rows at all rather than fail —
/// silently empty is worse than absent. Read the id here; never re-derive it.
///
/// Resolution is the same walk [`RuntimeActivityLayer::on_event`] does for a
/// progress row: the current span's own state when this layer is tracking it,
/// otherwise the nearest ancestor it is tracking. A caller sitting inside a
/// span whose target was skipped therefore still attributes to the enclosing
/// unit instead of to nothing.
///
/// `None` is a legitimate answer rather than a failure — the caller is below
/// the span floor, or the process installed a subscriber not backed by a
/// [`Registry`]. Callers must record the absence; a placeholder or empty
/// string would join rows that never belonged together.
///
/// The value is the same `u64` the wire carries, which
/// [`ActivityRecord::into_transport_event`] stringifies with `to_string`. A
/// durable column that wants to join against the live view stores that
/// decimal form.
pub fn current_activity_id() -> Option<u64> {
    tracing::Span::current()
        .with_subscriber(|(span_id, dispatch)| {
            let registry = dispatch.downcast_ref::<Registry>()?;
            let span = registry.span(span_id)?;
            tracked_unit(&span, MarkError::No).map(|unit| unit.activity_id)
        })
        .flatten()
}

// ─────────────────────────────────────────────────────────────────────
// The layer
// ─────────────────────────────────────────────────────────────────────

/// Turns spans and INFO+ events into the `Activity` transport family.
pub struct RuntimeActivityLayer {
    channel: Arc<ActivityChannel>,
}

impl RuntimeActivityLayer {
    /// The layer the process registers, writing into
    /// [`global_activity_channel`].
    pub fn new() -> Self {
        Self {
            channel: global_activity_channel(),
        }
    }

    /// A layer bound to a caller-owned channel. For tests, which must not
    /// share the process-wide queue with each other.
    pub fn with_channel(channel: Arc<ActivityChannel>) -> Self {
        Self { channel }
    }

    pub fn channel(&self) -> &Arc<ActivityChannel> {
        &self.channel
    }
}

impl Default for RuntimeActivityLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for RuntimeActivityLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let metadata = attrs.metadata();
        let target = metadata.target();
        if should_skip_target(target) {
            return;
        }

        // No level filter on spans: the design admits all spans and only
        // filters events, and the layer's own pinned `info` filter means a
        // DEBUG/TRACE span never reaches this layer anyway.
        let Some(span) = ctx.span(id) else {
            return;
        };

        let mut visitor = SpanFieldVisitor::default();
        attrs.record(&mut visitor);

        // The new span's own state is not inserted yet, so this resolves to
        // the nearest tracked *ancestor* — see `tracked_unit`.
        let ancestor = tracked_unit(&span, MarkError::No);
        let parent_activity_id = ancestor.as_ref().map(|a| a.activity_id);

        let (principal, workspace, scope_source) =
            resolve_scope(visitor.principal, visitor.workspace, ancestor.as_ref());

        // IDENTITY TRAVELS WITH SCOPE, AND ONLY WITH IT.
        //
        // `resolve_scope` above refuses to inherit a scope it does not trust: a
        // span whose half-declared scope CONTRADICTS its ancestor is filed
        // under the default scope rather than the ancestor's. Reading the same
        // ancestor for identifiers regardless would undo that decision through
        // a side door — the row would land in `anonymous/default` still wearing
        // another principal's `agent_id` and `thread_id`, publishing one
        // operator's agent and conversation ids onto a bucket every viewer can
        // read.
        //
        // So the ancestor is trusted for identity only when the resolved scope
        // actually came FROM it. That is read off `ScopeSource` — the decision
        // — rather than by comparing the resolved pair back against the
        // ancestor's. The comparison had a coincidence hole: an ancestor
        // already sitting in `anonymous/default` compares equal to the
        // default-scope fallback, so a child that contradicted it inherited its
        // identity anyway, from the ancestor it had just contradicted.
        //
        // A full declaration is the one case that still needs a value check.
        // `Declared` means the span named a scope of its own; that is only the
        // ancestor's work if it named the ancestor's scope, and a child
        // restating its parent's scope verbatim is the ordinary way a subtree
        // stays legible.
        let identity_ancestor = match scope_source {
            ScopeSource::Ancestor => ancestor.as_ref(),
            ScopeSource::Declared => ancestor
                .as_ref()
                .filter(|unit| unit.principal == principal && unit.workspace == workspace),
            ScopeSource::Default => None,
        };

        // Declared wins, else inherit from the nearest tracked ancestor, else
        // stay `None`. Deliberately *not* routed through `resolve_scope`: that
        // function falls back to the default scope, because a span always
        // belongs to someone even when it forgets to say so. These do not.
        // There is no default agent and no default workload class, and
        // inventing one would fill the view with confident wrong answers
        // instead of showing the instrumentation gap.
        //
        // `workload_class` inherits on the same gate. It is not an identifier,
        // but it describes what the ANCESTOR's work is for, and attributing a
        // foreign scope's workload to this row is the same category of wrong.
        let workload_class = visitor
            .workload_class
            .or_else(|| identity_ancestor.and_then(|unit| unit.workload_class));
        let agent_id =
            inherit_identifier(visitor.agent_id, identity_ancestor, |unit| &unit.agent_id);
        let thread_id =
            inherit_identifier(visitor.thread_id, identity_ancestor, |unit| &unit.thread_id);
        let task_id = inherit_identifier(visitor.task_id, identity_ancestor, |unit| &unit.task_id);
        // Same gate. A model name is not private the way an agent id is, but
        // inheriting one across a scope this span was not filed under is still
        // a false statement about which model this work used.
        let model = inherit_identifier(visitor.model, identity_ancestor, |unit| &unit.model);

        let activity_id = next_activity_id();
        let kind = visitor
            .kind
            .unwrap_or_else(|| default_kind_for_target(target));

        // The subtree root, read from the STRUCTURAL ancestor rather than
        // `identity_ancestor`. `parent_activity_id` above does the same, and
        // for the same reason: the parent edge and the root describe the shape
        // of the span tree, not who the work belongs to. Gating them on scope
        // agreement would sever the tree at exactly the spans whose scope is
        // most confused, which is where a viewer most needs the nesting.
        //
        // A span with no tracked ancestor is its own root. That keeps subtree
        // rollups total: every row belongs to exactly one root, including the
        // orphan whose real root closed or was never tracked. NULL would make
        // `GROUP BY root_activity_id` quietly lose those rows.
        let root_activity_id = ancestor
            .as_ref()
            .map_or(activity_id, |unit| unit.root_activity_id);
        let started_at_ms = now_ms();

        span.extensions_mut().insert(ActivityState {
            activity_id,
            parent_activity_id,
            root_activity_id,
            name: metadata.name(),
            target,
            kind,
            started_at_ms,
            opened_at: Instant::now(),
            principal: principal.clone(),
            workspace: workspace.clone(),
            outcome: AtomicU8::new(visitor.outcome.unwrap_or(OUTCOME_UNSET)),
            workload_class,
            agent_id: agent_id.clone(),
            thread_id: thread_id.clone(),
            task_id: task_id.clone(),
            model: model.clone(),
        });

        self.channel.push(ActivityRecord::Started {
            activity_id,
            parent_activity_id,
            name: metadata.name(),
            target,
            kind,
            principal,
            workspace,
            workload_class,
            agent_id,
            thread_id,
            task_id,
            model,
            // Taken straight off the visitor with no inheritance gate, unlike
            // every dimension above it. A span that did not declare an
            // operation did not perform one, so there is nothing to inherit and
            // an absent value here is the truth rather than a gap.
            operation: visitor.operation,
            // The same instant the state recorded, not a second reading of the
            // clock. The live row and the durable row must agree on when this
            // span started or a join across them looks like two spans.
            timestamp: started_at_ms,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };

        // Before the visitor, not after. `on_record` fires for EVERY
        // `Span::record` anywhere in the process, and the untracked ones used
        // to build a visitor and run `values.record` — allocating a `String`
        // per interesting field, and a `format!` per `record_debug` — only for
        // the tracked-span check below to throw all of it away. The target
        // check is a string comparison against a fixed prefix list and rejects
        // the same spans `on_new_span` already refused to track.
        if should_skip_target(span.metadata().target()) {
            return;
        }

        let mut visitor = SpanFieldVisitor::default();
        values.record(&mut visitor);

        // Outcome first, under the SHARED guard: it is an atomic precisely so
        // it can be set through `&ActivityState`. Scoped so this read guard is
        // released before the write guard below is taken — holding both would
        // deadlock on the same extensions lock.
        {
            let extensions = span.extensions();
            let Some(state) = extensions.get::<ActivityState>() else {
                return;
            };
            if let Some(code) = visitor.outcome {
                // A declared outcome is authoritative — the code knows what
                // happened and this layer only infers.
                state.outcome.store(code, Ordering::Relaxed);
            }
        }

        // The dimensions are plain fields, not atomics, so a late declaration
        // needs the exclusive guard.
        //
        // WHAT THIS CAN AND CANNOT DO. `ActivityStarted` for this span has
        // already been emitted by the time any `Span::record` runs, so a value
        // arriving here never changes the row the span opened with. What it
        // does change is everything downstream: children opened afterwards
        // inherit it, and the value is available for the rest of the span's
        // life. That is the case this exists for — the LLM router selects a
        // model *after* opening its span, so `model` is only knowable late.
        //
        // A declared value overwrites, matching `outcome` above. Code that
        // records a dimension is making a statement, and a re-route to a
        // different model is a new true statement rather than a duplicate.
        if visitor.has_late_dimensions() {
            let mut extensions = span.extensions_mut();
            let Some(state) = extensions.get_mut::<ActivityState>() else {
                return;
            };
            if let Some(class) = visitor.workload_class {
                state.workload_class = Some(class);
            }
            if let Some(agent_id) = visitor.agent_id {
                state.agent_id = Some(agent_id);
            }
            if let Some(thread_id) = visitor.thread_id {
                state.thread_id = Some(thread_id);
            }
            if let Some(task_id) = visitor.task_id {
                state.task_id = Some(task_id);
            }
            if let Some(model) = visitor.model {
                state.model = Some(model);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let target = metadata.target();
        if should_skip_target(target) {
            return;
        }

        // Same filter as the analytics layer: INFO and above.
        let level = *metadata.level();
        if level > tracing::Level::INFO {
            return;
        }

        // An ERROR marks the unit the row is attributed to — which may be an
        // ancestor, when the span the line was logged in has a skipped
        // target. Marking and attribution resolve together in `tracked_unit`
        // so they cannot disagree; a unit must never show a red row and
        // still close `closed`. Only the unit the row lands on is marked,
        // never the whole chain above it, so the view points at what failed
        // instead of painting the tree red.
        let mark = if level == tracing::Level::ERROR {
            MarkError::Yes
        } else {
            MarkError::No
        };

        let (activity_id, principal, workspace) = match ctx
            .event_span(event)
            .and_then(|span| tracked_unit(&span, mark))
        {
            Some(unit) => (Some(unit.activity_id), unit.principal, unit.workspace),
            // Below the span floor, or no tracked span anywhere above.
            // Expected, and rendered loose at the root rather than dropped.
            None => (None, None, None),
        };

        // Captures the `message` field and nothing else — see the privacy
        // note on `MessageVisitor`.
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        self.channel.push(ActivityRecord::Progress {
            activity_id,
            level: level_token(level),
            message: truncate_message(visitor.message),
            target,
            principal,
            workspace,
            timestamp: now_ms(),
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };

        // Absence of state means the open was skipped, so there is no
        // start for this finish to close.
        let record = {
            let extensions = span.extensions();
            let Some(state) = extensions.get::<ActivityState>() else {
                return;
            };
            // Everything here is a copy out of state the span already holds:
            // three fat pointers borrowed from `Metadata`, four `Arc` refcount
            // bumps, two `String` clones this record already made. No
            // allocation is added, no work is done that the durable sink could
            // have done later — because it could not: a `Finished` that had to
            // be paired against a possibly-evicted `Started` would lose rows.
            ActivityRecord::Finished {
                activity_id: state.activity_id,
                parent_activity_id: state.parent_activity_id,
                root_activity_id: state.root_activity_id,
                name: state.name,
                target: state.target,
                kind: state.kind,
                workload_class: state.workload_class,
                model: state.model.clone(),
                agent_id: state.agent_id.clone(),
                thread_id: state.thread_id.clone(),
                task_id: state.task_id.clone(),
                started_at_ms: state.started_at_ms,
                duration_ms: state.opened_at.elapsed().as_millis() as u64,
                outcome: outcome_token(state.outcome.load(Ordering::Relaxed)),
                principal: state.principal.clone(),
                workspace: state.workspace.clone(),
                timestamp: now_ms(),
            }
        };

        self.channel.push(record);
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn level_token(level: tracing::Level) -> &'static str {
    match level {
        tracing::Level::ERROR => "error",
        tracing::Level::WARN => "warn",
        tracing::Level::INFO => "info",
        tracing::Level::DEBUG => "debug",
        tracing::Level::TRACE => "trace",
    }
}

fn truncate_message(mut message: String) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_MESSAGE_BYTES;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str("…[truncated]");
    message
}

/// Visitor for the handful of span fields this layer understands.
///
/// Reads `principal`, `workspace`, `activity_kind`, `activity_outcome`,
/// `workload_class`, `agent_id`, `thread_id`, `task_id` and `model`, and
/// ignores everything else. Span fields are as browser-visible as log
/// messages are, so this list is a privacy boundary. Two rules keep it one:
///
/// * Fields with a closed set — `kind`, `outcome`, `workload_class` — are
///   normalised onto it on the way in, so an unrecognised token becomes
///   `None` rather than reaching the wire verbatim.
/// * The open-ended fields are **identifiers only** — an agent id, a thread
///   id, a task id, a model name. None of them is user content, and each is
///   length-bounded on the way in, because an unbounded string on a queue
///   that bounds only its record count is bounded in name alone.
///
/// Adding a field here widens what the browser sees. Anything carrying user
/// text belongs in the retrospective store, not on this event.
#[derive(Default)]
struct SpanFieldVisitor {
    principal: Option<String>,
    workspace: Option<String>,
    kind: Option<&'static str>,
    outcome: Option<u8>,
    workload_class: Option<&'static str>,
    /// `Arc<str>` from the moment they are read, not `String`: the value is
    /// handed straight to [`ActivityState`] and to the queued record, both of
    /// which hold `Arc<str>` so a subtree shares one allocation. Materialising
    /// a `String` here first would allocate, copy and free once per declared
    /// field per span for nothing.
    agent_id: Option<Arc<str>>,
    thread_id: Option<Arc<str>>,
    task_id: Option<Arc<str>>,
    model: Option<Arc<str>>,
    operation: Option<Arc<str>>,
}

impl SpanFieldVisitor {
    fn is_interesting(name: &str) -> bool {
        matches!(
            name,
            "principal"
                | "workspace"
                | "activity_kind"
                | "activity_outcome"
                | "workload_class"
                | "agent_id"
                | "thread_id"
                | "task_id"
                | "model"
                | "operation"
        )
    }

    fn assign(&mut self, name: &str, value: &str) {
        match name {
            "principal" => self.principal = Some(value.to_string()),
            "workspace" => self.workspace = Some(value.to_string()),
            "activity_kind" => self.kind = canonical_kind(value),
            "activity_outcome" => {
                // An unrecognised token leaves the outcome unset rather
                // than clobbering one this layer already inferred.
                let code = outcome_code(value);
                if code != OUTCOME_UNSET {
                    self.outcome = Some(code);
                }
            },
            // Narrowed onto the dispatch layer's own taxonomy. A span that
            // declares something outside it is treated as undeclared rather
            // than inventing a tenth class the analytical store cannot join
            // against.
            "workload_class" => self.workload_class = canonical_workload_class(value),
            "agent_id" => self.agent_id = bounded_identifier(value),
            "thread_id" => self.thread_id = bounded_identifier(value),
            "task_id" => self.task_id = bounded_identifier(value),
            "model" => self.model = bounded_identifier(value),
            // Read at open and never inherited, so it never reaches
            // `ActivityState` and a late `Span::record("operation", …)` has
            // nowhere to land — see `has_late_dimensions`. Every span that
            // declares one declares it in its `#[instrument]` fields.
            "operation" => self.operation = bounded_identifier(value),
            _ => {},
        }
    }
}

/// Identifier fields, trimmed and length-bounded.
///
/// Empty after trimming is `None`, not `Some("")` — an empty id would group
/// as its own bucket in the view and read as a real one.
///
/// The bound is deliberately far below `MAX_MESSAGE_BYTES`: these are ids and
/// model names, and a value approaching that length is a bug at the call site,
/// not a long identifier. Truncating rather than rejecting keeps a
/// mis-instrumented span visible instead of silently dimensionless.
fn bounded_identifier(declared: &str) -> Option<Arc<str>> {
    const MAX_IDENTIFIER_BYTES: usize = 128;

    let trimmed = declared.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= MAX_IDENTIFIER_BYTES {
        return Some(Arc::from(trimmed));
    }
    let mut end = MAX_IDENTIFIER_BYTES;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    Some(Arc::from(&trimmed[..end]))
}

impl SpanFieldVisitor {
    /// Whether this record carried any dimension worth taking the exclusive
    /// extensions guard for.
    ///
    /// `on_record` fires for every `Span::record` anywhere in the process, and
    /// the overwhelming majority carry only fields this layer ignores. Checking
    /// first keeps those off the write lock entirely.
    ///
    /// `operation` is deliberately absent from this list. It is the one
    /// dimension that does not inherit, so it is never stored on
    /// [`ActivityState`], so there is no state for a late record to update —
    /// it rides `Started` and only `Started`. Every span that declares one
    /// declares it in its `#[instrument]` fields, at open.
    fn has_late_dimensions(&self) -> bool {
        self.workload_class.is_some()
            || self.agent_id.is_some()
            || self.thread_id.is_some()
            || self.task_id.is_some()
            || self.model.is_some()
    }
}

impl tracing::field::Visit for SpanFieldVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if Self::is_interesting(field.name()) {
            self.assign(field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !Self::is_interesting(field.name()) {
            return;
        }
        // `%value` and `?value` both arrive here; strip the quotes `Debug`
        // adds to a string, matching `MessageVisitor`.
        let rendered = format!("{:?}", value);
        let unquoted = rendered
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(&rendered);
        self.assign(field.name(), unquoted);
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::realtime_events::event_visible_to_scope;
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;

    /// Tests emit under an explicit target so the layer's own module path
    /// (which `should_skip_target` guards) does not suppress them.
    ///
    /// Rooted at `magician` deliberately: `should_skip_target` now admits
    /// only first-party target roots, so a bare `runtime_activity_test`
    /// would be filtered as third-party and every test here would silently
    /// observe zero records — passing assertions about an empty vector
    /// rather than testing the layer.
    const TEST_TARGET: &str = "magician::runtime_activity_test";

    /// Run `f` with a registry carrying only this layer, then return
    /// everything the layer queued. Each call gets its own channel so
    /// tests never observe each other's rows.
    fn record_activity(capacity: usize, f: impl FnOnce()) -> Vec<ActivityRecord> {
        let channel = Arc::new(ActivityChannel::new(capacity));
        let subscriber = tracing_subscriber::registry()
            .with(RuntimeActivityLayer::with_channel(Arc::clone(&channel)));
        tracing::subscriber::with_default(subscriber, f);
        drain(&channel)
    }

    fn drain(channel: &ActivityChannel) -> Vec<ActivityRecord> {
        let mut records = Vec::new();
        while let Some(record) = channel.try_recv() {
            records.push(record);
        }
        records
    }

    /// `name` / `kind` are `&'static str` on the record, so the tuple does
    /// not borrow the vector it came from.
    fn started(record: &ActivityRecord) -> Option<(u64, Option<u64>, &'static str, &'static str)> {
        match record {
            ActivityRecord::Started {
                activity_id,
                parent_activity_id,
                name,
                kind,
                ..
            } => Some((*activity_id, *parent_activity_id, name, kind)),
            _ => None,
        }
    }

    #[test]
    fn span_open_and_close_emit_started_then_finished_with_a_duration() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            std::thread::sleep(Duration::from_millis(12));
            drop(entered);
            drop(span);
        });

        assert_eq!(records.len(), 2, "expected one start and one finish");

        let (start_id, parent, name, _kind) =
            started(&records[0]).expect("first record should be Started");
        assert_eq!(name, "unit_of_work");
        assert_eq!(parent, None, "a root span has no parent activity");

        match &records[1] {
            ActivityRecord::Finished {
                activity_id,
                duration_ms,
                outcome,
                ..
            } => {
                assert_eq!(*activity_id, start_id, "finish must close its own start");
                assert!(
                    *duration_ms >= 1,
                    "a 12ms span should report a plausible duration, got {duration_ms}ms"
                );
                assert_eq!(*outcome, "closed", "an undeclared outcome is not success");
            },
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn a_child_span_carries_its_parents_activity_id() {
        let records = record_activity(64, || {
            let parent = tracing::info_span!(target: TEST_TARGET, "parent_unit");
            let parent_guard = parent.enter();
            let child = tracing::info_span!(target: TEST_TARGET, "child_unit");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(parent_guard);
            drop(parent);
        });

        let (parent_id, parent_parent, parent_name, _) =
            started(&records[0]).expect("parent Started");
        let (child_id, child_parent, child_name, _) = started(&records[1]).expect("child Started");

        assert_eq!(parent_name, "parent_unit");
        assert_eq!(child_name, "child_unit");
        assert_eq!(parent_parent, None);
        assert_eq!(
            child_parent,
            Some(parent_id),
            "the child must hang off the parent's activity id"
        );
        assert_ne!(child_id, parent_id);
    }

    #[test]
    fn a_progress_row_attaches_to_the_span_it_was_logged_in() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            tracing::info!(target: TEST_TARGET, "made progress");
            drop(entered);
            drop(span);
        });

        let (span_id, _, _, _) = started(&records[0]).expect("Started");
        match &records[1] {
            ActivityRecord::Progress {
                activity_id,
                level,
                message,
                target,
                ..
            } => {
                assert_eq!(*activity_id, Some(span_id));
                assert_eq!(*level, "info");
                assert_eq!(message, "made progress");
                assert_eq!(*target, TEST_TARGET);
            },
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn a_progress_row_below_the_span_floor_has_no_activity_id() {
        let records = record_activity(64, || {
            tracing::info!(target: TEST_TARGET, "loose line");
        });

        assert_eq!(records.len(), 1);
        match &records[0] {
            // Expected, not a bug: the view renders this loose at the root.
            ActivityRecord::Progress { activity_id, .. } => assert_eq!(*activity_id, None),
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[test]
    fn events_below_info_are_filtered_out() {
        let records = record_activity(64, || {
            tracing::debug!(target: TEST_TARGET, "debug line");
            tracing::trace!(target: TEST_TARGET, "trace line");
            tracing::info!(target: TEST_TARGET, "info line");
            tracing::warn!(target: TEST_TARGET, "warn line");
            tracing::error!(target: TEST_TARGET, "error line");
        });

        let levels: Vec<&str> = records
            .iter()
            .filter_map(|record| match record {
                ActivityRecord::Progress { level, .. } => Some(*level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec!["info", "warn", "error"]);
    }

    #[test]
    fn the_emit_path_targets_are_skipped() {
        // Each of these handles every event the layer emits, so a log line
        // from one is a per-event feedback loop.
        for target in [
            "analytics",
            "magician::magician_v2::realtime_events",
            "magician::magician_v2::transport_log",
            "magician::magician_v2::api::websocket_handler",
            SELF_TARGET,
        ] {
            assert!(should_skip_target(target), "{target} must be guarded");
        }
        // Prefix, not equality — submodules of the emit path count too.
        assert!(should_skip_target(
            "magician::magician_v2::api::websocket_handler::inner"
        ));
        assert!(!should_skip_target(TEST_TARGET));
    }

    /// Third-party INFO must never reach the activity wire.
    ///
    /// `message` is free-form and crosses to a browser unredacted, so the
    /// question "did this runtime emit it?" is a privacy boundary, not a
    /// noise-reduction preference. The old policy admitted every target and
    /// excluded known-leaky ones by name, which protects only against leaks
    /// already discovered.
    #[test]
    fn third_party_targets_never_produce_activity() {
        for target in [
            "reqwest",
            "reqwest::connect",
            "duckdb",
            "sqlx::query",
            "aws_sdk_s3::operation",
            "rmcp::transport::auth",
            "lance::dataset",
            "hyper::client::conn",
        ] {
            assert!(
                should_skip_target(target),
                "{target} is not first-party and must not reach the wire"
            );
        }

        // First-party emits still get through: both crate-rooted module
        // paths and the explicit `target:` words this runtime uses.
        for target in [
            "magician::magician_v2::execution",
            "magicllm::dispatch",
            "runtime_core::registry",
            "meet_bot",
            "screen_capture",
            "analytics::llm_dispatch_rows",
            "artifact_v2.progress",
            "report:in_app",
        ] {
            assert!(
                is_first_party_target(target),
                "{target} is emitted by this runtime and must stay visible"
            );
        }
    }

    /// A prefix must not vouch for an unrelated crate that merely starts
    /// with the same letters.
    #[test]
    fn first_party_matching_is_by_whole_segment() {
        assert!(is_first_party_target("magician"));
        assert!(is_first_party_target("magician::a::b"));
        assert!(
            !is_first_party_target("magicianfoo::sneaky"),
            "`magician` must not admit a third-party `magicianfoo`"
        );
        assert!(!is_first_party_target("taskwarrior"), "vs `task`");
        assert!(!is_first_party_target("username"), "vs `user`");
    }

    /// The allowlist must stay sorted — `is_first_party_target` binary
    /// searches it, and an out-of-order entry silently stops matching.
    #[test]
    fn the_first_party_allowlist_is_sorted_and_unique() {
        let mut sorted = FIRST_PARTY_TARGET_ROOTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.as_slice(),
            FIRST_PARTY_TARGET_ROOTS,
            "keep FIRST_PARTY_TARGET_ROOTS sorted and free of duplicates"
        );
    }

    #[test]
    fn the_emit_path_produces_no_records() {
        let records = record_activity(64, || {
            tracing::warn!(target: "magician::magician_v2::transport_log", "append failed");
            tracing::error!(target: "magician::magician_v2::realtime_events", "rejected");
            tracing::info!(target: "analytics", "sink note");
            let span = tracing::info_span!(target: "magician::magician_v2::transport_log", "write");
            let entered = span.enter();
            drop(entered);
            drop(span);
        });

        assert!(
            records.is_empty(),
            "the layer must not observe its own emit path"
        );
    }

    #[test]
    fn noisy_library_targets_are_skipped_and_the_list_is_shared() {
        for target in crate::magician_v2::analytics::tracing_layer::NOISY_TARGET_PREFIXES {
            assert!(
                should_skip_target(target),
                "{target} is on the shared noisy list and must be skipped here too"
            );
        }

        let records = record_activity(64, || {
            tracing::info!(target: "hyper::client::conn", "pooled connection");
            tracing::warn!(target: "rustls::session", "handshake retry");
        });
        assert!(records.is_empty());
    }

    #[test]
    fn span_scope_fields_populate_the_event_and_children_inherit_them() {
        let records = record_activity(64, || {
            let parent = tracing::info_span!(
                target: TEST_TARGET,
                "scoped_unit",
                principal = "alice",
                workspace = "personal"
            );
            let parent_guard = parent.enter();
            let child = tracing::info_span!(target: TEST_TARGET, "child_unit");
            let child_guard = child.enter();
            tracing::info!(target: TEST_TARGET, "inside");
            drop(child_guard);
            drop(child);
            drop(parent_guard);
            drop(parent);
        });

        for record in records {
            let (principal, workspace) = match &record {
                ActivityRecord::Started {
                    principal,
                    workspace,
                    ..
                }
                | ActivityRecord::Finished {
                    principal,
                    workspace,
                    ..
                }
                | ActivityRecord::Progress {
                    principal,
                    workspace,
                    ..
                } => (principal.clone(), workspace.clone()),
            };
            assert_eq!(
                (principal.as_deref(), workspace.as_deref()),
                (Some("alice"), Some("personal")),
                "every row in a scoped tree must route to that scope, or the tree half-builds"
            );
        }
    }

    #[test]
    fn an_unscoped_span_routes_to_the_default_scope() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "unscoped_unit");
            let entered = span.enter();
            drop(entered);
            drop(span);
        });

        for record in records {
            let event = record.into_transport_event(0);
            assert!(
                event_visible_to_scope(&event, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
                "undeclared work belongs to whoever forgot to declare it, so it \
                 lands in the default scope"
            );
            assert!(
                !event_visible_to_scope(&event, "system", "system"),
                "and must NOT land in system/system — that bucket means \
                 genuinely runtime-wide work (cron, capability bootstrap), and \
                 filing ordinary background work there forced every viewer to \
                 subscribe to a bucket shared across principals"
            );
            assert!(
                !event_visible_to_scope(&event, "alice", "personal"),
                "and must not leak into a named user's workspace"
            );
        }
    }

    /// A span that declares `system`/`system` still routes there.
    ///
    /// The counterpart to the test above: genuinely cross-scope work
    /// (`attention_rank_recompute_pass` leases across every scope) says so
    /// positively, and that declaration must survive. The distinction being
    /// pinned is *declared* runtime-wide versus *undeclared*, which the old
    /// fallback collapsed into one bucket.
    #[test]
    fn a_span_declaring_the_system_scope_still_routes_there() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(
                target: TEST_TARGET,
                "cross_scope_pass",
                principal = "system",
                workspace = "system"
            );
            let entered = span.enter();
            drop(entered);
            drop(span);
        });

        for record in records {
            let event = record.into_transport_event(0);
            assert!(
                event_visible_to_scope(&event, "system", "system"),
                "declared runtime-wide work keeps the system bucket"
            );
            assert!(
                !event_visible_to_scope(&event, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
                "and must not fall into the undeclared bucket"
            );
        }
    }

    #[test]
    fn an_error_inside_a_span_marks_its_outcome() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "failing_unit");
            let entered = span.enter();
            tracing::error!(target: TEST_TARGET, "it broke");
            drop(entered);
            drop(span);
        });

        let outcome = records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Finished { outcome, .. } => Some(*outcome),
                _ => None,
            })
            .expect("Finished");
        assert_eq!(outcome, "error");
    }

    /// An ERROR logged inside a span this layer does *not* track must mark
    /// the unit its row was attributed to.
    ///
    /// The row hangs off the nearest tracked ancestor — that is the whole
    /// point of walking past skipped spans. Marking used to happen only on
    /// the span the line was logged in, so when that span was untracked the
    /// ancestor collected the red row and still closed `closed`: a unit
    /// reported healthy while visibly containing an error.
    #[test]
    fn an_error_below_an_untracked_span_marks_the_unit_it_is_attributed_to() {
        let records = record_activity(64, || {
            let tracked = tracing::info_span!(target: TEST_TARGET, "outer_unit");
            let tracked_guard = tracked.enter();
            // A span on the emit path — skipped, so it gets no state.
            let skipped = tracing::info_span!(
                target: "magician::magician_v2::transport_log",
                "untracked_unit"
            );
            let skipped_guard = skipped.enter();
            // The *event's* target is not skipped, so the row is emitted and
            // attributed upward to `outer_unit`.
            tracing::error!(target: TEST_TARGET, "it broke down here");
            drop(skipped_guard);
            drop(skipped);
            drop(tracked_guard);
            drop(tracked);
        });

        let progress_parent = records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Progress { activity_id, .. } => Some(*activity_id),
                _ => None,
            })
            .expect("the error row is emitted");
        let (finished_id, outcome) = records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Finished {
                    activity_id,
                    outcome,
                    ..
                } => Some((*activity_id, *outcome)),
                _ => None,
            })
            .expect("Finished");

        assert_eq!(
            progress_parent,
            Some(finished_id),
            "the row must be attributed to the tracked ancestor"
        );
        assert_eq!(
            outcome, "error",
            "and that same ancestor must close as failed, not `closed`"
        );
    }

    /// Activity ids must not restart at a fixed, low value each boot.
    ///
    /// The view backfills 24h off the on-disk log, so one page holds spans
    /// from earlier runs; the parquet `activity_id` column joins across
    /// restarts too. A counter starting at `1` every boot made both of
    /// those silently match unrelated spans. This asserts the property that
    /// prevents it — the base is drawn per process, not fixed — while
    /// leaving the within-run guarantees (strictly increasing, parent
    /// before child) to the tests above.
    #[test]
    fn activity_ids_do_not_restart_at_a_fixed_base_each_boot() {
        let first = next_activity_id();
        let second = next_activity_id();

        // Strictly increasing, NOT `first + 1`. The counter is a process
        // global and the test harness runs tests in parallel threads, so
        // any concurrent span open or record drain legitimately draws
        // between these two calls. Asserting adjacency would make this
        // test flaky under exactly the concurrency the counter exists to
        // be safe under; increasing is the property callers actually rely
        // on (a child's id must exceed its parent's).
        assert!(
            second > first,
            "ids must stay strictly increasing within a run ({first} -> {second})"
        );
        assert!(
            first > u32::MAX as u64,
            "a fresh process must not mint ids from a low fixed base ({first}); \
             colliding with a previous run's ids merges unrelated spans in the \
             view and mis-joins the parquet activity_id column"
        );
        assert!(
            first < u64::MAX / 2,
            "the base must leave counter headroom below the wrap point"
        );
    }

    /// Every emitted event carries a distinct `seq`.
    ///
    /// This is the consumer's deduplication key across the backfill/live
    /// overlap. A progress row has no other identity — two identical log
    /// lines in the same millisecond inside the same span serialize
    /// byte-for-byte alike — so if `seq` ever repeated for distinct
    /// events, one of them would be silently dropped from the view.
    #[test]
    fn every_emitted_event_carries_a_distinct_seq() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "seq_unit");
            let entered = span.enter();
            // Two identical lines, indistinguishable by content.
            tracing::info!(target: TEST_TARGET, "same message");
            tracing::info!(target: TEST_TARGET, "same message");
            drop(entered);
            drop(span);
        });

        let seqs: Vec<String> = records
            .into_iter()
            .map(|record| match record.into_transport_event(0) {
                RuntimeTransportEvent::ActivityStarted { seq, .. }
                | RuntimeTransportEvent::ActivityFinished { seq, .. }
                | RuntimeTransportEvent::ActivityProgress { seq, .. } => seq,
                other => panic!("expected an activity event, got {other:?}"),
            })
            .collect();

        assert!(seqs.len() >= 4, "start + two progress rows + finish");
        assert!(
            seqs.iter().all(|seq| !seq.is_empty()),
            "every activity event must carry a seq: {seqs:?}"
        );
        let unique: std::collections::HashSet<&String> = seqs.iter().collect();
        assert_eq!(
            unique.len(),
            seqs.len(),
            "seq must be unique per emission, including for byte-identical \
             progress rows: {seqs:?}"
        );
    }

    /// A span declaring half a scope must never adopt a *conflicting*
    /// ancestor scope.
    ///
    /// Half a scope cannot route on its own, and inheriting the ancestor's
    /// pair is right when they agree. When they disagree, inheriting shows
    /// one principal's work to another — so this fails closed to the system
    /// bucket instead.
    #[test]
    fn a_half_scope_that_contradicts_its_ancestor_fails_closed() {
        let records = record_activity(64, || {
            let parent = tracing::info_span!(
                target: TEST_TARGET,
                "alice_unit",
                principal = "alice",
                workspace = "personal"
            );
            let parent_guard = parent.enter();
            // Declares a *different* principal and no workspace at all.
            let child = tracing::info_span!(target: TEST_TARGET, "bob_unit", principal = "bob");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(parent_guard);
            drop(parent);
        });

        let child = records
            .iter()
            .find(|record| {
                matches!(record, ActivityRecord::Started { name, .. } if *name == "bob_unit")
            })
            .expect("the child Started row");
        let child_scope = match child {
            ActivityRecord::Started {
                principal,
                workspace,
                ..
            } => (principal.clone(), workspace.clone()),
            _ => unreachable!("filtered to Started above"),
        };

        assert_eq!(
            child_scope,
            (
                Some(DEFAULT_SCOPE_PRINCIPAL.to_string()),
                Some(DEFAULT_SCOPE_WORKSPACE.to_string())
            ),
            "a contradicted half-scope must not inherit alice's pair — it \
             falls back to the default scope, the same place any other \
             undeclared work goes"
        );

        let child_event = child.clone().into_transport_event(0);
        assert!(
            !event_visible_to_scope(&child_event, "alice", "personal"),
            "and must not be visible in the workspace it contradicted"
        );
        assert!(event_visible_to_scope(
            &child_event,
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE
        ));
    }

    /// The agreeing case still inherits — the fix must not cost the
    /// half-built-tree protection that inheritance exists for.
    #[test]
    fn a_half_scope_that_agrees_with_its_ancestor_still_inherits() {
        let records = record_activity(64, || {
            let parent = tracing::info_span!(
                target: TEST_TARGET,
                "alice_unit",
                principal = "alice",
                workspace = "personal"
            );
            let parent_guard = parent.enter();
            // Repeats the principal, omits the workspace.
            let child = tracing::info_span!(target: TEST_TARGET, "child_unit", principal = "alice");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(parent_guard);
            drop(parent);
        });

        let child_scope = records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Started {
                    name,
                    principal,
                    workspace,
                    ..
                } if *name == "child_unit" => Some((principal.clone(), workspace.clone())),
                _ => None,
            })
            .expect("the child Started row");

        assert_eq!(
            child_scope,
            (Some("alice".to_string()), Some("personal".to_string())),
            "an agreeing half-scope must still complete from its ancestor"
        );
    }

    #[test]
    fn a_declared_kind_wins_over_the_target_heuristic() {
        let records = record_activity(64, || {
            let declared = tracing::info_span!(
                target: TEST_TARGET,
                "declared",
                activity_kind = KIND_BACKGROUND
            );
            let guard = declared.enter();
            drop(guard);
            drop(declared);

            let bogus = tracing::info_span!(
                target: TEST_TARGET,
                "bogus",
                activity_kind = "not-a-real-kind"
            );
            let guard = bogus.enter();
            drop(guard);
            drop(bogus);
        });

        let kinds: Vec<&str> = records
            .iter()
            .filter_map(|record| started(record).map(|(_, _, _, kind)| kind))
            .collect();
        assert_eq!(kinds[0], KIND_BACKGROUND);
        // An unrecognised declaration never reaches the wire; it falls back
        // to the heuristic default for its target.
        assert_eq!(kinds[1], default_kind_for_target(TEST_TARGET));
    }

    #[test]
    fn the_kind_heuristic_separates_the_families() {
        assert_eq!(default_kind_for_target("magicllm::dispatch"), KIND_LLM);
        assert_eq!(
            default_kind_for_target("magician::magician_v2::execution::primitive_dispatch::x"),
            KIND_PROCESS
        );
        assert_eq!(
            default_kind_for_target("magician::magician_v2::agents::memory_consolidator"),
            KIND_BACKGROUND,
            "a consolidator under agents:: is background work, not an agent step"
        );
        assert_eq!(default_kind_for_target("some::other::module"), KIND_RUNTIME);
        for kind in ACTIVITY_KINDS {
            assert_eq!(canonical_kind(kind), Some(*kind));
        }
        assert_eq!(canonical_kind("wat"), None);
    }

    // ── Task 6: the live ↔ retrospective join key ────────────────────

    #[test]
    fn the_reader_returns_the_same_id_the_started_row_carries() {
        // The whole correctness claim of `current_activity_id`. If this ever
        // drifts, every join between a live activity row and a dispatch row
        // returns zero rows without anything erroring.
        let mut observed = None;
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            observed = current_activity_id();
            drop(entered);
            drop(span);
        });

        let (started_id, _, _, _) = started(&records[0]).expect("Started");
        assert_eq!(
            observed,
            Some(started_id),
            "the id a caller reads must be the id the wire carries, or the join is empty"
        );
        // And the durable column stores the wire's string form, not the u64.
        match records[0].clone().into_transport_event(0) {
            RuntimeTransportEvent::ActivityStarted { activity_id, .. } => {
                assert_eq!(activity_id, started_id.to_string());
            },
            other => panic!("expected ActivityStarted, got {other:?}"),
        }
    }

    #[test]
    fn the_reader_resolves_to_the_innermost_span_then_walks_up() {
        let mut inner_observed = None;
        let mut skipped_observed = None;
        let records = record_activity(64, || {
            let parent = tracing::info_span!(target: TEST_TARGET, "parent_unit");
            let parent_guard = parent.enter();
            {
                let child = tracing::info_span!(target: TEST_TARGET, "child_unit");
                let child_guard = child.enter();
                inner_observed = current_activity_id();
                drop(child_guard);
            }
            {
                // A span this layer skips must not orphan the caller beneath
                // it; the id resolves to the nearest tracked ancestor.
                let skipped =
                    tracing::info_span!(target: "magician::magician_v2::transport_log", "write");
                let skipped_guard = skipped.enter();
                skipped_observed = current_activity_id();
                drop(skipped_guard);
            }
            drop(parent_guard);
            drop(parent);
        });

        let (parent_id, _, _, _) = started(&records[0]).expect("parent Started");
        let (child_id, _, _, _) = started(&records[1]).expect("child Started");
        assert_eq!(
            inner_observed,
            Some(child_id),
            "innermost tracked span wins"
        );
        assert_eq!(
            skipped_observed,
            Some(parent_id),
            "an untracked span attributes to the unit that encloses it"
        );
    }

    #[test]
    fn the_reader_is_none_below_the_span_floor() {
        // An LLM call made outside any instrumented span genuinely has no
        // activity. Recording `None` is the honest column value; a
        // fabricated or empty id would join unrelated rows together.
        let mut outside_any_span = None;
        let mut after_close = None;
        record_activity(64, || {
            outside_any_span = current_activity_id();
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            drop(entered);
            drop(span);
            after_close = current_activity_id();
        });

        assert_eq!(outside_any_span, None);
        assert_eq!(after_close, None, "a closed span is not still current");
    }

    #[test]
    fn the_reader_is_none_when_the_layer_is_not_installed() {
        // A registry that never had this layer added tracks the span but
        // holds no `ActivityState` for it. The reader must say `None` rather
        // than panic or invent one — this is every process and test binary
        // that runs the code without the activity view switched on.
        let observed = tracing::subscriber::with_default(tracing_subscriber::registry(), || {
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            let observed = current_activity_id();
            drop(entered);
            observed
        });
        assert_eq!(observed, None);
    }

    #[test]
    fn a_long_message_is_capped_before_it_reaches_the_wire() {
        let long = "x".repeat(MAX_MESSAGE_BYTES * 2);
        let capped = truncate_message(long);
        assert!(capped.len() <= MAX_MESSAGE_BYTES + "…[truncated]".len());
        assert!(capped.ends_with("…[truncated]"));

        let short = "fine".to_string();
        assert_eq!(truncate_message(short), "fine");
    }

    // ── Task 3: backpressure ─────────────────────────────────────────

    fn sample_record(n: u64) -> ActivityRecord {
        ActivityRecord::Progress {
            activity_id: Some(n),
            level: "info",
            message: format!("row {n}"),
            target: TEST_TARGET,
            principal: None,
            workspace: None,
            timestamp: 0,
        }
    }

    /// Shutdown does not take the tail off the stream.
    ///
    /// The forwarder used to `break` out of its loop on cancellation and leave
    /// whatever was still queued — up to the channel's whole 4096-slot
    /// capacity of already-closed spans. Worse than the loss was its
    /// invisibility: `dropped()` counts capacity evictions in `push`, and a
    /// record abandoned at shutdown was never evicted, so the counter a viewer
    /// reads went on saying the stream was complete. The durable spine writes
    /// its only rows from `Finished`, so a shutdown could silently cost the
    /// last minute of every span that closed in it.
    ///
    /// Cancelled BEFORE the forwarder starts, so the loop takes its
    /// cancellation arm immediately and the drain is the only thing that can
    /// deliver these rows.
    #[tokio::test]
    async fn the_forwarder_drains_its_queue_before_it_stops() {
        let channel = Arc::new(ActivityChannel::new(64));
        let broadcaster = RuntimeTransportBroadcaster::new(64);
        let mut receiver = broadcaster.subscribe();
        let shutdown = CancellationToken::new();
        for n in 0..5 {
            channel.push(sample_record(n));
        }
        shutdown.cancel();

        spawn_activity_forwarder(Arc::clone(&channel), &broadcaster, None, shutdown)
            .await
            .expect("the forwarder task joins");

        let mut delivered = 0;
        while receiver.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(
            delivered, 5,
            "every queued record must reach the bus before the forwarder stops"
        );
        assert!(
            channel.is_empty(),
            "and nothing may be left behind uncounted"
        );
        assert_eq!(
            channel.dropped(),
            0,
            "these were not evictions — the drop counter must not learn about them"
        );
    }

    #[test]
    fn a_saturated_channel_never_blocks_the_producer() {
        let channel = ActivityChannel::new(ACTIVITY_CHANNEL_CAPACITY);
        let overflow = 1_000u64;
        let total = ACTIVITY_CHANNEL_CAPACITY as u64 + overflow;

        // No consumer exists at all. If `push` waited on capacity this
        // loop would never return and the test would hang — terminating is
        // the proof.
        let began = Instant::now();
        for n in 0..total {
            channel.push(sample_record(n));
        }
        let elapsed = began.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "pushing {total} records into a queue with no consumer took {elapsed:?}"
        );
        assert_eq!(
            channel.len(),
            ACTIVITY_CHANNEL_CAPACITY,
            "queue stays bounded"
        );
        assert_eq!(channel.dropped(), overflow, "every eviction is counted");
    }

    #[test]
    fn drop_oldest_keeps_the_newest_rows() {
        let channel = ActivityChannel::new(4);
        for n in 0..6 {
            channel.push(sample_record(n));
        }

        let ids: Vec<Option<u64>> = drain(&channel)
            .iter()
            .map(|record| match record {
                ActivityRecord::Progress { activity_id, .. } => *activity_id,
                _ => None,
            })
            .collect();
        // A live view wants what is happening now, so the oldest two go.
        assert_eq!(ids, vec![Some(2), Some(3), Some(4), Some(5)]);
        assert_eq!(channel.dropped(), 2);
    }

    #[test]
    fn the_dropped_count_is_non_zero_and_reaches_a_consumer() {
        let channel = ActivityChannel::new(8);
        for n in 0..20 {
            channel.push(sample_record(n));
        }
        assert!(channel.dropped() > 0, "the channel must admit it dropped");

        let event = channel
            .try_recv_event()
            .expect("a consumer should get an event");
        let dropped = match &event {
            RuntimeTransportEvent::ActivityProgress { dropped, .. }
            | RuntimeTransportEvent::ActivityStarted { dropped, .. }
            | RuntimeTransportEvent::ActivityFinished { dropped, .. } => *dropped,
            other => panic!("expected an activity event, got {other:?}"),
        };
        assert_eq!(
            dropped,
            channel.dropped(),
            "the count on the wire is the count the layer holds"
        );
        assert_eq!(dropped, 12);
    }

    #[test]
    fn the_dropped_count_is_stamped_at_drain_time() {
        let channel = ActivityChannel::new(2);
        channel.push(sample_record(0));
        channel.push(sample_record(1));
        // Enqueued before any drop happened…
        assert_eq!(channel.dropped(), 0);

        channel.push(sample_record(2));
        channel.push(sample_record(3));
        assert_eq!(channel.dropped(), 2);

        // …but drained after, so it still carries the current total. This
        // is why drop-oldest can never strand the count: a saturated queue
        // is a full queue, so there is always a next row to carry it.
        let event = channel.try_recv_event().expect("event");
        match event {
            RuntimeTransportEvent::ActivityProgress { dropped, .. } => assert_eq!(dropped, 2),
            other => panic!("expected ActivityProgress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_waiting_consumer_wakes_on_push() {
        let channel = Arc::new(ActivityChannel::new(16));
        let consumer = Arc::clone(&channel);
        let handle = tokio::spawn(async move { consumer.recv().await });

        tokio::task::yield_now().await;
        channel.push(sample_record(7));

        let record = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("consumer should wake")
            .expect("join");
        match record {
            ActivityRecord::Progress { activity_id, .. } => assert_eq!(activity_id, Some(7)),
            other => panic!("expected Progress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pushing_from_async_code_does_not_yield() {
        // `push` is a synchronous fn, so it cannot await — a saturating
        // producer inside a tokio task can stall neither its own runtime
        // nor the caller. Exercised here to keep that property honest if
        // the signature ever changes.
        let channel = ActivityChannel::new(4);
        for n in 0..64 {
            channel.push(sample_record(n));
        }
        assert_eq!(channel.len(), 4);
        assert_eq!(channel.dropped(), 60);
    }

    /// Every declared workload class survives the visitor, in canonical form.
    ///
    /// Asserted against `ACTIVITY_WORKLOAD_CLASSES` itself rather than a list
    /// retyped here: a copy would keep agreeing with itself after the real set
    /// changed, which is the failure this is meant to catch.
    #[test]
    fn every_declared_workload_class_is_accepted() {
        for declared in ACTIVITY_WORKLOAD_CLASSES {
            let mut visitor = SpanFieldVisitor::default();
            visitor.assign("workload_class", declared);
            assert_eq!(
                visitor.workload_class,
                Some(*declared),
                "`{declared}` is in the closed set and must survive the visitor"
            );
        }
    }

    /// Case folds on the way in, canonicalises on the way out.
    ///
    /// Without this, `Foreground_Chat` and `foreground_chat` would group as two
    /// separate lanes in the view and as two separate keys in any rollup.
    #[test]
    fn workload_class_case_folds_to_one_lane() {
        let mut visitor = SpanFieldVisitor::default();
        visitor.assign("workload_class", "  Foreground_Chat  ");
        assert_eq!(visitor.workload_class, Some(WORKLOAD_FOREGROUND_CHAT));
    }

    /// The layer's classes must be the exact strings the analytical store holds.
    ///
    /// This is the whole reason the taxonomy was reused rather than reinvented:
    /// `llm_dispatch_rows` writes `trace.workload_class.as_str()` into the
    /// `workload_class` parquet column, so a live activity row and a dispatch
    /// row are only joinable if both spell the class the same way. They did
    /// not — the constants were typed from the Rust variant names, giving
    /// `ForegroundChat` on the wire against a stored `foreground_chat`. Nothing
    /// erred; the lanes grouped correctly and every join returned zero rows,
    /// which is the failure mode that looks like an answer.
    ///
    /// The `match` is the part that makes this durable. It is exhaustive, so a
    /// tenth `LlmWorkloadClass` variant fails to *compile* here rather than
    /// quietly becoming a class the store can hold and the layer cannot name.
    #[test]
    fn every_workload_class_matches_the_dispatch_taxonomy() {
        use magicllm::LlmWorkloadClass;

        fn layer_constant(class: LlmWorkloadClass) -> &'static str {
            match class {
                LlmWorkloadClass::ForegroundChat => WORKLOAD_FOREGROUND_CHAT,
                LlmWorkloadClass::InteractiveTask => WORKLOAD_INTERACTIVE_TASK,
                LlmWorkloadClass::AutonomousTask => WORKLOAD_AUTONOMOUS_TASK,
                LlmWorkloadClass::Scheduled => WORKLOAD_SCHEDULED,
                LlmWorkloadClass::Ambient => WORKLOAD_AMBIENT,
                LlmWorkloadClass::CommsAssist => WORKLOAD_COMMS_ASSIST,
                LlmWorkloadClass::Memory => WORKLOAD_MEMORY,
                LlmWorkloadClass::Evaluation => WORKLOAD_EVALUATION,
                LlmWorkloadClass::System => WORKLOAD_SYSTEM,
            }
        }

        let taxonomy = [
            LlmWorkloadClass::ForegroundChat,
            LlmWorkloadClass::InteractiveTask,
            LlmWorkloadClass::AutonomousTask,
            LlmWorkloadClass::Scheduled,
            LlmWorkloadClass::Ambient,
            LlmWorkloadClass::CommsAssist,
            LlmWorkloadClass::Memory,
            LlmWorkloadClass::Evaluation,
            LlmWorkloadClass::System,
        ];

        for class in taxonomy {
            assert_eq!(
                layer_constant(class),
                class.as_str(),
                "the layer's constant for {class:?} must be the string \
                 `llm_dispatch_batch.workload_class` stores, or the live row \
                 and the analytical row never join"
            );
        }

        // And the narrowing list the visitor maps onto is exactly that set —
        // no extra class the store can never contain, none missing.
        let stored: std::collections::BTreeSet<&str> =
            taxonomy.iter().map(|class| class.as_str()).collect();
        let declared: std::collections::BTreeSet<&str> =
            ACTIVITY_WORKLOAD_CLASSES.iter().copied().collect();
        assert_eq!(declared, stored);
        assert_eq!(
            ACTIVITY_WORKLOAD_CLASSES.len(),
            taxonomy.len(),
            "and holds no duplicates"
        );

        // The visitor accepts every one of them, so a span declaring what the
        // store holds is never treated as undeclared.
        for class in taxonomy {
            let mut visitor = SpanFieldVisitor::default();
            visitor.assign("workload_class", class.as_str());
            assert_eq!(visitor.workload_class, Some(class.as_str()));
        }
    }

    /// An unrecognised class is undeclared, never a tenth value.
    ///
    /// This is the direction that fails silently if inverted. A class that
    /// reached the wire verbatim would render as a real lane, and would join
    /// against nothing in `llm_dispatch_batch` — a grouping that looks correct
    /// and is empty.
    #[test]
    fn an_unrecognised_workload_class_is_undeclared() {
        for declared in ["nonsense", "Background", "foreground", ""] {
            let mut visitor = SpanFieldVisitor::default();
            visitor.assign("workload_class", declared);
            assert_eq!(
                visitor.workload_class, None,
                "`{declared}` is outside the closed set and must not reach the wire"
            );
        }
    }

    /// Identifiers are trimmed, bounded, and empty-is-absent.
    ///
    /// `Some("")` would group as its own bucket and read as a real agent.
    #[test]
    fn identifier_fields_are_trimmed_bounded_and_never_empty() {
        let mut visitor = SpanFieldVisitor::default();
        visitor.assign("agent_id", "  presto  ");
        visitor.assign("thread_id", "   ");
        visitor.assign("model", &"m".repeat(4096));

        assert_eq!(visitor.agent_id.as_deref(), Some("presto"));
        assert_eq!(visitor.thread_id, None, "whitespace-only is absent");
        let model = visitor
            .model
            .expect("an over-long model is truncated, not dropped");
        assert!(
            model.len() <= 128,
            "identifier fields are bounded; got {} bytes",
            model.len()
        );
    }

    fn unit_with(agent: Option<&str>, class: Option<&'static str>) -> TrackedUnit {
        TrackedUnit {
            activity_id: 1,
            // A unit with no ancestor is its own root, which is what
            // `on_new_span` resolves for a real root span. Using a different
            // value here would make the helper describe a shape the layer
            // never produces.
            root_activity_id: 1,
            principal: None,
            workspace: None,
            workload_class: class,
            agent_id: agent.map(Arc::from),
            thread_id: None,
            task_id: None,
            model: None,
        }
    }

    /// A declared value wins over anything above it.
    #[test]
    fn a_declared_identifier_beats_the_ancestor() {
        let ancestor = unit_with(Some("root-agent"), None);
        let resolved = inherit_identifier(Some(Arc::from("own-agent")), Some(&ancestor), |unit| {
            &unit.agent_id
        });
        assert_eq!(resolved.as_deref(), Some("own-agent"));
    }

    /// An undeclared child inherits from the nearest tracked ancestor.
    ///
    /// This is the property the whole dimension model rests on: a background
    /// root declares once and every LLM call beneath it is background work
    /// because of where it sits, not because of what it is. Asserted here on
    /// the resolver alone; the span tree that feeds it is covered by
    /// `a_child_span_inherits_every_dimension_its_root_declared`.
    #[test]
    fn an_undeclared_child_inherits_from_its_ancestor() {
        let ancestor = unit_with(Some("root-agent"), Some("Ambient"));
        let agent = inherit_identifier(None, Some(&ancestor), |unit| &unit.agent_id);
        assert_eq!(agent.as_deref(), Some("root-agent"));
    }

    /// The dimensions a `Started` record carries, by span name.
    ///
    /// Returns owned values so the tuple does not borrow `records`, and
    /// panics rather than returning `None` on a miss: every caller here is
    /// asserting about a span it just opened, so an absent record is a bug in
    /// the test, not a case to fold into the assertion.
    fn started_dimensions(
        records: &[ActivityRecord],
        span_name: &str,
    ) -> (
        Option<&'static str>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Started {
                    name,
                    workload_class,
                    agent_id,
                    thread_id,
                    task_id,
                    ..
                } if *name == span_name => Some((
                    *workload_class,
                    agent_id.as_deref().map(str::to_string),
                    thread_id.as_deref().map(str::to_string),
                    task_id.as_deref().map(str::to_string),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Started record named `{span_name}`"))
    }

    /// Inheritance as the wire actually carries it, through the layer.
    ///
    /// The resolver tests above exercise `inherit_identifier` on a hand-built
    /// `TrackedUnit`. Only a real span tree proves the part that can actually
    /// break: `on_new_span` resolves against the nearest tracked *ancestor*
    /// before inserting the new span's own state, and every dimension — not
    /// just the one the resolver test happens to pass — rides that walk.
    /// Transitive by induction, so the grandchild is the assertion that the
    /// induction holds rather than only the base case.
    #[test]
    fn a_child_span_inherits_every_dimension_its_root_declared() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "declaring_root",
                workload_class = WORKLOAD_AMBIENT,
                agent_id = "presto",
                thread_id = "thread-7",
                task_id = "task-9"
            );
            let root_guard = root.enter();
            // Declares nothing at all.
            let child = tracing::info_span!(target: TEST_TARGET, "silent_child");
            let child_guard = child.enter();
            let grandchild = tracing::info_span!(target: TEST_TARGET, "silent_grandchild");
            let grandchild_guard = grandchild.enter();
            drop(grandchild_guard);
            drop(grandchild);
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        let declared = (
            Some(WORKLOAD_AMBIENT),
            Some("presto".to_string()),
            Some("thread-7".to_string()),
            Some("task-9".to_string()),
        );
        for name in ["declaring_root", "silent_child", "silent_grandchild"] {
            assert_eq!(
                started_dimensions(&records, name),
                declared,
                "`{name}` must carry the dimensions its root declared"
            );
        }
    }

    /// The same inheritance with the child opened after an await — the shape
    /// every background pass actually has.
    ///
    /// Mirrors `a_nested_worker_hangs_off_its_parent_across_an_await` in
    /// `tests/runtime_activity_visibility.rs`, which proves the parentage;
    /// this proves the dimensions ride it. A child created after a yield gets
    /// its parent from the instrumented future's span, not from a guard still
    /// on the stack, so it is the case where a naive "read the enclosing
    /// guard" implementation would quietly resolve to nothing.
    #[test]
    fn a_child_opened_after_an_await_still_inherits_its_roots_dimensions() {
        use tracing::Instrument;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime polls on the subscriber's thread");
        let records = record_activity(64, || {
            runtime.block_on(
                async {
                    tokio::task::yield_now().await;
                    let child = tracing::info_span!(target: TEST_TARGET, "post_await_child");
                    let guard = child.enter();
                    drop(guard);
                }
                .instrument(tracing::info_span!(
                    target: TEST_TARGET,
                    "awaiting_root",
                    workload_class = WORKLOAD_SCHEDULED,
                    agent_id = "presto"
                )),
            );
        });

        assert_eq!(
            started_dimensions(&records, "post_await_child"),
            (
                Some(WORKLOAD_SCHEDULED),
                Some("presto".to_string()),
                None,
                None
            ),
            "an await must not sever the dimensions from the root"
        );
    }

    /// A child that declares its own value keeps it, and still inherits the
    /// ones it stayed silent about.
    ///
    /// The two halves have to hold at once: a sub-agent dispatched inside an
    /// `Ambient` pass is a different agent doing the same class of work.
    #[test]
    fn a_declared_dimension_overrides_only_the_one_it_declares() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "declaring_root",
                workload_class = WORKLOAD_AMBIENT,
                agent_id = "root-agent"
            );
            let root_guard = root.enter();
            let child =
                tracing::info_span!(target: TEST_TARGET, "sub_agent", agent_id = "sub-agent");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        let (class, agent, _, _) = started_dimensions(&records, "sub_agent");
        assert_eq!(agent.as_deref(), Some("sub-agent"), "declared wins");
        assert_eq!(
            class,
            Some(WORKLOAD_AMBIENT),
            "and the dimension it stayed silent about still comes from the root"
        );
    }

    /// Open `pass` as a root, hang one silent `llm_dispatch` under it, and
    /// return the class that call reaches the wire with.
    ///
    /// The child is silent and named exactly as production names it, because
    /// `operation_llm_router`'s span deliberately declares no class of its own:
    /// it is a mechanism serving whoever called it, and declaring there would
    /// label every model call in the runtime identically. Whatever these
    /// assertions see, the root put there.
    fn lane_of_a_call_under(pass: impl FnOnce() -> tracing::Span) -> Option<&'static str> {
        let records = record_activity(64, || {
            let pass = pass();
            let pass_guard = pass.enter();
            let dispatch = tracing::info_span!(target: TEST_TARGET, "llm_dispatch");
            let dispatch_guard = dispatch.enter();
            drop(dispatch_guard);
            drop(dispatch);
            drop(pass_guard);
            drop(pass);
        });
        started_dimensions(&records, "llm_dispatch").0
    }

    /// A chat turn's model calls are foreground work, and carry the session's
    /// identity with them.
    ///
    /// Mirrors the declaration on `chat::service::process_chat_inline_turn`.
    /// Nothing under `chat/` was instrumented before it, so a chat-driven
    /// `llm_dispatch` opened as its own root and resolved to no class at all —
    /// the Undeclared lane's largest occupant, and the one piece of work a user
    /// is unambiguously blocked on.
    #[test]
    fn a_chat_turn_puts_its_model_calls_in_the_foreground_lane() {
        let records = record_activity(64, || {
            let turn = tracing::info_span!(
                target: TEST_TARGET,
                "chat_turn",
                activity_kind = KIND_AGENT,
                workload_class = WORKLOAD_FOREGROUND_CHAT,
                principal = "owner",
                workspace = "private",
                agent_id = "personal-assistant",
                thread_id = "session-1"
            );
            let turn_guard = turn.enter();
            let dispatch = tracing::info_span!(target: TEST_TARGET, "llm_dispatch");
            let dispatch_guard = dispatch.enter();
            drop(dispatch_guard);
            drop(dispatch);
            drop(turn_guard);
            drop(turn);
        });

        assert_eq!(
            started_dimensions(&records, "llm_dispatch"),
            (
                Some(WORKLOAD_FOREGROUND_CHAT),
                Some("personal-assistant".to_string()),
                Some("session-1".to_string()),
                None
            ),
            "a model call inside a chat turn is foreground chat work on that thread"
        );
        assert_eq!(
            started_scope(&records, "llm_dispatch"),
            (Some("owner".to_string()), Some("private".to_string())),
            "and it belongs to the session's scope, not the system bucket"
        );
    }

    /// A dispatched task a human asked for in a conversation.
    ///
    /// Mirrors `execution::agentic::executor::agentic_run_workload_class` when
    /// `chat_inline` is set or `chat_session_id` is present. The run is a ROOT
    /// here deliberately: a task execution runs on its own tokio task and does
    /// not sit under the chat turn that asked for it, so inheritance cannot
    /// reach it and the declaration has to be its own.
    #[test]
    fn an_agentic_run_with_chat_lineage_claims_the_interactive_lane() {
        assert_eq!(
            lane_of_a_call_under(|| tracing::info_span!(
                target: TEST_TARGET,
                "agentic_run",
                activity_kind = KIND_AGENT,
                workload_class = WORKLOAD_INTERACTIVE_TASK,
                principal = "owner",
                workspace = "private",
                agent_id = "presto",
                task_id = "task-9",
                thread_id = "session-1"
            )),
            Some(WORKLOAD_INTERACTIVE_TASK)
        );
    }

    /// A run with no trigger evidence leaves its calls in the Undeclared lane,
    /// on purpose.
    ///
    /// `AgenticContext` carries no trigger, source or origin for a run with no
    /// chat lineage, so `agentic_run_workload_class` returns `None` and the
    /// span records nothing — self-directed work and timer-driven work are
    /// genuinely indistinguishable from in there. This test exists to stop a
    /// later "helpful" default: a declared value beats an inherited one, so
    /// guessing `autonomous_task` here would permanently preempt the
    /// `scheduled` a monitor above could legitimately declare.
    #[test]
    fn an_agentic_run_with_no_trigger_evidence_stays_undeclared() {
        // Exactly what the production field expression evaluates to in this
        // case; `Option::None` records no value at all.
        let undeclared: Option<&'static str> = None;
        assert_eq!(
            lane_of_a_call_under(move || tracing::info_span!(
                target: TEST_TARGET,
                "agentic_run",
                activity_kind = KIND_AGENT,
                workload_class = undeclared,
                principal = "owner",
                workspace = "private",
                agent_id = "presto",
                task_id = "task-9"
            )),
            None,
            "an honest gap, not a confident wrong answer"
        );
    }

    /// A run delegated inside a chat turn takes its own lane without dragging
    /// the turn's own calls into it.
    ///
    /// Both halves have to hold at once. The turn is the user-blocking reply;
    /// the run underneath it is the task that reply dispatched, and the two are
    /// separately answerable questions ("what did chat cost" versus "what did
    /// its delegated work cost"). If the nested declaration leaked upward, or
    /// if it failed to override, one of those two numbers would be a lie.
    #[test]
    fn a_delegated_run_keeps_its_own_lane_inside_a_chat_turn() {
        let records = record_activity(64, || {
            let turn = tracing::info_span!(
                target: TEST_TARGET,
                "chat_turn",
                activity_kind = KIND_AGENT,
                workload_class = WORKLOAD_FOREGROUND_CHAT,
                principal = "owner",
                workspace = "private",
                agent_id = "personal-assistant",
                thread_id = "session-1"
            );
            let turn_guard = turn.enter();
            let dispatch = tracing::info_span!(target: TEST_TARGET, "llm_dispatch");
            let dispatch_guard = dispatch.enter();
            drop(dispatch_guard);
            drop(dispatch);
            let run = tracing::info_span!(
                target: TEST_TARGET,
                "agentic_run",
                activity_kind = KIND_AGENT,
                workload_class = WORKLOAD_INTERACTIVE_TASK
            );
            let run_guard = run.enter();
            drop(run_guard);
            drop(run);
            drop(turn_guard);
            drop(turn);
        });

        assert_eq!(
            started_dimensions(&records, "llm_dispatch").0,
            Some(WORKLOAD_FOREGROUND_CHAT),
            "the turn's own call stays foreground work"
        );
        let (class, agent, thread, _) = started_dimensions(&records, "agentic_run");
        assert_eq!(class, Some(WORKLOAD_INTERACTIVE_TASK), "the run declares");
        assert_eq!(
            (agent.as_deref(), thread.as_deref()),
            (Some("personal-assistant"), Some("session-1")),
            "and still inherits the identity it stayed silent about"
        );
    }

    /// The `(operation, model)` a `Started` row carries, by span name.
    ///
    /// The pair on purpose: they are the same shape of field — a bounded
    /// identifier read at open — and differ only in that one inherits and one
    /// does not, so the contrast is only visible when both are read.
    fn started_call_fields(
        records: &[ActivityRecord],
        span_name: &str,
    ) -> (Option<String>, Option<String>) {
        records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Started {
                    name,
                    operation,
                    model,
                    ..
                } if *name == span_name => Some((
                    operation.as_deref().map(str::to_string),
                    model.as_deref().map(str::to_string),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Started record named `{span_name}`"))
    }

    /// What makes a hundred `llm_dispatch` rows tell each other apart.
    ///
    /// The router has always named the operation on its span; the layer used
    /// to drop it on the floor, because `is_interesting` did not list it. Every
    /// dispatch therefore reached the browser as the same line: same span name
    /// (it is a boundary, so the name is identical by construction), same kind,
    /// same target.
    ///
    /// And it must NOT inherit, which is the half that could quietly go wrong.
    /// Every other dimension here describes the subtree, so a silent child
    /// takes its ancestor's; an operation describes one call, and handing it
    /// down would state that the child issued a request it never issued.
    #[test]
    fn an_operation_reaches_the_wire_and_stops_at_the_span_that_declared_it() {
        let records = record_activity(64, || {
            // Scope is declared because the real `llm_dispatch` declares it,
            // and because inheritance is gated on scope agreement: without it
            // the child would inherit nothing at all and the assertion below
            // would pass for the wrong reason.
            let dispatch = tracing::info_span!(
                target: TEST_TARGET,
                "llm_dispatch",
                activity_kind = KIND_LLM,
                principal = "owner",
                workspace = "private",
                operation = "agentic_decision",
                model = "some-model"
            );
            let dispatch_guard = dispatch.enter();
            let inner = tracing::info_span!(target: TEST_TARGET, "silent_child");
            let inner_guard = inner.enter();
            drop(inner_guard);
            drop(inner);
            drop(dispatch_guard);
            drop(dispatch);
        });

        assert_eq!(
            started_call_fields(&records, "llm_dispatch"),
            (
                Some("agentic_decision".to_string()),
                Some("some-model".to_string())
            ),
            "the operation the router names must survive the layer"
        );
        assert_eq!(
            started_call_fields(&records, "silent_child"),
            (None, Some("some-model".to_string())),
            "`model` describes the subtree and inherits; `operation` names one \
             call and must stop at the span that declared it"
        );
    }

    /// A root that makes no model call at all still has to declare.
    ///
    /// `observable_source_run` polls subscribed sources and files candidates;
    /// it dispatches no LLM operation, so nothing about the Undeclared
    /// *dispatch* lane forces its hand — but the span is its own row on the
    /// view, and an undeclared root renders as the instrumentation gap it is.
    /// Asserted on the root's own dimensions rather than on a child, because
    /// there is no child to assert on.
    #[test]
    fn a_root_with_no_model_calls_still_leaves_the_undeclared_lane() {
        let records = record_activity(64, || {
            let run = tracing::info_span!(
                target: TEST_TARGET,
                "observable_source_run",
                activity_kind = KIND_BACKGROUND,
                workload_class = WORKLOAD_AMBIENT,
                principal = "owner",
                workspace = "private"
            );
            let guard = run.enter();
            drop(guard);
            drop(run);
        });

        assert_eq!(
            started_dimensions(&records, "observable_source_run").0,
            Some(WORKLOAD_AMBIENT),
            "polling subscribed sources observes the outside world"
        );
    }

    /// The workers that reach the operation router without an agent run.
    ///
    /// `mail_assist` says so in as many words in its own module header — the
    /// distill and classify loops call the operation router directly because
    /// they are lightweight background loops, not agent runs — and the
    /// ambient distiller does the same from its 15-minute timer. That is
    /// precisely why their model calls opened as classless roots, and why the
    /// declaration has to sit on the pass rather than anywhere above it.
    ///
    /// Three separate span trees rather than one, because each child is named
    /// `llm_dispatch` exactly as production names it and `started_dimensions`
    /// resolves by name.
    #[test]
    fn a_direct_dispatch_worker_declares_its_own_lane() {
        assert_eq!(
            lane_of_a_call_under(|| tracing::info_span!(
                target: TEST_TARGET,
                "ambient_distill_pass",
                activity_kind = KIND_BACKGROUND,
                workload_class = WORKLOAD_AMBIENT,
                principal = "owner",
                workspace = "private"
            )),
            Some(WORKLOAD_AMBIENT),
            "distilling passively captured browsing is ambient work"
        );
        assert_eq!(
            lane_of_a_call_under(|| tracing::info_span!(
                target: TEST_TARGET,
                "mail_classify_pass",
                activity_kind = KIND_BACKGROUND,
                workload_class = WORKLOAD_COMMS_ASSIST,
                principal = "owner",
                workspace = "private"
            )),
            Some(WORKLOAD_COMMS_ASSIST)
        );
        assert_eq!(
            lane_of_a_call_under(|| tracing::info_span!(
                target: TEST_TARGET,
                "mail_distill_scope_tick",
                activity_kind = KIND_BACKGROUND,
                workload_class = WORKLOAD_COMMS_ASSIST,
                principal = "owner",
                workspace = "private"
            )),
            Some(WORKLOAD_COMMS_ASSIST)
        );
    }

    /// An undeclared root reaches the wire with no dimensions at all.
    ///
    /// The layer-level counterpart to `an_undeclared_root_stays_undeclared`:
    /// scope acquires a default because a span always belongs to someone,
    /// and these must not. A `Started` row carrying an invented class would
    /// render as a real lane in the view and join against nothing in
    /// `llm_dispatch_batch`.
    #[test]
    fn an_undeclared_span_reaches_the_wire_with_no_dimensions() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "undeclared_unit");
            let entered = span.enter();
            drop(entered);
            drop(span);
        });

        assert_eq!(
            started_dimensions(&records, "undeclared_unit"),
            (None, None, None, None)
        );
    }

    /// A recycled span slot must not hand its dimensions to the next span.
    ///
    /// The registry pools span slots and reuses one as soon as its span
    /// closes, so a freshly opened root routinely lands on the slot a
    /// just-closed span held. `on_new_span` reads `ActivityState` out of the
    /// registry's extensions *before* inserting its own — safe only because
    /// the registry clears a slot's extensions when it recycles it. If that
    /// ever stops holding, a new root silently adopts a dead span's parent
    /// id, agent and workload class, with nothing erroring; the monotonic
    /// `activity_id` counter would not save it, because the leak is in the
    /// state read, not in the id.
    #[test]
    fn a_recycled_span_slot_does_not_leak_the_previous_spans_dimensions() {
        let records = record_activity(256, || {
            // Enough closes to have returned slots to the pool, at the top
            // level so nothing below is a live descendant.
            for _ in 0..16 {
                let span = tracing::info_span!(
                    target: TEST_TARGET,
                    "declaring_root",
                    workload_class = WORKLOAD_AMBIENT,
                    agent_id = "presto",
                    task_id = "task-9"
                );
                let guard = span.enter();
                drop(guard);
                drop(span);
            }
            let fresh = tracing::info_span!(target: TEST_TARGET, "fresh_root");
            let guard = fresh.enter();
            drop(guard);
            drop(fresh);
        });

        let (_, parent, _, _) = records
            .iter()
            .filter_map(started)
            .find(|(_, _, name, _)| *name == "fresh_root")
            .expect("the fresh root's Started row");
        assert_eq!(parent, None, "a fresh root has no parent activity");
        assert_eq!(
            started_dimensions(&records, "fresh_root"),
            (None, None, None, None),
            "a recycled slot must not carry the previous occupant's dimensions"
        );
    }

    /// Undeclared with nothing above it stays undeclared.
    ///
    /// The assertion that stops a later "helpful" default. Scope falls back to
    /// `anonymous/default` because a span always belongs to someone; there is
    /// no default agent and no default workload class, and inventing one would
    /// fill the view with confident wrong answers instead of showing the gap.
    #[test]
    fn an_undeclared_root_stays_undeclared() {
        assert_eq!(inherit_identifier(None, None, |unit| &unit.agent_id), None);

        let orphan = unit_with(None, None);
        assert_eq!(
            inherit_identifier(None, Some(&orphan), |unit| &unit.agent_id),
            None,
            "an ancestor that never declared one cannot supply it either"
        );
    }

    /// The scope a span's `Started` row was filed under.
    ///
    /// Companion to `started_dimensions`, and panics on a miss for the same
    /// reason: every caller is asserting about a span it just opened.
    fn started_scope(
        records: &[ActivityRecord],
        span_name: &str,
    ) -> (Option<String>, Option<String>) {
        records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Started {
                    name,
                    principal,
                    workspace,
                    ..
                } if *name == span_name => Some((principal.clone(), workspace.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Started record named `{span_name}`"))
    }

    /// A contradicted scope takes the ancestor's identity with it.
    ///
    /// `resolve_scope` files a span whose half-declared scope disagrees with
    /// its ancestor under the DEFAULT scope rather than the ancestor's. If
    /// identifiers kept inheriting anyway, that row would land in
    /// `anonymous/default` still carrying another principal's `agent_id` and
    /// `thread_id` — publishing one operator's agent and conversation ids into
    /// a bucket every viewer can read.
    ///
    /// Driven through a real span tree, NOT by restating the gate expression.
    /// The previous version of this test reimplemented the gate inline and
    /// asserted on its own copy, which meant a regression in `on_new_span`
    /// could not fail it — the test would go on proving that the expression it
    /// had just written behaved the way it had just written it.
    #[test]
    fn a_contradicted_scope_does_not_inherit_the_ancestors_identity() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "owning_root",
                principal = "owner",
                workspace = "private",
                workload_class = WORKLOAD_AMBIENT,
                agent_id = "owner-agent",
                thread_id = "owner-thread"
            );
            let root_guard = root.enter();
            // Half-declares, and what it declares disagrees with the ancestor.
            let child = tracing::info_span!(
                target: TEST_TARGET,
                "contradicting_child",
                principal = "someone-else"
            );
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        assert_eq!(
            started_scope(&records, "contradicting_child"),
            (
                Some(DEFAULT_SCOPE_PRINCIPAL.to_string()),
                Some(DEFAULT_SCOPE_WORKSPACE.to_string())
            ),
            "a contradicted half-declaration is filed under the default scope"
        );
        assert_eq!(
            started_dimensions(&records, "contradicting_child"),
            (None, None, None, None),
            "and must arrive there empty — another principal's agent, thread \
             and workload must not ride a default-scope row"
        );
    }

    /// The same refusal when the ancestor is ITSELF in the default scope.
    ///
    /// This is the case the value comparison could not see. `resolve_scope`
    /// falls back to `anonymous/default`, which is exactly the pair this
    /// ancestor already holds, so "did the resolved scope equal the ancestor's"
    /// answered *yes* for a span that had just contradicted it — and identity
    /// was inherited from the ancestor it contradicted. Both sides being the
    /// default bucket means it was never a cross-principal leak, but it was a
    /// false statement about whose work the row is, and it would have become a
    /// leak the moment the fallback scope changed.
    #[test]
    fn a_contradiction_is_refused_even_when_the_ancestor_is_in_the_default_scope() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "default_bucket_root",
                principal = DEFAULT_SCOPE_PRINCIPAL,
                workspace = DEFAULT_SCOPE_WORKSPACE,
                workload_class = WORKLOAD_AMBIENT,
                agent_id = "default-bucket-agent"
            );
            let root_guard = root.enter();
            let child = tracing::info_span!(
                target: TEST_TARGET,
                "contradicting_child",
                principal = "someone-else"
            );
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        // The coincidence: the fallback lands on the ancestor's own scope.
        assert_eq!(
            started_scope(&records, "contradicting_child"),
            (
                Some(DEFAULT_SCOPE_PRINCIPAL.to_string()),
                Some(DEFAULT_SCOPE_WORKSPACE.to_string())
            )
        );
        assert_eq!(
            started_dimensions(&records, "contradicting_child"),
            (None, None, None, None),
            "a span that contradicted its ancestor must not inherit from it, \
             even when the fallback happens to land on the ancestor's scope"
        );
    }

    /// The ordinary paths still inherit. Without this, the gate could pass by
    /// never inheriting anything at all.
    ///
    /// Two shapes, because they take different arms of the decision: a child
    /// that declares nothing (scope resolved FROM the ancestor), and a child
    /// that restates its parent's scope in full (scope declared, and it names
    /// the ancestor's).
    #[test]
    fn an_agreeing_scope_still_inherits_identity() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "owning_root",
                principal = "owner",
                workspace = "private",
                workload_class = WORKLOAD_AMBIENT,
                agent_id = "owner-agent"
            );
            let root_guard = root.enter();
            let silent = tracing::info_span!(target: TEST_TARGET, "silent_child");
            let silent_guard = silent.enter();
            drop(silent_guard);
            drop(silent);
            let restating = tracing::info_span!(
                target: TEST_TARGET,
                "restating_child",
                principal = "owner",
                workspace = "private"
            );
            let restating_guard = restating.enter();
            drop(restating_guard);
            drop(restating);
            drop(root_guard);
            drop(root);
        });

        for name in ["silent_child", "restating_child"] {
            assert_eq!(
                started_scope(&records, name),
                (Some("owner".to_string()), Some("private".to_string())),
                "`{name}` belongs to the owner's scope"
            );
            assert_eq!(
                started_dimensions(&records, name),
                (
                    Some(WORKLOAD_AMBIENT),
                    Some("owner-agent".to_string()),
                    None,
                    None
                ),
                "`{name}` agrees with its ancestor and must keep its identity"
            );
        }
    }

    /// A record carrying no dimension never takes the write guard.
    ///
    /// `on_record` fires for every `Span::record` in the process and almost all
    /// of them carry fields this layer ignores. The predicate is what keeps
    /// those off the exclusive extensions lock, so it is asserted directly
    /// rather than inferred from behaviour.
    #[test]
    fn a_record_with_no_dimensions_skips_the_write_guard() {
        let mut visitor = SpanFieldVisitor::default();
        visitor.assign("activity_outcome", "success");
        assert!(
            !visitor.has_late_dimensions(),
            "an outcome-only record must not take the exclusive guard"
        );

        let mut visitor = SpanFieldVisitor::default();
        visitor.assign("model", "some-model");
        assert!(visitor.has_late_dimensions());
    }

    /// The model a `Started` row carried, or `None`.
    fn started_model(records: &[ActivityRecord], span_name: &str) -> Option<String> {
        records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Started { name, model, .. } if *name == span_name => {
                    Some(model.as_deref().map(str::to_string))
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Started record named `{span_name}`"))
    }

    /// A dimension recorded after the span opened still reaches children.
    ///
    /// This is the whole point of reading dimensions in `on_record`. The LLM
    /// router selects a model AFTER opening its span, so `model` is only
    /// knowable late. `ActivityStarted` has already sailed by then and cannot
    /// be amended — what a late value must still do is flow to everything
    /// opened underneath it.
    ///
    /// Driven through a real `Span::record` rather than a hand-built
    /// `TrackedUnit`, so `on_record` itself is what is under test. The second
    /// The declaring span's start row still cannot carry model, so this assertion
    /// confirms `ActivityFinished` still does.
    #[test]
    fn a_late_declaration_reaches_children_even_though_its_own_row_has_sailed() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "late_declaring_root",
                model = tracing::field::Empty
            );
            let root_guard = root.enter();
            // The router's shape: the span is already open and already
            // announced when the model becomes known.
            root.record("model", "chosen-late");
            let child = tracing::info_span!(target: TEST_TARGET, "child_after_the_choice");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        assert_eq!(
            started_model(&records, "child_after_the_choice").as_deref(),
            Some("chosen-late"),
            "a model chosen after the parent span opened must still tag its children"
        );
        assert_eq!(
            started_model(&records, "late_declaring_root"),
            None,
            "and the declaring span's `ActivityStarted` cannot carry it because it already emitted"
        );
        let root_finished_model = records
            .iter()
            .find_map(|record| match record {
                ActivityRecord::Finished { name, model, .. } if *name == "late_declaring_root" => {
                    model.as_deref().map(str::to_string)
                },
                _ => None,
            })
            .expect("late declaring span must finish with model");
        assert_eq!(
            root_finished_model.as_str(),
            "chosen-late",
            "the durable row carries model where the start row cannot"
        );
    }

    /// A field this layer does not understand is not captured at all.
    ///
    /// The visitor is a privacy boundary: span fields are as browser-visible
    /// as log messages. This asserts the boundary holds by construction rather
    /// than by everyone remembering it.
    #[test]
    fn an_unlisted_field_is_ignored() {
        assert!(!SpanFieldVisitor::is_interesting("prompt"));
        assert!(!SpanFieldVisitor::is_interesting("user_email"));
        assert!(SpanFieldVisitor::is_interesting("workload_class"));
        assert!(SpanFieldVisitor::is_interesting("agent_id"));
    }

    /// `(activity_id, root_activity_id)` for every finish, in emission order.
    fn finished_roots(records: &[ActivityRecord]) -> Vec<(u64, u64)> {
        records
            .iter()
            .filter_map(|record| match record {
                ActivityRecord::Finished {
                    activity_id,
                    root_activity_id,
                    ..
                } => Some((*activity_id, *root_activity_id)),
                _ => None,
            })
            .collect()
    }

    /// The denormalised root is what turns "everything this agent turn cost"
    /// from a recursive walk over the parent edge into one `GROUP BY`. It has
    /// to be the *top* of the tree, not the immediate parent.
    #[test]
    fn every_span_in_a_subtree_reports_the_same_root() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(target: TEST_TARGET, "agent_turn");
            let root_guard = root.enter();
            let child = tracing::info_span!(target: TEST_TARGET, "step");
            let child_guard = child.enter();
            let grandchild = tracing::info_span!(target: TEST_TARGET, "llm_call");
            let grandchild_guard = grandchild.enter();
            drop(grandchild_guard);
            drop(grandchild);
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        let (root_id, _, _, _) = started(&records[0]).expect("root Started");
        let finished = finished_roots(&records);
        assert_eq!(finished.len(), 3, "three spans opened, three closed");
        for (activity_id, reported_root) in &finished {
            assert_eq!(
                *reported_root, root_id,
                "span {activity_id} must report the top of its tree, not its parent"
            );
        }
        assert!(
            finished
                .iter()
                .any(|(activity_id, root)| activity_id == root),
            "the root must be its own root, so a subtree rollup includes it"
        );
    }

    /// A span with no tracked ancestor is its own root rather than NULL.
    /// Otherwise `GROUP BY root_activity_id` silently loses every orphan.
    #[test]
    fn an_orphan_span_is_its_own_root() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "lonely_unit");
            let entered = span.enter();
            drop(entered);
            drop(span);
        });

        let finished = finished_roots(&records);
        assert_eq!(finished.len(), 1);
        assert_eq!(
            finished[0].0, finished[0].1,
            "a root with no ancestor is its own root"
        );
    }

    /// The durable store reads its dimensions off the *close*, so a finish
    /// that did not carry them would produce rows that cannot be sliced —
    /// exactly the gap the spine exists to close. Inheritance must survive the
    /// trip to the close record, not just reach the start row.
    #[test]
    fn a_finish_carries_the_dimensions_its_start_resolved() {
        let records = record_activity(64, || {
            let root = tracing::info_span!(
                target: TEST_TARGET,
                "agent_turn",
                principal = "owner",
                workspace = "default",
                workload_class = "ambient",
                agent_id = "agent-7",
                thread_id = "thread-9"
            );
            let root_guard = root.enter();
            // Declares nothing of its own; everything below must be inherited.
            let child = tracing::info_span!(target: TEST_TARGET, "step");
            let child_guard = child.enter();
            drop(child_guard);
            drop(child);
            drop(root_guard);
            drop(root);
        });

        let child_finish = records
            .iter()
            .find(
                |record| matches!(record, ActivityRecord::Finished { name, .. } if *name == "step"),
            )
            .expect("the child's finish");

        match child_finish {
            ActivityRecord::Finished {
                workload_class,
                agent_id,
                thread_id,
                task_id,
                kind,
                target,
                started_at_ms,
                principal,
                ..
            } => {
                assert_eq!(*workload_class, Some(WORKLOAD_AMBIENT));
                assert_eq!(agent_id.as_deref(), Some("agent-7"));
                assert_eq!(thread_id.as_deref(), Some("thread-9"));
                assert_eq!(
                    *task_id, None,
                    "an undeclared, uninherited dimension stays absent on the close too"
                );
                assert_eq!(*kind, KIND_RUNTIME);
                assert_eq!(*target, TEST_TARGET);
                assert_eq!(principal.as_deref(), Some("owner"));
                assert!(
                    *started_at_ms > 0,
                    "the close must carry a real wall-clock start, not a zero placeholder"
                );
            },
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    /// The start row and the close row must agree on when the span began.
    /// Reading the clock twice would make a durable row and its live event
    /// look like two different spans to anything joining them.
    #[test]
    fn the_start_row_and_the_close_row_agree_on_the_start_time() {
        let records = record_activity(64, || {
            let span = tracing::info_span!(target: TEST_TARGET, "unit_of_work");
            let entered = span.enter();
            std::thread::sleep(Duration::from_millis(5));
            drop(entered);
            drop(span);
        });

        let started_at = match &records[0] {
            ActivityRecord::Started { timestamp, .. } => *timestamp,
            other => panic!("expected Started, got {other:?}"),
        };
        let finish_start = match &records[1] {
            ActivityRecord::Finished { started_at_ms, .. } => *started_at_ms,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_eq!(started_at, finish_start);
    }
}
