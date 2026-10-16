# Runtime Async Stack Boundaries

Some Magician runtime paths have async poll chains or synchronous builder frames
deep enough to overflow an ordinary Rust/Tokio stack. They use
definition-level type erasure, scheduler-root joined tasks, bounded synchronous
builders and an isolated execution pool on Tokio's ordinary stack policy, not a
process-wide `RUST_MIN_STACK` or larger worker stacks. Measured deep polls get
a temporary stack segment ([last section](#temporary-stack-segments)).

The budget is a debug build on a 2 MiB worker stack, which the `default_stack_`
test family and the Makefile's `env -u RUST_MIN_STACK` hold the runtime to.
Release builds pack frames tighter; the rules are the same.

## Runtimes and scale

Boot (`magician-bin`) loads config and resolves `runtime.scale` before it builds
the main, execution or Lance Tokio runtimes. Seed profile `current` keeps four
execution workers and four dedicated Lance workers; `MAGICIAN_SCALE_PROFILE` and
explicit `runtime.scale.overrides.*` integers take effect on restart. Blocking
admission defaults to 16 permits on `current`
(`runtime.scale.overrides.blocking_admission_permits`; `0` is unlimited).
`MAGICIAN_BLOCKING_ADMISSION=off` skips the semaphore. Tokio
`max_blocking_threads` is not reduced.

The execution runtime is registered with `execution::runtime_boundary`. Its
workers are named `magician-execution-worker` and use Tokio's ordinary stack
policy: no `thread_stack_size` override, no process-wide `RUST_MIN_STACK`.

## Ownership rule

Any producer that can start or resume a full agentic loop passes an owned, lazy
closure through `spawn_execution_job` / `run_execution_job`, so the deep future
is built, boxed and first polled on an execution worker. Actix and
`magician-bg` workers only schedule or await a small join handle.

- **Lazy**, because building the future first still constructs it on the
  caller's (HTTP) stack. Heap-pinning on the Actix worker changes storage, not
  which thread owns the poll stack.
- **Joined**, because an unjoined `tokio::spawn` could leave paid provider/tool
  work detached. Awaited jobs abort when the waiter drops, so an HTTP disconnect
  cancels the work.
- **Context carried across:** coding context, scoped secret context,
  cancellation token, and the exact shared execution-token meter (a
  captured-meter carrier restores the same `Arc`, so nested and refinement
  segments share one counter). App-owner credentials are re-scoped in the child.

Producers covered: non-streaming and streaming chat
(`ChatService::process_message*_on_execution_runtime`, which copy borrowed
request fields into owned values; the current-runtime implementations are
private), Tutor HTTP turns (web and mobile), contextual writing, voice notes and
Ambient Dictation (`submit_voice_note_handler`), cascaded HandsFree voice (the
WebSocket actor receives the terminal result as a message), live-voice
Tutor/App Copilot takeovers (`submit_tutor_takeover_turn`), inline chat
capability packs, Artifact V3 background runs, scheduled and chat-triggered goal
cycles, delegated children, manual triggers (prompt-pipeline assembly and
task-backed root provisioning are separate joined scheduler-root tasks),
approval auto-resume, HITL answer/resume and async continue, exact paused
resume, durable continuation re-entry, eval, and direct or initial-message HTTP
execution jobs. Source-contract tests enumerate these producers, so a new
ambient launch fails the normal suite.

`execute_agentically` returns a heap-owned, type-erased future, and
`execute_agentically_scoped` passes a lazy closure through `run_execution_job`
before creating the scheduler-root lane. Runtime ownership is therefore an
executor invariant: startup recovery, child reconciliation, supervisor wakes and
embedded callers cannot run a full execution on an Actix or `magician-bg`
worker.

## Scheduler-root lane

The execution runtime's workers still have ordinary stacks, and the agentic loop
is deep. Each execution pre-spawns one private scheduler-root lane
(`SchedulerRootLane`) before the core loop is polled. Its shallow receiver
builds jobs into a structured `JoinSet` rather than awaiting them inline, so a
job can await nested jobs without deadlock. Dropping a result receiver cancels
that job; dropping the execution aborts the lane worker. Deep direct-path code
never calls `tokio::spawn` or builds a `JoinSet`.

Work on the lane (construction and poll depth both reset):

- The whole direct-action path (`execute_direct_path_on_scheduler_root`):
  preflight, dispatch, verification, post-action state, history mutation. It
  temporarily owns history, current state, last-action context, recovery
  context and loop detector, and swaps them back exactly once; long histories
  and browser state are moved, not cloned. Concurrent read-only follow-ups
  enter the same way.
- Decision / provider polling (`decide_next_action`), browser-aware state
  building and fail-closed projection sanitization. Immutable decision inputs
  and the active-owner definition are shared through `Arc`.
- `tool_search` deferred-name ceilings, derived from the exact authority binding
  and provider-visible dispatch set; the full schema catalog is never rebuilt on
  the deep stack.
- Delegated-child admission (`delegate_to_agent`: policy resolution, Artifact
  V2 scheduling, persistence, parent attachment, resource admission, launch).
  The decision handler submits a lazy dispatch factory to the lane; explicit
  orchestrator delegation and verification repair use the joined
  execution-runtime wrapper. Those are the only production sites allowed to
  call `DelegationDispatcher::spawn_children`. Timing logs report queue wait
  apart from dispatcher timings.
- Heavy owner/persistence seams: owner-profile loads and refreshes,
  handover/yield-back transitions, terminal precision judging, durable
  task-state reads/writes and paused/terminal execution-summary writes, through
  one context-preserving helper. Between segments (no active lane) it falls back
  to the joined process execution runtime.
- Owner convergence and staged fast/hybrid/procedure memory retrieval, on owned
  read-only snapshots. The helper takes a lazy factory, so the three-stage
  future is built and polled on the lane (heap-pinning it on the decision stack
  would still build it there and can overflow during owner refresh). Each stage
  and their joined future are boxed before generic scope wrappers receive them.
  Caller drop cancels the retrieval job; its deadline and fail-open handling are
  unchanged.
- LanceDB request-path retrieval (memory FTS/hybrid scoring, filtered procedure
  search) crosses a second, narrower scheduler-root boundary right before
  DataFusion builds a query plan, because its synchronous SQL-filter parser is
  deep. The permit wait stays inside the retrieval timeout; caller drop or
  timeout aborts the search.
- One-time stack-heavy initialization: embedded capability YAML
  (`include_str!`) is parsed once into a process cache that catalog assembly
  borrows; credential-redaction regexes are warmed at the scheduler root before
  the executor future is built.
- Typed `PathAccessDenied` is classified synchronously before the boxed
  sandbox-override HITL future is built, so ordinary provider errors carry no
  dormant approval state through the direct-action frame.

## Fresh execution-runtime tasks

These sites hand an awaited job to a fresh execution-runtime task because they
run while a full agent/tool poll chain is still unwinding:

- Terminal output finalizers (execution-output, task-user and task-user
  projection) receive owned contexts before bundle building or LLM synthesis.
  Ordering, retries, routing overrides and cancellation are preserved.
  Production call sites may not invoke model-backed finalizers inline (source
  contract).
- Task-backed status projection into Artifact V2
  (`persist_runtime_execution_outcome_by_execution_id`,
  `persist_external_execution_outcome`). This is needed even for tiny JSON:
  typed Serde decoding of a small `TaskManifest` at the bottom of the
  orchestration chain has exhausted a worker stack. The inline helper is private
  to Artifact V2.
- Delegated-child settlement passes the one authorized parent continuation
  through `spawn_execution_job`, so parent and child executor chains never nest
  across delegation levels; the single-flight terminal transaction is kept.
- Durable continuation re-entry starts at a fresh task boundary.

## Definition-level type erasure

A heap-owned, type-erased future returned by the callee keeps its state machine
(and its poll/drop glue) out of every caller's concrete future. A caller-side
`Box::pin(async move { ... })` is not equivalent, nor is a fresh scheduler task
alone when task-local wrappers would duplicate the state across debug poll
frames. Erased at definition:

- `execute_agentically`, `execute_agentically_resume_with_validation`, and the
  cycle/refinement wrappers that enter the recursive core executor.
- Chat service/API edges (`process_message*`, `process_chat_inline_turn`, the
  SSE task in `magician-api/src/chat_api.rs`) and the fast/hybrid memory
  renderer. Every inline-turn caller (including Tutor background continuation)
  boxes the nested turn, and every Chat/realtime-voice caller boxes the shared
  outer-loop prompt renderer.
- HITL exact resume: `web_api.rs` and
  `StateTransitionService::resume_execution_tree` box
  `MagicianV2Orchestrator::resume_execution_tree`, calling the inherent method
  explicitly across the same-named async-trait boundary.
- Direct dispatch boxes one provider-family future before polling it. The Pack
  path heap-owns every workflow, disclosure, resource, physical-effect and
  reliability child; the Pack arm is the named `execute_pack_dispatch` with a
  sync `pack_dispatch_future` factory.
- Apps physical-effect entry points (compiled, MCP, OS-jail, interactive) and
  the shared effect owner's coordinator children. Task/root guards,
  cancellation and durable settlement ordering are unchanged.
- The app launch chain (`launch_direct_app_action`, `execute_admitted_bridge`,
  `AppWorkflowService::invoke*`): every hop is boxed, siblings included.
- The scheduled goal cycle is a `GoalCycleJob` (`agents/runtime.rs`) whose
  stages `admit_launch`, `run_pipeline` and `settle` each return a
  definition-erased future; only the running stage is on the stack.
- The runtime resume recovery worker chain (coordinator → worker → candidate →
  drive → durable-receipt resume) and `execute_agentically_resume_exact`
  (which boxes its inner future at construction).
- The pause-commit tail below `prepare_terminal_loop_settlement_receipt`, and
  `execute_agentically_inner`, whose body lives in `execute_agentically_run` so
  the `#[instrument]` wrapper does not hold its temporaries.
- Artifact V2 startup synthesis reconciliation heap-pins each task's recovery
  future.

Synchronous builders are split into bounded frames: `build_compiled_registry`
builds provider families in separate frames; cold memory-snapshot overlap-graph
materialization uses explicit bounded loops.

## Heap and JSON admission boundaries

Scheduler-root handoff protects async poll stacks, but not recursive Serde
traversal or whole-payload cloning. Runtime JSON has a separate admission
contract:

- Retained external `serde_json::Value` trees are rejected or truncated at depth
  64 (empty containers at the boundary count), then traversed, canonicalized,
  redacted and disposed with explicit heap-owned work stacks. Rejected deep
  values are drained iteratively so recursive drop glue cannot overflow.
  Provider-bound sanitization builds only the retained-depth prefix.
- Artifact projection combines secret, path, internal-field, ownership, depth
  and node filtering in one bounded traversal.
- Synthesis prompt shaping drains over-deep/over-wide JSON iteratively;
  response fence stripping stays borrowed until the byte ceiling has passed;
  grounding JSON uses the shared heap-stack pretty writer.
- Encoded-size checks use a counting writer, not a second buffer. Tool-result
  projection measures the borrowed wire before materializing it.
- Canonical tool results enforce byte/node/depth ceilings, digest through an
  iterative writer and stream file-backed values atomically. Authenticated reads
  verify size/hash in 64 KiB chunks, rewind and reapply admission in the Serde
  pass.
- Canonical result selection runs a heap-stack preorder visitor twice (count,
  then page), aborts each entry measurement at 64 KiB + 1 and materializes only
  the returned page.
- Execution-artifact indexes admit encoded bytes before Serde, cap records and
  nodes, and stream atomic rewrites; routing prefers the persisted logical type
  header over parsing the body.
- Inline secret substitution uses the same heap-stack walker; the
  unresolved-reference guard inspects the typed action without serializing it.
- Accessibility extraction and Merkle assembly are iterative; the retained
  accessibility tree is capped at 64 levels; DOM/CDP builders keep their
  50-level guards.
- Terminal execution cleanup never deserializes the full pause payload (the
  generated `AgenticPauseState` visitor is a large synchronous frame): the pause
  store keeps an execution-to-storage-key index and deletes by exact hashed
  filename; before hydration it reads only `pause_state.execution_id`.

These are data-shape bounds, not answer-quality truncation. Data that cannot
meet a retained-value contract fails closed with a typed error; larger results
need a streaming contract, not larger stacks.

## Recovery and task-graph invariants

- The startup synthesis sweep budgets each task (20 s) and has no sweep-wide
  deadline, so a busy boot cannot strand tasks after a cut. It logs
  `scanned / scheduled / timed_out / elapsed_ms` when it did anything. It does
  not re-attach delegated children of an already-terminal task.
- A child runtime watcher stops when the task is terminal and gives up after 30
  reconciliation attempts (about a minute at the 2 s backoff cap) with one
  ERROR, rather than re-queuing synthesis for a dead task.
- A task-record write must never call a guarded reader (`get_task`,
  `get_execution`) while holding the task write guard: the guard is an exclusive
  file lock, and re-acquiring it on a fresh descriptor in the same process
  blocks forever.
- Writing a child execution as `active_root_execution_id` or
  `latest_root_execution_id` logs `[REDUCER] a child execution is being written
  as the task root pointer`.
- Child-terminal → parent-resume recursion is bounded by its own walk state
  (storage does not reject a cyclic parent link). A `ParentResumeWalk` is
  threaded through `persist_execution_outcome_within_walk` →
  `handle_child_terminal` → `handle_child_terminal_claimed` →
  `maybe_resume_parent_after_v3_child_results`. It stops before revisiting an
  execution, at a self-parent, or at `MAX_PARENT_RESUME_WALK_DEPTH`, logs the
  trail and hands the parent to the wake-up queue for a fresh walk. Entry
  points outside a walk start a fresh one.
- Crash recovery finds `tasks/` and `internal_tasks/` executions alike. An
  interrupted `Executing` document recovers to `Runnable`; the orphan sweep then
  fails runs without live in-process controls and projects that outcome to
  task/chat surfaces. Execution settlement runs before chat-delegate terminal
  recovery, or the chat sweep can see the stale non-terminal state.
- A deterministic crash on a resume path loops under the supervisor's
  unexpected-exit restart (the durable receipt, not task status, drives the
  recovery worker; `catch_unwind` bounds panics, not aborts).
  `supervisor-ctl stop-magician` breaks the loop.
- Initial browser navigation arms observation only for a typed initial URL or an
  explicit `http(s)://` URL; bare tokens such as `main.go` are not taken as
  hosts.

## Qualification

`scripts/run-supervisor.sh` and `scripts/run-rust-tests-with-report.sh` remove
inherited `RUST_MIN_STACK`. `MAGICIAN_EMERGENCY_RUST_MIN_STACK` is for incident
recovery only; it warns and is not a supported configuration.

`make test-agentic-default-stack` is the focused lane: architecture and
adversarial-depth regressions, lazy-construction and waiter-drop contracts,
`tool_search` and delegated-admission reproductions on the ordinary execution
runtime, a Pack HTTP preflight-to-dispatch reproduction, and an ignored stress
case of 1,000 sequential agentic actions through a real in-memory provider
(asserting 1,001 iterations and exactly 1,000 provider executions). `make
test-rust` runs the non-stress contracts. Named budget tests include
`scheduled_goal_cycle_fits_the_default_execution_worker_stack` and
`default_stack_a_pause_commit_fits_an_ordinary_worker` (pinned at 2 MiB).

Provider-backed full-loop fixtures build and poll their graph on
`build_execution_runtime()` via a small lazy closure; the default-stack lane
stays the independent detector for executor growth. Restart integration tests
build each non-`Send` Actix lifecycle in its own blocking job and drop it before
the next; holding both in one async-test future can itself overflow.

A SIGABRT from stack overflow cannot be reported as a failed assertion; in a
suite it can surface after every case printed `ok`.

## Diagnosing and fixing an overflow

Do not remove existing erased boundaries, turn one back into an `async fn` /
opaque `impl Future`, or compensate with a larger stack. First tell a monolithic
synchronous frame from a nested async poll chain.

Measuring:

- Frame widths can be read from the binary: each aarch64 prologue names its
  frame in the stack-probe target `sub x9, sp, #N` (plus post-probe remainder
  and pre-indexed `stp`). A crash report's symbol + `imageOffset` and image UUID
  pin the build; no reproduction is needed. When the unwinder fails inside
  `___chkstk_darwin`, walk the `x29` chain by hand.
- Measure the deployed artifact (`magician.bin` at the repo root, built by the
  Makefile), not a stray `target/`.
- Capture at the `RUST_MIN_STACK` value where it just aborts; a much smaller
  value catches a shallower, misleading stack.
- Equal-width frames across unequal functions mean one embedded object (a child
  state machine awaited inline); it names the call to box.
- A repeated symbol is not recursion: check frame indices (an async fn's
  outer/inner closure with a tracing wrapper between them).
- No crash is evidence only if the path under test actually ran.

Fixing:

- Debug builds give every awaited temporary its own slot and reserve a whole
  frame at entry, so an inline await reserves the child's full state machine
  for every branch, taken or not.
- A future from another `async fn` awaited inline → box the **call**. Boxing a
  locally built block (`Box::pin(work)`) does nothing: it is built on the stack
  first. Returning a definition-erased future from the callee is stronger.
- A `match` with large arms → lift the big arms; the frame is `max(arms)`.
- A long linear body → split regions into separate boxed futures so each
  region's locals die before the next allocates. `Box::pin` moves state to the
  heap; only a split moves the poll frame.
- A lift helps only when the lifted region is not running at the deep moment.
  Isolate the deep call in the smallest frame and lift what runs around it.
  Re-measure: if a lift makes the frame larger, revert it.
- A region is safe to wrap only if it exits via `?` / `return Err`; an early
  `return Ok(..)` would be swallowed by the inner block and needs an explicit
  done/continue encoding. Use `async {` (not `async move`) to keep by-reference
  capture. Check bracket deltas are zero and that `git diff -w` shows only the
  wrapper lines; avoid re-indenting raw strings.
- `#[instrument]` and a long body must not share a function on a deep path: the
  wrapper holds the body's construction temporaries. Move the body into its own
  boxed future.
- Boxing inside a `LocalKey::scope` does not change task-local propagation.
- Rare arms that always return are lifted whole and boxed at the call, e.g.
  `resolve_decision_error` (`phases/resolve.rs`) and
  `conclude_effect_indeterminate` (`executor.rs`).
- Deep one-off work that does not need the loop's stack belongs on the
  `SchedulerRootLane`, as decide already does.

Security-sensitive regions in `execute_action_inner` (outward gate,
app-workflow authorization, engagement-ceiling authority) mix early returns with
fall-through and stay inline; reshaping them needs a compile and test run, not a
mechanical edit.

## Temporary stack segments

`execution::stack_headroom::with_stack_headroom` runs every poll of a wrapped,
heap-pinned future on a fresh 8 MiB `stacker` segment (`SEGMENT`; the red zone
equals the segment, so on a 2 MiB worker every wrapped poll switches). Wrapped:
children spawned into a structured set with the execution token meter
(`spawn_with_execution_token_meter_in_set`, `agentic/types.rs`), run-loop driver phases
(`run_loop/driver_worker.rs`), the terminal success gate (`executor.rs`),
recovery direct and sleeping-retry runs (`agents/runtime.rs`), App workflow
launch admission (`apps/workflows.rs`) and the detached HTTP resume
(`web_api.rs`). The `off_frame!` macro (`magician/src/lib.rs`) boxes an awaited
call so its state leaves the caller's frame. Runtime worker threads keep
Tokio's 2 MiB default.
