//! Minimal execution context for primitive dispatch.
//!
//! The primitive dispatchers don't need the full
//! [`crate::magician_v2::execution::agentic::AgenticContext`] — only a
//! handful of identity fields (storage path, principal/workspace/task/
//! execution scope) for trace placement and session identification. This
//! struct is the tight contract between the executor (which has access to a
//! populated `AgenticContext` *or* an
//! [`crate::magician_v2::execution::agentic::executor::ActionExecutors`])
//! and the primitive dispatch layer.
//!
//! Keeping a separate type here means the dispatch modules don't transitively
//! depend on `AgenticContext`'s ~100-field surface, and makes dispatch
//! testable with synthetic contexts.

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::browser::session::sanitize_session_id;
/// Cache-stable seed for the inner-loop run.
///
/// Dynamic per-turn status is intentionally excluded; later runtime-context
/// phases render that from the runtime ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedGoalBlock {
    pub goal: String,
    pub initial_micro_goal: Option<String>,
    pub success_criteria: Option<String>,
    pub durable_task_state_ref: Option<String>,
}

impl SeedGoalBlock {
    pub fn from_goal(goal: impl Into<String>, success_criteria: Option<String>) -> Self {
        let goal = goal.into().trim().to_string();
        Self {
            initial_micro_goal: (!goal.is_empty()).then_some(goal.clone()),
            goal,
            success_criteria: success_criteria.and_then(|value| {
                let trimmed = value.trim().to_string();
                (!trimmed.is_empty()).then_some(trimmed)
            }),
            durable_task_state_ref: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.goal.trim().is_empty()
            && self
                .initial_micro_goal
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
            && self
                .success_criteria
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
    }

    pub fn objective_text(&self) -> String {
        let mut lines = Vec::new();
        let goal = self.goal.trim();
        if !goal.is_empty() {
            lines.push(format!("goal: {goal}"));
        }
        if let Some(criteria) = self.success_criteria.as_deref().map(str::trim) {
            if !criteria.is_empty() {
                lines.push(format!("success_criteria: {criteria}"));
            }
        }
        if let Some(micro_goal) = self.initial_micro_goal.as_deref().map(str::trim) {
            if !micro_goal.is_empty() && micro_goal != goal {
                lines.push(format!("initial_micro_goal: {micro_goal}"));
            }
        }
        if lines.is_empty() {
            "goal: Use the current tool primitives to make progress.".to_string()
        } else {
            lines.join("\n")
        }
    }
}

pub fn primitive_objective_id(capability_name: &str, seed: &SeedGoalBlock) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"magician-inner-loop-objective-v1\0");
    hasher.update(capability_name.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(seed.objective_text().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn primitive_objective_text(seed: &SeedGoalBlock) -> String {
    seed.objective_text()
}

/// Levels of `PrimitiveExecCtx::browser_capture_withheld` (P4): what a
/// browser observation may return after a typed fill delivered a secret to
/// the page. Nothing is withheld.
pub const CAPTURE_WITHHELD_NONE: u8 = 0;
/// Image captures are withheld — pixels cannot be redacted — until a
/// navigation or a whole-page text observation without a delivered value;
/// text observations run and are scrubbed by the delivered value.
pub const CAPTURE_WITHHELD_PIXELS: u8 = 1;
/// Everything that returns page content is withheld: the value was split
/// across fields (one character each), so no text scrub can recognise it and
/// no observation can prove it gone — only a navigation command clears this.
pub const CAPTURE_WITHHELD_PAGE: u8 = 2;

/// Context bundle passed into primitive dispatch entry functions
/// ([`super::browser::execute_browser_action`],
/// [`super::capability_invoker::ScopedDeterministicCapabilityInvoker`]).
///
/// `Debug` is implemented manually so large side-channel handles are rendered
/// as booleans/counts instead of full structures.
#[derive(Clone)]
pub struct PrimitiveExecCtx {
    /// Magician storage root (`magician_data_v3` or override). Used to
    /// resolve the bundled `agent-browser` CLI path and the trace file
    /// location.
    pub storage_base_path: PathBuf,

    // --- Scope (used for trace path + future per-task config) ---
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub task_id: Option<String>,
    /// Originating chat session, when this task was launched from chat. This
    /// value is runtime provenance and must never come from model arguments.
    pub chat_session_id: Option<String>,

    // --- Execution identity (used for trace filename + session id) ---
    pub execution_id: Option<String>,
    pub legacy_execution_id: Option<String>,
    pub cycle_id: Option<String>,
    pub goal_id: Option<String>,
    pub agent_id: Option<String>,
    pub max_spawned_tasks: Option<u32>,

    /// Identity of THIS dispatch attempt, so an effect can be correlated with
    /// the decision that ordered it after control is lost.
    ///
    /// Carries the existing `LlmToolLineageIdentity::tool_execution_id`
    /// (`"{llm_call_id}:tool:{model_tool_call_id}"`) rather than a newly minted
    /// key. That shape matters: a deliberate retry re-decides and therefore
    /// gets a fresh `llm_call_id`, while a crash and re-run of the same
    /// decision reuses the key — so a genuine retry is never mistaken for a
    /// replay, and a replay is never mistaken for a retry.
    ///
    /// `None` where no model tool call sits behind the dispatch: the internal
    /// Yutori vision screenshot and viewport probe, the long-lived
    /// content-source context, and test fixtures. A `None` here means "not
    /// attributable", never "safe to repeat".
    ///
    /// RUNTIME-MINTED ONLY. Never populate this from model arguments — an
    /// LLM-authored key would let a model launder a repeat as a fresh attempt.
    pub effect_id: Option<String>,

    /// Exact runtime-resolved invocation provenance. Model arguments never
    /// populate this carrier; flat compiled dispatch uses it to stamp hidden
    /// owner/surface fields after stripping model-supplied `__*` keys.
    pub invocation_context: Option<crate::magician_v2::agents::AgentInvocationContext>,

    /// Work authority the owning execution runs under. Runtime
    /// provenance mirroring `chat_session_id`: stamped from
    /// `ActionExecutors::run_identity`, never from model
    /// arguments — the ctx is the only source, with no param fallback.
    ///
    /// Generic over every arm of
    /// [`crate::magician_v2::work_context::WorkContextKind`], because the
    /// execution context it is stamped from is. A `Program` carrier reaching
    /// here must not be flattened to `None`: absent means "this execution was
    /// never confined", and a program-scoped run was.
    pub work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,

    /// Operator-approved sandbox roots for THIS execution (the same
    /// `Arc<Mutex<HashSet<String>>>` held on
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors`]).
    /// When a file path outside the boot `file_sandbox` is approved via the
    /// sandbox-override HITL, the executor merges it in there; the PlainCompiled
    /// dispatch arm scopes it into the
    /// `crate::magician_v2::execution::compiled_dispatch::SESSION_FILE_SANDBOX_ROOTS`
    /// task-local so file-touching compiled handlers (`read_file`) widen their
    /// effective `FileSandboxConfig` on the retry — matching what the native
    /// `files` pack path already does at the `ExecutableAction::File` gate.
    /// `None` for boots/tests without a live executor.
    pub session_file_sandbox_roots:
        Option<Arc<std::sync::Mutex<std::collections::HashSet<String>>>>,

    /// True when the agentic decision that produced this action came from the
    /// Yutori provider. Gates the Yutori N1.5 action-translation shim in the
    /// browser dispatcher — only Yutori emits the coordinate-based
    /// `left_click`/`drag`/`scroll` vocabulary; other providers use native
    /// agent-browser commands directly. Threaded from
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors`]'s
    /// `decision_provider_is_yutori` flag.
    pub yutori_browser_actions: bool,

    /// Side-channel flag set by [`super::dispatch::dispatch_primitive`]
    /// the first time this execution opens a browser session. Read at
    /// outer-loop terminal time to decide whether `agent-browser
    /// --session ... close` should fire. Threaded from
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors::browser_session_used`].
    pub browser_session_used: Option<Arc<std::sync::atomic::AtomicBool>>,

    /// Unique retrieval-handoff browser sessions opened during this execution.
    /// Terminal cleanup drains this set so transferred headless/headed/CDP
    /// sessions cannot outlive their owner accidentally.
    pub browser_additional_session_ids:
        Option<Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>>,

    /// Configured engine for ordinary headed/headless sessions. An explicit
    /// tool-call `engine` argument takes precedence.
    pub browser_engine: Option<String>,

    /// Per-command output bound for typed retrieval handoff sessions.
    pub browser_capture_limit_bytes: Option<usize>,

    /// Configured local Magicutor proxy for typed authenticated handoffs.
    pub browser_cdp_url: String,

    /// Per-agent ceiling on which browser transports this execution may use,
    /// copied verbatim from the owner's `AgentDefinition::browser_transports`.
    /// **Empty means all three** — the restriction is opt-in, so an execution
    /// that never learned an owner keeps the behaviour it had before ceilings
    /// existed. Names are validated where the ceiling is applied
    /// (`BrowserTransportCeiling::parse`), so an unrecognised entry refuses the
    /// browse instead of quietly shrinking or widening the set.
    ///
    /// This is a ceiling on the AGENT, not on the work: a delegated child
    /// inherits its parent's work carrier, so a carrier-based rule alone lets an
    /// outsider-steerable agent reach the owner's Chrome through a delegate that
    /// holds a browser. The §5A.2 carrier confinement check in
    /// `dispatch::dispatch_browser_primitive` is complementary, not redundant.
    pub browser_transports: Vec<String>,

    /// Side-channel set by the inner-loop runner when the LLM emits a
    /// terminal control with `keep_browser_cdp_connection_alive=true`
    /// (or the legacy alias `keep_browser_session_alive=true`). The
    /// outer-loop terminal cleanup reads this alongside the static
    /// [`crate::magician_v2::execution::agentic::AgenticContext::keep_browser_cdp_connection_alive`]
    /// flag and the outcome classifier. Threaded from
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors::browser_session_keep_alive_override`].
    pub browser_session_keep_alive_override: Option<Arc<std::sync::atomic::AtomicBool>>,

    /// Side-channel set by the inner-loop runner when the LLM emits a
    /// terminal control with `keep_browser_window_open=true`. The
    /// outer-loop terminal cleanup reads this alongside the static
    /// [`crate::magician_v2::execution::agentic::AgenticContext::keep_browser_window_open`]
    /// flag and, when set without cdp-alive, invokes
    /// `agent-browser close --keep-browser` to detach the daemon
    /// while leaving Chromium visible. See
    /// `docs/plans/2026-05-24-browser-session-lifecycle-redesign.md`.
    pub browser_window_open_override: Option<Arc<std::sync::atomic::AtomicBool>>,

    /// Override for the agent-browser `--session` id. When `Some`, the
    /// inner-loop browser dispatcher passes this to
    /// `AgentBrowserSession::new` instead of deriving the session id
    /// from `thread_id()`. Plumbed from
    /// `AgenticContext.browser_session_id_override` →
    /// `AgenticContextOverrides.browser_session_id_override` →
    /// `GoalTaskOptions.browser_session_id_override`. Set by chat-inline
    /// delegate / handover so child executions share one Chrome window
    /// keyed on the chat thread rather than spawning a fresh window per
    /// `execution_id`. `None` preserves legacy behaviour.
    pub browser_session_id_override: Option<String>,

    /// Secret store handle used by direct inner-loop dispatchers that do not
    /// flow through `ExecutableAction` secret injection. Browser commands use
    /// this to resolve `[REDACTED:<id>]` / `[REF:<id>]` placeholders at the
    /// final argv/stdin boundary, after the LLM has planned with placeholders
    /// only.
    pub secret_store: Option<Arc<crate::magician_v2::secrets::SecretStore>>,

    /// Scope-aware authority used by governed runtimes to resolve declared
    /// secret references against the exact principal/workspace selected by
    /// the invocation. A scope-local store alone cannot prove that binding.
    pub secret_store_resolver: Option<Arc<crate::magician_v2::secrets::SecretStoreResolver>>,

    /// Test/package override for the bounded operator configuration consumed
    /// by governed CLI profile adapters. Production leaves this unset and
    /// reads the runtime-owned configuration path.
    pub governed_operator_config_source: Option<Arc<str>>,

    /// Test/package override for the directory containing the exact regular
    /// governed executable. It is a search directory, never a model-provided
    /// executable or arbitrary argv prefix.
    pub governed_executable_directory: Option<PathBuf>,

    /// API-mining router for live browser-to-API takeover. Threaded from
    /// `ActionExecutors` into the primitive browser dispatcher because flat
    /// `browser__*` calls no longer pass through the legacy browser action enum.
    pub api_router:
        Option<Arc<std::sync::Mutex<crate::magician_v2::api_mining::router::ApiRouter>>>,

    /// Last API replay attempt side-channel consumed by the outer agentic
    /// loop's iteration history projection.
    pub api_replay_last_outcome: Option<
        Arc<std::sync::Mutex<Option<crate::magician_v2::execution::agentic::ApiReplayMeta>>>,
    >,

    /// Scoped API-mining storage root used by the reqwest replay fallback.
    pub api_mining_base_path: Option<PathBuf>,

    /// Last known page URL for API-mining action binding. Browser navigations
    /// (`open`, `tab new/open`, and simple `location.*` evals) update this;
    /// subsequent `click`/`fill`/`press`/body-read actions use it as page context.
    pub api_mining_last_page_url: Option<Arc<std::sync::Mutex<Option<String>>>>,

    /// Accumulated browser action events for post-run correlation with drained
    /// network traces.
    pub api_mining_action_events:
        Option<Arc<std::sync::Mutex<Vec<crate::magician_v2::api_mining::correlator::ActionEvent>>>>,

    /// Lazy sequence recorder slot. The browser dispatcher starts it on the
    /// first recorded browser/API step; the orchestrator finalizes it at run end.
    pub api_mining_sequence_recorder: Option<
        Arc<
            std::sync::Mutex<
                Option<crate::magician_v2::api_mining::sequence_recorder::SequenceRecorder>,
            >,
        >,
    >,

    /// Scoped ephemeral secret namespace for this execution. See
    /// `agentic::executor::ephemeral_secret_scope_id`; the browser dispatcher
    /// forwards this into the shared secret injector so scoped user-input
    /// values do not collide across concurrent executions.
    pub ephemeral_secret_scope_id: Option<String>,
    /// The run's delivered-values set (P4): a typed fill that delivers
    /// material adds the values here so every later observation of the run is
    /// scrubbed with them. See `ActionExecutors::delivered_secret_values`.
    pub delivered_secret_values:
        Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
    /// Set after a typed fill delivered material: what a browser observation
    /// may return until the page moves on, one of the `CAPTURE_WITHHELD_*`
    /// levels. See `ActionExecutors::browser_capture_withheld`.
    pub browser_capture_withheld: Option<Arc<std::sync::atomic::AtomicU8>>,
    /// The run's pending authentication challenge (P4): the governed runtime
    /// records a declared prompt nothing could answer here, so the next
    /// secure ask of that kind binds to the program. See
    /// `ActionExecutors::pending_challenge`.
    pub pending_challenge: Option<
        Arc<
            std::sync::Mutex<
                Option<crate::magician_v2::secrets::challenge::AuthenticationChallenge>,
            >,
        >,
    >,

    /// Central scoped HITL/user-request service. API-mining write replay uses
    /// this to ask for one-shot operator approval before firing a direct
    /// write-like HTTP request. `None` keeps orphan/test dispatchers fail-closed
    /// to the browser rail.
    pub user_request_service: Option<Arc<crate::magician_v2::user_requests::UserRequestService>>,

    /// Optional sink for mid-flight progress lines emitted by an inner-loop
    /// dispatcher (e.g., subprocess stderr lines from the CLI template
    /// dispatcher). Each call publishes a single human-readable string. The
    /// chat path sets this to a closure that publishes an `ActionProgress`
    /// `ProgressMessage` through the existing `inline_pack` subscription so
    /// the user sees the helper's actual progress lines (e.g. `[NANOBANANA2]
    /// generate_content started`) rather than just a heartbeat.
    /// Autonomous-loop dispatch leaves this `None`.
    pub progress_publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,

    /// Optional realtime event broadcaster threaded from
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors::event_broadcaster`].
    /// When present, the inner-loop runner emits structured
    /// `tool.call.started` / `tool.call.finished` events around every tool
    /// dispatch so the UI can render tool calls without scraping feed
    /// messages. `None` for tests / orphan dispatches (no events emitted).
    pub event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,

    /// Optional `CompiledDispatchAuthority` bundle for spend-gated
    /// compiled-pack dispatch inside the inner loop. When present AND
    /// `config.enabled`, gated compiled-provider calls flow through
    /// reserve / commit / rollback ledger bookkeeping via
    /// `compiled_dispatch::dispatch_compiled_provider`. When absent
    /// (tests, callers without RA wiring), gated calls take the
    /// degraded path (warn + execute inner action) the same as chat
    /// did before Phase 1 of the dispatch refactor landed. Threaded
    /// from either
    /// [`crate::magician_v2::execution::agentic::executor::ActionExecutors::compiled_dispatch_authority`]
    /// (autonomous outer loop) or
    /// [`crate::magician_v2::chat::service::ChatService::resource_authority`]
    /// (chat-spawned inner loop). Plan doc:
    /// `docs/archive/plans/2026-05-20-compiled-dispatch-extraction.md` Phase 5.
    pub compiled_dispatch_authority:
        Option<crate::magician_v2::execution::compiled_dispatch::CompiledDispatchAuthority>,

    /// Mid-flight cancellation signal. When the chat path spawns an
    /// inner loop via `dispatch_capability_pack`, it threads its
    /// per-session token (the same one stored in
    /// `ChatService::active_chat_runs`) through to the inner loop so
    /// the inner LLM call and any inner tool dispatch can race against
    /// it. The autonomous outer loop threads its own
    /// `cancellation_token` (kept in
    /// the orchestrator's active execution controls) the same way. When
    /// the chat user clicks Stop or a task is cancelled via
    /// `cancel_execution_tree`, the token fires and every inner-loop
    /// LLM call and CLI subprocess can observe the signal and bail.
    /// `None` for orphan dispatches and unit tests — those simply run
    /// to completion without a cancel surface.
    pub cancellation_token: Option<tokio_util::sync::CancellationToken>,

    /// Artifact-v2 workspace handle, threaded from
    /// `ActionExecutors::artifact_v2_workspace`. Required for the
    /// inner-loop `read_artifact` built-in to resolve an `artifact_id`
    /// back to the persisted file via the execution artifacts index.
    /// `None` for tests / orphan dispatches; `read_artifact` returns a
    /// not-available error in that mode.
    pub artifact_workspace: Option<crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace>,
}

impl std::fmt::Debug for PrimitiveExecCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrimitiveExecCtx")
            .field("storage_base_path", &self.storage_base_path)
            .field("principal", &self.principal)
            .field("workspace", &self.workspace)
            .field("task_id", &self.task_id)
            .field("chat_session_id", &self.chat_session_id)
            .field("execution_id", &self.execution_id)
            .field("legacy_execution_id", &self.legacy_execution_id)
            .field("cycle_id", &self.cycle_id)
            .field("goal_id", &self.goal_id)
            .field("agent_id", &self.agent_id)
            .field("max_spawned_tasks", &self.max_spawned_tasks)
            .field("invocation_context", &self.invocation_context)
            .field("work_authority", &self.work_authority)
            .field("browser_session_used", &self.browser_session_used.is_some())
            .field(
                "browser_additional_session_ids",
                &self.browser_additional_session_ids.is_some(),
            )
            .field("browser_engine", &self.browser_engine)
            .field(
                "browser_capture_limit_bytes",
                &self.browser_capture_limit_bytes,
            )
            .field("browser_cdp_url", &self.browser_cdp_url)
            .field("browser_transports", &self.browser_transports)
            .field(
                "browser_session_keep_alive_override",
                &self.browser_session_keep_alive_override.is_some(),
            )
            .field(
                "browser_window_open_override",
                &self.browser_window_open_override.is_some(),
            )
            .field("secret_store", &self.secret_store.is_some())
            .field(
                "secret_store_resolver",
                &self.secret_store_resolver.is_some(),
            )
            .field(
                "governed_operator_config_source",
                &self.governed_operator_config_source.is_some(),
            )
            .field(
                "governed_executable_directory",
                &self.governed_executable_directory.is_some(),
            )
            .field("api_router", &self.api_router.is_some())
            .field(
                "api_replay_last_outcome",
                &self.api_replay_last_outcome.is_some(),
            )
            .field("api_mining_base_path", &self.api_mining_base_path)
            .field(
                "api_mining_last_page_url",
                &self.api_mining_last_page_url.is_some(),
            )
            .field(
                "api_mining_action_events",
                &self.api_mining_action_events.is_some(),
            )
            .field(
                "api_mining_sequence_recorder",
                &self.api_mining_sequence_recorder.is_some(),
            )
            .field(
                "ephemeral_secret_scope_id",
                &self.ephemeral_secret_scope_id.is_some(),
            )
            .field("user_request_service", &self.user_request_service.is_some())
            .field(
                "compiled_dispatch_authority",
                &self.compiled_dispatch_authority.is_some(),
            )
            .field("cancellation_token", &self.cancellation_token.is_some())
            .field("artifact_workspace", &self.artifact_workspace.is_some())
            .finish()
    }
}

impl PrimitiveExecCtx {
    /// Build the agent-browser-style `--session <id>` value for this
    /// execution. Uses the first available identity field, falling back to
    /// a literal `primitive` for tests / orphan dispatches.
    pub fn thread_id(&self) -> String {
        self.execution_id
            .as_deref()
            .or(self.legacy_execution_id.as_deref())
            .or(self.cycle_id.as_deref())
            .or(self.goal_id.as_deref())
            .or(self.agent_id.as_deref())
            .unwrap_or("primitive")
            .to_string()
    }

    /// The agent-browser `--session` id this execution should attach
    /// to. Returns `browser_session_id_override` when set (chat-inline
    /// delegate / handover plumbing — child shares the parent's browser
    /// window); otherwise falls back to deriving from `thread_id()`,
    /// matching the legacy per-execution behaviour.
    ///
    /// Centralised so both `primitive/dispatch.rs::dispatch_browser_action`
    /// and the outer-loop close hook agree on which session id to use.
    ///
    /// # Engagement partition (§5A.2)
    ///
    /// `agent-browser` keys a launched browser's state — cookies, logged-in
    /// accounts, storage — by `--session <id>`, so two executions that resolve
    /// the same id share one browser. That makes the id the place an
    /// engagement boundary can be drawn, and
    /// [`crate::magician_v2::engagement_retrieval::engagement_browser_session_id`]
    /// draws it: a confined execution gets its own id space, so a research
    /// visit made for one counterparty can never land in the window another
    /// counterparty's visit left behind.
    ///
    /// The namespace is applied to the override too, not only to the derived
    /// id. A parent and child on the same engagement still share a window
    /// (both namespace the same override the same way); a session id handed
    /// across engagements resolves to a different id under each, which is the
    /// point — an override is a request to share, and sharing across
    /// engagements is exactly what must not happen.
    pub fn effective_browser_session_id(&self) -> String {
        let base = if let Some(override_id) = self
            .browser_session_id_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            override_id.to_string()
        } else {
            format!("magician-{}", sanitize_session_id(&self.thread_id()))
        };
        // Every carried arm gets an id space; only a genuinely unbound
        // execution keeps the shared one.
        //
        // An unreadable carrier must not fall back to the shared, unpartitioned
        // id — that is the one execution whose browser state we can say least
        // about. It gets its own namespace instead, so whatever it does is at
        // least not done in a window somebody else will reuse. A `Program`
        // carrier must not fall back to it either, and for a different reason:
        // the carrier is perfectly readable, it is the *scope vocabulary* that
        // cannot express it yet, and answering "unbound" there would put a
        // program's browsing in the owner's own signed-in window.
        //
        // The program's segment is derived from the work key, so it reads
        // `program-<id>` where an engagement reads `<id>`. Those can never
        // collide: an engagement id is a ULID, whose alphabet has no `-`, so no
        // sanitised engagement segment can begin `program-`. The *kind* label
        // that `engagement_browser_session_id` writes ahead of the segment
        // still says `engagement` for both — cosmetic only, nothing parses it
        // back — and it stops saying so once that helper takes the generic
        // carrier (owned by `engagement_retrieval`, outside this change).
        let unreadable = || crate::magician_v2::agents::RetrievalScope::Bound {
            engagement_id: "unreadable".to_string(),
        };
        let scope = match self.work_authority.as_ref() {
            None => crate::magician_v2::agents::RetrievalScope::Unbound,
            Some(carried) => match &carried.work {
                crate::magician_v2::work_context::WorkContextKind::Engagement(_) => {
                    // Through the containment seam rather than open-coding the
                    // rule, so the engagement namespace this derives cannot
                    // drift from the one retrieval filters under.
                    let narrowed =
                        crate::magician_v2::engagements::EngagementAuthorityRef::try_from(carried)
                            .ok();
                    crate::magician_v2::engagement_retrieval::contained_retrieval_scope(
                        narrowed.as_ref(),
                    )
                    .unwrap_or_else(unreadable)
                },
                crate::magician_v2::work_context::WorkContextKind::Program(_) => {
                    crate::magician_v2::agents::RetrievalScope::bound(carried.as_key())
                        .unwrap_or_else(unreadable)
                },
            },
        };
        crate::magician_v2::engagement_retrieval::engagement_browser_session_id(&base, &scope)
    }

    /// Resolve the process storage root. Used by [`Self::default_for_runtime`]
    /// when constructing a context outside an `AgenticContext`.
    pub fn storage_runtime_root() -> PathBuf {
        crate::magician_v2::process_storage::runtime_root()
    }

    /// Construct a default-shaped context suitable for tests or for
    /// runtime callers who only have partial identity information. All
    /// optional fields default to `None`; storage path uses the standard
    /// resolution.
    pub fn default_for_runtime() -> Self {
        Self {
            storage_base_path: Self::storage_runtime_root(),
            principal: None,
            workspace: None,
            task_id: None,
            chat_session_id: None,
            execution_id: None,
            legacy_execution_id: None,
            cycle_id: None,
            goal_id: None,
            agent_id: None,
            max_spawned_tasks: None,
            invocation_context: None,
            work_authority: None,
            session_file_sandbox_roots: None,
            yutori_browser_actions: false,
            browser_session_used: None,
            browser_additional_session_ids: None,
            browser_engine: None,
            browser_capture_limit_bytes: None,
            browser_cdp_url: crate::magician_v2::execution::primitive_dispatch::browser::DEFAULT_MAGICUTOR_PROXY_URL.to_string(),
            // Empty: every transport. The ceiling is opt-in.
            browser_transports: Vec::new(),
            browser_session_keep_alive_override: None,
            browser_window_open_override: None,
            browser_session_id_override: None,
            secret_store: None,
            secret_store_resolver: None,
            governed_operator_config_source: None,
            governed_executable_directory: None,
            api_router: None,
            api_replay_last_outcome: None,
            api_mining_base_path: None,
            api_mining_last_page_url: None,
            api_mining_action_events: None,
            api_mining_sequence_recorder: None,
            ephemeral_secret_scope_id: None,
            delivered_secret_values: None,
            browser_capture_withheld: None,
            pending_challenge: None,
            user_request_service: None,
            progress_publisher: None,
            event_broadcaster: None,
            compiled_dispatch_authority: None,
            cancellation_token: None,
            artifact_workspace: None,
            effect_id: None,
        }
    }

    /// Builder helper: attach this dispatch attempt's identity.
    ///
    /// Set from the lineage identity that `begin_agentic_tool_lineage` already
    /// mints immediately before the call. Any context that is cloned and reused
    /// across attempts MUST overwrite it — a stale effect id is worse than none,
    /// because it would attribute one attempt's effect to another.
    pub fn with_effect_id(mut self, effect_id: Option<String>) -> Self {
        self.effect_id = effect_id;
        self
    }

    /// Builder helper: mark this execution as Yutori-driven so the browser
    /// dispatcher enables the Yutori N1.5 action-translation shim. Defaults to
    /// `false`; `build_primitive_exec_ctx` sets it from the recorded decision
    /// provider.
    pub fn with_yutori_browser_actions(mut self, enabled: bool) -> Self {
        self.yutori_browser_actions = enabled;
        self
    }

    /// Builder helper: attach the artifact-v2 workspace handle so the
    /// `read_artifact` built-in can resolve `artifact_id` → persisted file
    /// via the execution artifacts index. Defaults to `None`; the
    /// boot-time wiring in
    /// `crate::magician_v2::execution::agentic::executor::build_primitive_exec_ctx_from_executors`
    /// clones `ActionExecutors::artifact_v2_workspace` into here so real
    /// runs always have a resolver bound.
    pub fn with_artifact_workspace(
        mut self,
        workspace: Option<crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace>,
    ) -> Self {
        self.artifact_workspace = workspace;
        self
    }

    /// Builder helper: attach the realtime event broadcaster so the inner
    /// loop can emit structured `tool.call.*` events around dispatches.
    pub fn with_event_broadcaster(
        mut self,
        broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    ) -> Self {
        self.event_broadcaster = broadcaster;
        self
    }

    /// Builder helper: attach the scoped user-request service for primitive
    /// dispatchers that need to pause on Attention/HITL.
    pub fn with_user_request_service(
        mut self,
        service: Option<Arc<crate::magician_v2::user_requests::UserRequestService>>,
    ) -> Self {
        self.user_request_service = service;
        self
    }

    /// Builder helper: attach the `CompiledDispatchAuthority` bundle so
    /// the inner-loop's `CompiledProviderDispatcher` can charge the
    /// spend ledger for gated compiled-pack calls. `None` (the default)
    /// keeps the degraded path — gated calls run without ledger
    /// bookkeeping. Boot-time wiring forwards the same bundle the chat
    /// fast-path uses, so the same `magician-config.yaml > resource_authority`
    /// budgets govern both callers.
    pub fn with_compiled_dispatch_authority(
        mut self,
        authority: Option<
            crate::magician_v2::execution::compiled_dispatch::CompiledDispatchAuthority,
        >,
    ) -> Self {
        self.compiled_dispatch_authority = authority;
        self
    }

    /// Builder helper: attach the mid-flight cancellation token from the
    /// caller. Chat passes its per-session token (cloned from
    /// `ChatService::active_chat_runs`); the autonomous outer loop
    /// passes the execution's token from
    /// the orchestrator's active execution controls. The inner-loop
    /// runner races every LLM call against this token and inner
    /// dispatchers (CLI template, browser) check it between iterations
    /// — cancelling the token aborts inner work and stops billing for
    /// unproduced LLM tokens via reqwest stream drop. `None` (the
    /// default) keeps the existing run-to-completion behavior for
    /// tests / orphan dispatches.
    pub fn with_cancellation_token(
        mut self,
        token: Option<tokio_util::sync::CancellationToken>,
    ) -> Self {
        self.cancellation_token = token;
        self
    }

    /// Builder helper: attach a mid-flight progress sink. Inner-loop
    /// dispatchers (e.g., the CLI template) call this on each line of
    /// subprocess stderr while the work is in progress. Returning quickly is
    /// the caller's responsibility — the publisher should not block.
    pub fn with_progress_publisher(
        mut self,
        publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
    ) -> Self {
        self.progress_publisher = publisher;
        self
    }

    /// Builder helper: attach the browser-session-used flag (the
    /// `AtomicBool` lives on `ActionExecutors`; the dispatcher flips it
    /// to `true` on first browser pack invocation).
    pub fn with_browser_session_used(
        mut self,
        used: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        self.browser_session_used = used;
        self
    }

    pub fn with_browser_additional_session_ids(
        mut self,
        session_ids: Option<Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>>,
    ) -> Self {
        self.browser_additional_session_ids = session_ids;
        self
    }

    /// Builder helper: thread the execution's operator-approved sandbox roots
    /// (from `ActionExecutors::session_file_sandbox_roots`) so file-touching
    /// compiled handlers can widen their sandbox after a sandbox-override HITL.
    pub fn with_session_file_sandbox_roots(
        mut self,
        roots: Option<Arc<std::sync::Mutex<std::collections::HashSet<String>>>>,
    ) -> Self {
        self.session_file_sandbox_roots = roots;
        self
    }

    /// Builder helper: attach the LLM-driven keep-CDP-alive override
    /// flag. The runner sets the underlying `AtomicBool` to `true`
    /// when it observes a terminal control call with
    /// `keep_browser_cdp_connection_alive=true` (or the legacy
    /// `keep_browser_session_alive=true` alias).
    pub fn with_browser_session_keep_alive_override(
        mut self,
        flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        self.browser_session_keep_alive_override = flag;
        self
    }

    /// Builder helper: attach the LLM-driven keep-window-open override
    /// flag. The runner sets the underlying `AtomicBool` to `true`
    /// when it observes a terminal control call with
    /// `keep_browser_window_open=true`.
    pub fn with_browser_window_open_override(
        mut self,
        flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        self.browser_window_open_override = flag;
        self
    }

    /// Builder helper: attach the agent-browser `--session` id override.
    /// When `Some`, the inner-loop browser dispatcher uses this verbatim
    /// instead of deriving the session id from `thread_id()`. Chat-inline
    /// delegate / handover paths set this so child executions share one
    /// Chrome window keyed on the chat thread.
    pub fn with_browser_session_id_override(mut self, session_id: Option<String>) -> Self {
        self.browser_session_id_override = session_id;
        self
    }

    /// Builder helper: attach the secret context used by direct dispatchers
    /// such as browser. The LLM sees only placeholders; dispatch resolves them
    /// immediately before invoking the underlying CLI and sanitizes command
    /// output before returning it to the loop.
    pub fn with_secret_context(
        mut self,
        store: Option<Arc<crate::magician_v2::secrets::SecretStore>>,
        ephemeral_scope_id: Option<String>,
    ) -> Self {
        self.secret_store = store;
        self.ephemeral_secret_scope_id = ephemeral_scope_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Attach the run's delivered-values set and capture-withheld flag (P4).
    pub fn with_delivery_tracking(
        mut self,
        delivered: Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>,
        capture_withheld: Arc<std::sync::atomic::AtomicU8>,
        pending_challenge: Arc<
            std::sync::Mutex<
                Option<crate::magician_v2::secrets::challenge::AuthenticationChallenge>,
            >,
        >,
    ) -> Self {
        self.delivered_secret_values = Some(delivered);
        self.browser_capture_withheld = Some(capture_withheld);
        self.pending_challenge = Some(pending_challenge);
        self
    }

    /// Attach the scope-aware secret authority used by governed credential
    /// preparation. Values remain sealed inside the resolver/adapter boundary.
    pub fn with_secret_store_resolver(
        mut self,
        resolver: Option<Arc<crate::magician_v2::secrets::SecretStoreResolver>>,
    ) -> Self {
        self.secret_store_resolver = resolver;
        self
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_governed_runtime_overrides(
        mut self,
        operator_config_source: impl Into<Arc<str>>,
        executable_directory: PathBuf,
    ) -> Self {
        self.governed_operator_config_source = Some(operator_config_source.into());
        self.governed_executable_directory = Some(executable_directory);
        self
    }

    /// Builder helper: attach API-mining live takeover state used by browser
    /// primitive dispatch.
    pub fn with_api_mining_runtime(
        mut self,
        router: Option<Arc<std::sync::Mutex<crate::magician_v2::api_mining::router::ApiRouter>>>,
        replay_last_outcome: Option<
            Arc<std::sync::Mutex<Option<crate::magician_v2::execution::agentic::ApiReplayMeta>>>,
        >,
        base_path: Option<PathBuf>,
        last_page_url: Option<Arc<std::sync::Mutex<Option<String>>>>,
        action_events: Option<
            Arc<std::sync::Mutex<Vec<crate::magician_v2::api_mining::correlator::ActionEvent>>>,
        >,
        sequence_recorder: Option<
            Arc<
                std::sync::Mutex<
                    Option<crate::magician_v2::api_mining::sequence_recorder::SequenceRecorder>,
                >,
            >,
        >,
    ) -> Self {
        self.api_router = router;
        self.api_replay_last_outcome = replay_last_outcome;
        self.api_mining_base_path = base_path;
        self.api_mining_last_page_url = last_page_url;
        self.api_mining_action_events = action_events;
        self.api_mining_sequence_recorder = sequence_recorder;
        self
    }

    /// Builder helper: set the principal / workspace / task scope.
    pub fn with_scope(
        mut self,
        principal: Option<String>,
        workspace: Option<String>,
        task_id: Option<String>,
    ) -> Self {
        self.principal = principal;
        self.workspace = workspace;
        self.task_id = task_id;
        self
    }

    /// Builder helper: set the execution identity.
    pub fn with_execution(
        mut self,
        execution_id: Option<String>,
        legacy_execution_id: Option<String>,
        cycle_id: Option<String>,
        goal_id: Option<String>,
        agent_id: Option<String>,
    ) -> Self {
        self.execution_id = execution_id;
        self.legacy_execution_id = legacy_execution_id;
        self.cycle_id = cycle_id;
        self.goal_id = goal_id;
        self.agent_id = agent_id;
        self
    }

    /// Builder helper: attach the work authority ref stamped from the
    /// executors' runtime slot. Ctx-only by design — dispatch must never fall
    /// back to a model-supplied parameter for this value.
    pub fn with_work_authority(
        mut self,
        work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,
    ) -> Self {
        self.work_authority = work_authority;
        self
    }

    /// Attach the server-resolved invocation lane used by authorization-aware
    /// compiled tools. Absence remains absence and therefore fails closed.
    pub fn with_invocation_context(
        mut self,
        invocation_context: Option<crate::magician_v2::agents::AgentInvocationContext>,
    ) -> Self {
        self.invocation_context = invocation_context;
        self
    }

    /// Builder helper: attach authoritative chat-session lineage.
    pub fn with_chat_session(mut self, chat_session_id: Option<String>) -> Self {
        self.chat_session_id = chat_session_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Builder helper: set the maximum number of tasks this execution may spawn.
    pub fn with_max_spawned_tasks(mut self, max_spawned_tasks: Option<u32>) -> Self {
        self.max_spawned_tasks = max_spawned_tasks;
        self
    }

    pub fn with_browser_engine(mut self, browser_engine: Option<String>) -> Self {
        self.browser_engine = browser_engine;
        self
    }

    pub fn with_browser_capture_limit_bytes(mut self, limit: Option<usize>) -> Self {
        self.browser_capture_limit_bytes = limit;
        self
    }

    pub fn with_browser_cdp_url(mut self, cdp_url: String) -> Self {
        self.browser_cdp_url = cdp_url;
        self
    }

    /// Builder helper: carry the owner agent's declared browser-transport
    /// ceiling into flat dispatch. An empty list is the unrestricted default,
    /// so passing `Vec::new()` is the same as never calling this.
    pub fn with_browser_transports(mut self, browser_transports: Vec<String>) -> Self {
        self.browser_transports = browser_transports;
        self
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn thread_id_prefers_execution_id() {
        let ctx = PrimitiveExecCtx {
            execution_id: Some("exec-1".into()),
            legacy_execution_id: Some("legacy-1".into()),
            cycle_id: Some("cycle-1".into()),
            goal_id: Some("goal-1".into()),
            agent_id: Some("agent-1".into()),
            ..PrimitiveExecCtx::default_for_runtime()
        };
        assert_eq!(ctx.thread_id(), "exec-1");
    }

    #[test]
    fn thread_id_falls_back_through_identity_chain() {
        let mut ctx = PrimitiveExecCtx::default_for_runtime();
        ctx.legacy_execution_id = Some("legacy".into());
        ctx.cycle_id = Some("cycle".into());
        assert_eq!(ctx.thread_id(), "legacy");

        ctx.legacy_execution_id = None;
        assert_eq!(ctx.thread_id(), "cycle");

        ctx.cycle_id = None;
        ctx.goal_id = Some("goal".into());
        assert_eq!(ctx.thread_id(), "goal");

        ctx.goal_id = None;
        ctx.agent_id = Some("agent".into());
        assert_eq!(ctx.thread_id(), "agent");

        ctx.agent_id = None;
        assert_eq!(ctx.thread_id(), "primitive");
    }

    #[test]
    fn default_storage_base_path_uses_env_var_when_set() {
        // Don't mutate process env in this test; just verify construction uses
        // the shared default resolver instead of carrying its own fallback.
        let ctx = PrimitiveExecCtx::default_for_runtime();
        assert_eq!(
            ctx.storage_base_path,
            crate::magician_v2::process_storage::runtime_root()
        );
    }

    #[test]
    fn objective_id_is_stable_for_same_capability_and_goal_frame() {
        let seed = SeedGoalBlock::from_goal(
            "complete all visible tests",
            Some("all tests marked Pass".to_string()),
        );

        let first = primitive_objective_id("browser", &seed);
        let second = primitive_objective_id("browser", &seed);

        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(primitive_objective_text(&seed).contains("complete all visible tests"));
        assert_ne!(
            first,
            primitive_objective_id("gmail", &seed),
            "capability is part of the objective identity"
        );
    }

    #[test]
    fn with_scope_and_with_execution_chain() {
        let ctx = PrimitiveExecCtx::default_for_runtime()
            .with_scope(
                Some("anonymous".into()),
                Some("default".into()),
                Some("task-1".into()),
            )
            .with_execution(
                Some("exec-1".into()),
                None,
                Some("cycle-1".into()),
                None,
                Some("agent-1".into()),
            )
            .with_chat_session(Some("  chat-session-1  ".into()));
        assert_eq!(ctx.principal.as_deref(), Some("anonymous"));
        assert_eq!(ctx.workspace.as_deref(), Some("default"));
        assert_eq!(ctx.task_id.as_deref(), Some("task-1"));
        assert_eq!(ctx.execution_id.as_deref(), Some("exec-1"));
        assert_eq!(ctx.cycle_id.as_deref(), Some("cycle-1"));
        assert_eq!(ctx.chat_session_id.as_deref(), Some("chat-session-1"));
        assert_eq!(ctx.thread_id(), "exec-1");
    }

    #[test]
    fn blank_chat_session_lineage_is_not_propagated() {
        let ctx =
            PrimitiveExecCtx::default_for_runtime().with_chat_session(Some(" \t\n ".to_string()));

        assert_eq!(ctx.chat_session_id, None);
    }

    /// Pins that a program-confined execution does NOT browse in the owner's
    /// shared window, and does not share one with any other program either.
    ///
    /// The failure this stops is a program carrier falling through to
    /// `RetrievalScope::Unbound` because the scope vocabulary has no program
    /// form. The agent-browser CLI keys cookies and signed-in accounts by
    /// session id, so an unpartitioned id means a program's research visit
    /// lands in the window holding the owner's own logged-in sessions.
    #[test]
    fn a_program_browses_in_its_own_session_namespace() {
        use crate::magician_v2::work_context::{WorkAuthorityRef, WorkContextKind};

        let carrying = |work: Option<WorkContextKind>| {
            let mut ctx = PrimitiveExecCtx::default_for_runtime();
            ctx.execution_id = Some("exec-1".to_string());
            ctx.work_authority = work
                .map(|work| WorkAuthorityRef::new(work, 0).expect("a safe id for a test carrier"));
            ctx.effective_browser_session_id()
        };

        let unbound = carrying(None);
        let program_a = carrying(Some(WorkContextKind::Program("prog-a".to_string())));
        let program_b = carrying(Some(WorkContextKind::Program("prog-b".to_string())));

        assert_ne!(
            program_a, unbound,
            "a program must not resolve to the shared, unpartitioned session the owner's own \
             runs use"
        );
        assert_ne!(
            program_a, program_b,
            "two programs sharing one browser would share cookies and signed-in accounts"
        );

        // And an engagement keeps the namespace it already had, so no existing
        // browser profile is orphaned by generalising the carrier. `01H` is a
        // ULID-shaped id; the alphabet has no `-`, which is why a sanitised
        // engagement segment can never collide with a program's `program-…`.
        let engagement = carrying(Some(WorkContextKind::Engagement("01H".to_string())));
        assert!(
            engagement.contains("scope-engagement-01h-"),
            "the engagement namespace must be byte-stable, got: {engagement}"
        );
        assert_ne!(
            engagement,
            carrying(Some(WorkContextKind::Program("01H".to_string())))
        );
    }
}
