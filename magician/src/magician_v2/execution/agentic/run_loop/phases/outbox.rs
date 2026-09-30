//! The producer half of the event outbox: a phase's transport events, turned
//! into [`JournalAppend`] records.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Event emission is an
//! outbox, not an inline call*. The rule is that a phase **appends** the event
//! it wants and a projector emits it, because a phase that emitted inline emits
//! again every time it re-runs after a crash.
//!
//! # READ THIS BEFORE BUDGETING FOR THE PROTECTION
//!
//! Delivery now has exactly one path per produced event. A record accepted by
//! the outbox is emitted by the projector; an event that cannot be journalled
//! (including the resident in-process arm, which has no drain) is emitted
//! inline. That fallback is load-bearing: dropping inline emission merely
//! because the stateless arm owns a projector made every in-process timeline
//! go blank, while emitting on both paths duplicates stateless timelines.
//!
//! What this module *does* buy, stated so nobody has to re-derive it: the
//! records exist, they are produced by production code on the real paths, and
//! the conversion that produces them is exercised rather than imagined.
//!
//! # THE DRAIN EXISTS ON THE DEFAULT STATELESS ARM
//!
//! The one channel from a phase to an append is
//! `driver_worker::PhaseReport::records`, assembled by
//! `executor.rs::InProcessWorkerHost`. That host is the stateless arm's live
//! adapter and drains this buffer once per phase.
//!
//! `run_loop::driver_inproc::run_iteration` is the explicit rollback arm. It
//! calls phases directly, assembles no `PhaseReport`, and has no drain.
//!
//! [`journalling_has_a_drain`] therefore refuses to buffer while the process is
//! on the rollback arm. Without that gate an in-process run
//! filled [`MAX_PENDING_BYTES_PER_RUN`] within tens of iterations — the
//! `AgenticDecisionMade` and `LLMResponseReceived` records are kilobytes each —
//! warned that its buffer was full, held 256 KiB per run until the process
//! ended, and paid two JSON encodes
//! per event — three, before 2026-08-28 — for records that were guaranteed to
//! be discarded.
//!
//! The long switch analysis below is chronological rationale. Current behavior
//! is the exact-one-path rule at the top of this module: accepted records are
//! projector-only; refusals and the no-drain rollback arm emit inline.
//!
//! # CUTTING INLINE EMISSION IS NOT A ONE-LINE CHANGE
//!
//! An earlier cut of this doc said that when the drain lands, cutting inline
//! emission is deleting the `executors.emit_event(event)` in [`journal_and_emit`]
//! and no call site changes. **That was wrong**, and it is the claim that sizes
//! the whole remaining project, so it is corrected here rather than deleted.
//!
//! `ActionExecutors::emit_event` (`executor.rs`) is not a transport
//! shim. It decides, per event, whether the event is a **canonical runtime fact**
//! or transport-only:
//!
//! - With a broadcaster: `map_v2_realtime_event(&event)` and
//!   `self.canonical_event_scope`. When the mapped `execution_id` equals the
//!   scope's, `broadcaster.emit(event)` — which persists a runtime fact.
//!   Otherwise `broadcaster.emit_transport_only(event)`.
//! - With no broadcaster: `self.canonical_event_sink.emit(scope.clone(),
//!   mapped.event_type, mapped.payload)`, again gated on the same scope
//!   comparison — and note the missing arm: with no broadcaster, an event whose
//!   mapped id does NOT match the scope reaches no transport at all. There is no
//!   `emit_transport_only` on that path. [`routing_for`] records
//!   `TransportOnly` for it, which is the event's CLASSIFICATION and not a claim
//!   that anything was sent.
//!
//! **`broadcaster.emit` is not `emit_transport_only` plus persistence**, and the
//! shorthand is worth refusing before somebody builds a projector on it. `emit`
//! (`realtime_events.rs:4376`) additionally derives
//! `v3_planning_progress_for_event` (`:4392`) and fans it out as a *second*,
//! transport-only event for `planexec_`-prefixed runs (`:4409`);
//! `emit_transport_only` (`:4167`) never produces it. Replay is unaffected — a
//! `TransportOnly` record put back through `emit_transport_only` loses nothing it
//! ever had — but the delta between the branches is persistence PLUS a live
//! fan-out, not persistence alone.
//!
//! ## THE RECORD NOW CARRIES THAT DECISION — corrected 2026-08-27
//!
//! What this section said next was that the decision came from *state the record
//! does not carry*, so a sink built on [`rejoin`] alone would take every event
//! down the transport-only branch, keep every live surface working, and silently
//! stop the persisted runtime-fact stream. **The first half is no longer true and
//! the second half is exactly what would still happen to a sink that ignores the
//! new field**, so both are kept.
//!
//! [`JournalBody::Event`] carries a
//! [`RecordedEventRouting`](super::super::journal::RecordedEventRouting):
//! `CanonicalRuntimeFact { scope }`, `TransportOnly`, or `Unrecorded`. The
//! producer computes it in [`routing_for`] from the same three fields
//! `emit_event` reads, and `ProjectorCursor::project` hands it to the sink
//! through `ProjectedEventSink::emit_routed`.
//!
//! **It is the DECISION, not a copy of `emit_event`'s branch selection**, and the
//! difference is load-bearing: `emit_event`'s branch is the decision multiplied
//! by the transports the producing process happened to hold, and a producer with
//! a canonical event and no `canonical_event_sink` emits nothing at all.
//! Recording *that* would tell a replayer the event was not a runtime fact, which
//! is false. See [`routing_for`] for the rule and for why the scope on a
//! set of executors always names the executors' own execution.
//!
//! ## TWO THINGS THIS DOES NOT DISCHARGE, and the third was discharged
//!
//! 1. ~~**`HostEventSink` does not read it yet.**~~ — **BOTH OVERRIDES LANDED
//!    2026-08-28, and this item is condition 2, which is now MET.** Struck
//!    through rather than deleted because it was the load-bearing claim of the
//!    section above it, and a reader who finds the old text elsewhere needs to
//!    know which way it was resolved.
//!
//!    `ProjectedEventSink::emit_routed` has a DEFAULT that forwards to `emit`
//!    and drops the routing, and `driver_worker::HostEventSink` was on it — so
//!    the field was written, carried, and discarded one call before the only
//!    party that can act on it. It now overrides `emit_routed` and hands the
//!    routing to the host, and `executor.rs::InProcessWorkerHost::
//!    emit_projected_event` honours all three `RecordedEventRouting` arms,
//!    reading the record rather than re-deriving from a scope a replaying
//!    process may not hold.
//!
//!    **This census read condition 2 as open until 2026-08-28**, three sections
//!    of it, while both overrides were already in the tree. Two files
//!    disagreeing about whether a gate is met is how a nearly-met gate gets
//!    budgeted as far from met — the same failure the numbers below had.
//! 2. ~~**[`routing_for`] is a second implementation of the predicate.**~~ —
//!    **CLOSED 2026-08-28, and it was a THIRD implementation, not a second.**
//!    `emit_event` wrote the rule out twice on its own: as
//!    `should_emit_runtime_fact` in its broadcaster branch and again inlined
//!    into its canonical-sink branch.
//!
//!    All three now call
//!    [`canonical_runtime_fact_of`](crate::magician_v2::artifact_v2::canonical_runtime_fact_of),
//!    which lives beside `map_v2_realtime_event` and the scope it compares
//!    against.
//!
//!    **The fix was NOT what this item said it was.** "`emit_event` calling
//!    this one" cannot work: [`routing_for`] asks a fourth question —
//!    whether the scope's execution is the one whose journal the record is
//!    being filed in — which produces `Unrecorded`, an answer a live emit has
//!    no use for because it has no record. Calling it from `emit_event` would
//!    have meant inventing a `journal_execution_id` that does not exist there,
//!    making that branch unreachable by construction and hiding the difference
//!    rather than removing the duplication. Only the genuinely shared part was
//!    folded together.
//! 3. ~~**Twenty-one KNOWN emission sites still record nothing.**~~ —
//!    **CLOSED 2026-08-28**, and this is condition 1. The seven that were owed
//!    a diff were converted; the remaining fourteen are deliberate and every
//!    one carries its refusal at its own site.
//!
//!    The number went UP before it came down, and the shape of that is worth
//!    keeping: it was twelve when this paragraph was first written, eleven of
//!    those twelve were real, the fourth sweep found ten more the earlier
//!    method could not see, and eleven became twenty-one before becoming
//!    fourteen. **Nothing in the code regressed at any point.** Every movement
//!    was a property of how the sweeps were built, which is the argument for
//!    reading *METHOD, AND WHAT IT CANNOT SEE* before trusting either number:
//!    fourteen is a floor rather than a total, for the same reason
//!    twenty-one was.
//!
//! ## What it cost, since the alternative was cheaper
//!
//! The other answer was to hand the **live host's** scope to the sink. That
//! makes the decision a property of *the process doing the projecting*: a cold
//! lifecycle projector holds no process-local scope, and an ordinary
//! `broadcaster.emit` would still reach live transports while silently omitting
//! canonical persistence. The record therefore carries the producer's scope,
//! and both the resident host and host-free terminal projector use the
//! broadcaster's recorded-scope entry point. It refuses a missing sink or a
//! mapped execution mismatch instead of advancing the durable cursor after a
//! transport-only downgrade.
//!
//! What the record pays for it:
//!
//! - **Bytes.** A canonical record carries four extra strings. Against
//!   [`MAX_JOURNAL_RECORD_BYTES`] that is a fraction of a percent; against a
//!   small control event it is a large *relative* increase, and it moves the
//!   size refusal in [`build`] by that much. The four are a run-level constant
//!   repeated per record — the scope is fixed before the executors enter their
//!   `Arc` and is never mutated after — so this is redundancy paid for
//!   self-description.
//! - **Staleness by design.** A record replays under the rule that was in force
//!   when it was written. A later fix to the routing rule does not reach records
//!   already on disk.
//! - **A fourth thing a `JournalBody::Event` can be wrong about**, and the
//!   `Unrecorded` variant exists so that being wrong is visible rather than
//!   indistinguishable from `TransportOnly`.
//!
//! What it does NOT pay: a copy of the execution id. The scope on the record is
//! four fields, not five, because the id is a run-level constant and a per-line
//! copy would buy nothing.
//!
//! **That is not the same as the journal file naming the run**, which is what
//! this paragraph said until 2026-08-28. The file's key is a loop-state address
//! — `{exec}` for a plain first run, `{exec}-r{n}` / `{exec}-n{…}` /
//! `{exec}-p{n}` for a resumed, nested or refinement-pass invocation — so on
//! those three shapes the key names no execution at all. **A reader must take
//! the fifth field from the run, never from the key**; see
//! [`routing_for`]'s *IT IS NOT THE FILE'S KEY* and
//! [`RecordedCanonicalScope`](super::super::journal::RecordedCanonicalScope)'s
//! *THE FILE KEY IS NOT THE EXECUTION ID*. This was listed as a fourth
//! undischarged thing, due the moment condition 1 was met, because the sink
//! that overrides `emit_routed` is the first reader that has to answer it.
//!
//! **It is DISCHARGED**, and it was discharged before condition 1 rather than
//! after: `executor.rs::InProcessWorkerHost::emit_projected_runtime_fact`
//! takes the fifth field from `runtime_execution_id_opt(self.ctx)` — the run's
//! own id, the same value `phases::outbox::journal` resolved when it wrote the
//! record — and a run with no execution id gets a warning and a dropped event
//! rather than one filed under a fabricated id.
//!
//! # WHAT IS NOT JOURNALLED
//!
//! **Fifteen known emission sites reach a transport and record nothing, and
//! every one of them carries a written refusal at its own site. Fifty go
//! through this module.** Re-swept and re-counted 2026-08-28, fourth sweep;
//! the seven that were OWED A DIFF were converted later the same day, which is
//! what moved 43/21 to 50/14.
//!
//! Read *METHOD, AND WHAT IT CANNOT SEE* before using either number: the
//! unjournalled figure is a **floor**, it went UP by ten on the fourth sweep
//! while one site was being converted, and the reason it went up is a property
//! of how the earlier sweeps were built rather than of anything that changed in
//! the code. A fifth sweep may well find a fifteenth.
//!
//! ## THE COUNTING UNIT, stated once because three censuses disagreed
//!
//! An **emission site** here is *one call, in code the loop owns, that reaches
//! a transport* — counted where the loop's own code makes the call, because
//! that is the granularity at which a `PhaseAddress` can be threaded.
//!
//! Two consequences, and both have bitten:
//!
//! - **A site is not an emission.** Several sites emit more than one event per
//!   call, and two of them emit a number that varies per call:
//!   `emit_completed_tool_dispatch` emits between one and seven records
//!   depending on where the dispatch failed, and
//!   `executor.rs::emit_agentic_tool_consumption` emits one per queued lineage.
//!   A reader sizing the buffer or the ordinal space from a site count will be
//!   wrong in the unsafe direction.
//! - **A journalled site can still emit something unjournalled.**
//!   `emit_agent_execution_mapping` takes a `PhaseAddress` and is journalled
//!   from seventeen of its eighteen call sites; the eighteenth passes `None`
//!   and emits the run's `agent.execution.mapping` envelope with no record.
//!   A site-count census cannot see that one at all, which is why it is listed
//!   below in its own right and why the refusal is written at the `None`.
//!
//! ## THE FIFTY, reproducible from three greps
//!
//! - **9** direct phase emissions on the transport rail (`decide` 5,
//!   `epilogue` 2, `prepare` 1, `apply` 1), each a call to
//!   [`journal_and_emit`]. A recursive grep for that call in the phases
//!   directory reads **9**, exactly the call sites — and it reads 9 only
//!   because this section names the function through a doc link rather than
//!   writing out the call syntax. It read 10 while an earlier version quoted
//!   the pattern and matched itself, and the note explaining the extra match
//!   was itself the extra match. Do not re-introduce the literal form here.
//! - **36** in `executor.rs`, routed through its `journal_and_emit_at`, which
//!   forwards here whenever the threaded `PhaseAddress` is `Some`. It was 28
//!   until 2026-08-28, when `emit_agentic_tool_consumption` became the
//!   twenty-ninth and the seven owed-a-diff conversions took it to 36.
//!
//!   **A bare count of that name in that file reads 55, and 55 is not the
//!   number.** Eighteen of those matches are prose — the conversions added
//!   doc comments that name the function — so the count to take is
//!   non-comment lines only, which reads **37**, one of which is the
//!   definition. This is the same self-matching hazard the phase-rail bullet
//!   above records, arriving by the other door: there it was the census
//!   quoting the call syntax, here it is the converted sites explaining
//!   themselves. A grep for a function name counts the documentation too.
//! - **5** on the **named** rail — four in
//!   `executor.rs::emit_step_events_if_signaled` (`plan.step.started` and
//!   `plan.step.finished`, in the completed branch and the failed branch) and
//!   one in `project_autonomous_action_result` (`tool.result.projected`), all
//!   through `journal_and_emit_named_at` into [`journal_and_emit_named`]. A
//!   count of that name reads **6**, one of which is the definition.
//!
//! ## THE FIFTEEN, by what each is owed
//!
//! Grouped by the ANSWER rather than by the file, because the answer is what
//! condition 3 on [`journal_and_emit`] is checked against. Every one of them
//! carries its refusal, or its owed-a-diff note, **at its own site** — the
//! census is the index, not the record. A refusal that lives only here is one
//! that rots the next time a fifty-thousand-line file moves.
//!
//! ### Refused: outside any phase body (7 sites, 10 emissions)
//!
//! No phase is on the stack and none follows, so no drain would ever take a
//! record produced there. See `executor.rs`'s `PhaseAddress` for the two
//! failures such a record takes, neither of which it can carry a warning
//! about.
//!
//! 1. `AgenticMaxIterationsReached` — `execute_agentically_inner`, after the
//!    iteration loop has finished.
//! 2. Its paired `HitlRequested`, same place.
//! 3. `AgenticExecutionStarted` — `build_run_setup`, before iteration 1.
//! 4. `emit_agent_execution_mapping(ctx, executors, None)` — the same
//!    `build_run_setup` block, and the site a site-count census cannot see.
//!    **New to this census on 2026-08-28.**
//! 5. `agent_events::emit_agent_cycle_started` — `execute_agent_cycle`.
//! 6. `emit_agent_cycle_completed`, success branch — same function.
//! 7. `emit_agent_cycle_completed`, failure branch — same function.
//!
//!    Sites 5–7 are **new to this census on 2026-08-28** and each is two
//!    emissions, not one: a `broadcaster.emit(..)` of the typed cycle event —
//!    the PERSISTING rail — plus a `publish_agent_update` for the operator
//!    feed. They are outside not merely a phase but the **loop**:
//!    `execute_agent_cycle` wraps the function that contains the iteration
//!    loop. No downward closure from a phase body can reach them, which is
//!    blind spot 5 below.
//!
//! ### Refused: already durable by another path (2 sites)
//!
//! 8. `capture_pack_action_artifacts_projection`'s
//!    `canonical_event_sink.emit(.., ArtifactCreated, ..)`.
//! 9. `capture_pack_action_inline_result_artifact`'s, the same.
//!
//!    This is a **finding, not a deferral** — journalling this rail makes the
//!    timeline worse, because a replay appends a second canonical record for
//!    one artifact. See *FOUR VOCABULARIES* below for the derivation and both
//!    sites for the refusal.
//!
//! ### Refused: behind a trait object, and the trait is the wrong place (1 site)
//!
//! 22. `OwnershipRuntime::update_execution_owner_snapshot` →
//!     `orchestrator::v2_orchestrator::emit_execution_responsibility_changed_for`
//!     → `broadcaster.emit(ExecutionResponsibilityChanged)`, the persisting
//!     rail. **New to this census on 2026-08-28**, found by the class-4 sweep.
//!     Five frames in `executor.rs` reach it and one of them is
//!     `handle_handover_to_agent_decision`, so a phase does cause it.
//!
//!     Refused for two reasons, the second of which is specific to this shape:
//!     [`journal_and_emit_at`] is not nameable from `orchestrator` (the
//!     obstruction sites 10–12 hit), and threading a `PhaseAddress` means
//!     putting one on a trait that exists to keep the loop from knowing what
//!     the orchestrator is.
//!
//!     **The authority half is already journalled** —
//!     `JournalBody::OwnerTransition` moves the ceiling on replay — so what a
//!     replay loses is the notification, not the run's authority. The refusal
//!     is written at the call site in `executor.rs::prepare_owner_transition_inner`.
//!
//! ### Refused: no address exists, and one cannot be made (5 sites)
//!
//! 10. `analytics/operation_llm_telemetry.rs::emit_request` — `LLMRequestSent`.
//! 11. …`::emit_failure` — `LLMResponseReceived`.
//! 12. …`::emit_usage_outcome` — `LLMResponseReceived`.
//!
//!     A phase IS on the stack for all three, so this is the one refusal in
//!     the list that is not about a missing frame. That file's header lists
//!     three obstructions in cost order, and the first is that
//!     [`journal_and_emit`] is `pub(in crate::magician_v2::execution::agentic)`
//!     and not nameable from outside this subtree.
//!
//!     **Which phase reaches which was wrong here until 2026-08-28**, and it
//!     is corrected rather than dropped because a fixer trusting the old text
//!     would thread an address into the wrong phase. This census attributed
//!     all three to `decide.rs` building an `OperationLlmTelemetryContext` per
//!     decision. That is the route for site 12 only. Sites 10 and 11 are
//!     reached from **Apply**: `phases/apply.rs` calls
//!     `executor.rs::review_terminal_draft_against_opened_evidence`, which
//!     builds its **own** context and calls both. Site 12 additionally has a
//!     **third** reaching family this census named nowhere —
//!     `execution/compiled_providers.rs`'s `analyze_image_via_openai`
//!     capability provider, a dispatched pack reached from Apply's tool
//!     dispatch through a `dyn CapabilityProvider`. Eleven public methods
//!     funnel into site 12, and its callers include a phase body, a pack
//!     provider, background workers and API helpers with no run behind them.
//!     That is why it cannot take a single phase address.
//!
//! 13. `analytics/llm_trace_activation.rs::emit_activity_cost` — `ActivityCost`,
//!     one per priced-or-local LLM call, on the task subscribed to the LLM
//!     queue's broadcast.
//! 14. `analytics/runtime_activity_layer.rs::spawn_activity_forwarder` —
//!     `ActivityStarted` / `ActivityFinished`, off a process-global channel
//!     inside a `tokio::spawn`.
//!
//!     The **off-stack pair**: not merely outside a phase body but not on the
//!     phase's stack at all. See *THE OFF-STACK PAIR* below.
//!
//! ### ~~OWED A DIFF~~ — CONVERTED 2026-08-28 (7 sites, no longer counted)
//!
//! These held condition 1 closed, correctly: each was a real hole in a
//! replayed timeline rather than a deliberate exclusion. All seven were new to
//! this census on 2026-08-28 and all seven were converted the same day, so
//! they are **struck from the fourteen** and listed here only so a reader who
//! finds the old text elsewhere knows which way it resolved.
//!
//! Every one took the same shape — an envelope-returning form beside the
//! emitting one, so the record is built once and either sent or journalled —
//! **except site 17**, which is noted at its entry below.
//!
//! 15. `llm_tool_lineage::emit_tool_linkage_gap`, from
//!     `executor.rs::begin_agentic_tool_lineage` — 1 emission.
//! 16. `…::emit_tool_proposed`, same function — 1 emission.
//!
//!     **DONE.** `begin_agentic_tool_lineage` already held an
//!     `at: PhaseAddress` — threaded through its nine phase callers for
//!     `emit_agentic_lineage_mapping_gap` — so each needed only its own
//!     envelope-returning form beside the emitting one:
//!     `tool_proposed_envelope` and `tool_linkage_gap_envelope`. The extra
//!     care the linkage gap needed was honoured: `emit_completed_tool_dispatch`
//!     still reaches its record through the same pipeline, and the emitting
//!     form is unchanged.
//!
//!     The `broadcaster` BINDING in that function went with the conversion;
//!     the `as_ref()?` guard did not, and must not. `journal_and_emit_at` is
//!     total, so without it a run with no broadcaster would record emissions
//!     the producer never made.
//!
//! 17. `…::emit_completed_tool_dispatch`, from
//!     `executor.rs::finish_agentic_tool_lineage` — **1 to 7** emissions,
//!     through eight early returns.
//! 18. `…::emit_tool_rollback`, same function — 2 emissions.
//! 19. `…::emit_tool_branch_materialized`, same function — 1 emission.
//!
//!     **DONE.** `finish_agentic_tool_lineage` now takes a `PhaseAddress`,
//!     threaded through all nine phase callers — seven across `phases::apply`'s
//!     gate and dispatch halves, passing `Some((iteration, Phase::Apply))`, and
//!     two in `phases::resolve::run`, passing `Some((iteration, Phase::Resolve))`.
//!
//!     **Site 17 was not split, it was RESTRUCTURED**, and the difference is
//!     the whole point. Writing an envelope-returning twin beside the emitter
//!     would have duplicated a control flow with eight early returns, and this
//!     entry's own warning says what that costs: get one branch wrong and a
//!     stage record goes missing with nothing to say it is missing. So the
//!     pipeline was moved into `completed_tool_dispatch_records`, which exists
//!     ONCE; `emit_completed_tool_dispatch` and
//!     `completed_tool_dispatch_envelopes` are both thin wrappers over the
//!     vector it returns. There is deliberately no second branch structure to
//!     drift. Sites 18 and 19 took the mechanical shape, 18 returning a pair
//!     rather than an `Option` because a rollback missing either half reads as
//!     one that never ends or never begins.
//!
//! 20. `agent_update_emitter::publish_agent_update` — `ArtifactCreateFailed`,
//!     from `capture_pack_action_artifacts_projection`.
//! 21. The same, `ArtifactCreated`, from the same function.
//!
//!     **DONE.** `publish_agent_update` was split — `agent_update_envelope`
//!     builds, the emitter sends — and `at` was threaded from
//!     `execute_direct_path_post_setup_tail`, which had held one all along,
//!     through `capture_pack_action_artifacts_projection`. The test-only
//!     wrapper passes `None`, which emits without recording: a harness call
//!     has no phase to address.
//!
//!     Site 21 sits **inside** the `if let (Some(sink), Some(scope))` block
//!     whose canonical emission is site 8, and the two answers still differ
//!     because the destinations do: site 8 is already durable and a replay
//!     would double it, while this operator-feed envelope is ephemeral and a
//!     crash loses it outright. **The refusal on site 8 must not be read as
//!     covering site 21** — that is why one of the two was converted and the
//!     other was not, three lines apart.
//!
//! ## WHAT THE 2026-08-28 CONVERSION MOVED, and what it did not
//!
//! One site left the unjournalled list on this sweep:
//! `executor.rs::emit_agentic_tool_consumption`, the last of the two
//! `AgentEventEnvelope` sites this census said were "owed an address, not a
//! variant". Both halves of that claim held.
//! `analytics::llm_tool_lineage::emit_tool_lineage_record` was split into a
//! builder and an emitter, so the envelope is produced once and journalled
//! rather than rebuilt at the call site, and `phases/resolve.rs` passes
//! `Some((iteration, Phase::Resolve))`.
//!
//! **The list still got longer**, from eleven to twenty-one, and nothing in
//! the code changed to make that true. Ten sites were found by sweeping
//! `executor.rs`'s entire emission vocabulary — every identifier in it whose
//! name contains `emit`, then each of those resolved to what it reaches —
//! instead of filtering reachable bodies for a fixed list of emitter names.
//! That is the correction the next reader most needs, and it is written up in
//! *METHOD* below as blind spot 5.
//!
//! ## Deliberately excluded, in both directions
//!
//! - **Everything downstream of the bus.** `feed/*`'s projections,
//!   `chat::service`'s re-emissions and the other broadcaster subscribers all
//!   fire *because an event was already emitted*, so a projector replaying
//!   journalled events re-drives them for free. Journalling those would double
//!   them.
//! - **The chat path's own calls into `llm_tool_lineage`.** `chat::service`
//!   is not the agentic loop: no `AgenticContext`, no phase, no outbox.
//!   `emit_tool_result_consumed` stays as the unjournalled entry point for it,
//!   which is why that function survived the split above.
//! - **`OperationLlmRouter`'s own `LLMResponseReceived` sites**
//!   (`query_analysis/operation_llm_router.rs`) are **unresolved**, not
//!   excluded and not counted. `decide.rs` reaches an LLM through
//!   `executors.native_adapter`, and no sweep so far has traced that trait
//!   object to a provider. They are the strongest remaining candidates for a
//!   twenty-second site.
//!
//! ## THE OFF-STACK PAIR, and why the refusal is not the same one as the others
//!
//! Sites 1–7 have no phase on the stack because the loop has not started or
//! has finished. Sites 13 and 14 have no phase on the stack because **they are
//! not on the phase's stack at all**: one runs on the task subscribed to the
//! LLM queue's broadcast, the other on the forwarder task draining
//! `runtime_activity_layer`'s global channel. The phase that caused the work
//! has typically returned — and on the `tracing` side the producer is a
//! `Layer` callback that fires on whatever thread held the span, which
//! *AN AMBIENT ADDRESS CANNOT WORK HERE* already shows cannot carry an address
//! across a `tokio::spawn`.
//!
//! Journalling either would mean carrying `(execution_id, iteration, phase)`
//! through the queue message and through `ActivityRecord`, and then answering
//! what a record means when the phase it names committed three phases ago.
//! Both are real designs; neither is a phase-local change, and neither is owed
//! before the flip.
//!
//! There is also a reason not to want them journalled that is independent of
//! cost: they are **telemetry about** a run, not events **of** it. An outbox
//! replays a run's timeline; a cost row and an activity span are observations
//! of the process that ran it, and their absence from a replayed timeline is
//! not a hole in that timeline.
//!
//! ## METHOD, AND WHAT IT CANNOT SEE
//!
//! Swept 2026-08-27 (twice), 2026-08-28 (twice). Each sweep corrected the one
//! before it, and the correction was never a code change — it was always the
//! method reaching somewhere the previous method could not.
//!
//! **What the first three did.** Build a call graph over every `fn` in
//! `executor.rs`; seed it with every identifier the six phase files call that
//! resolves to one of them; close it transitively; then filter each reachable
//! body for a fixed list of **seven emitter names** — the two
//! `ActionExecutors` methods, the three `RuntimeTransportBroadcaster` ones,
//! the canonical sink's `emit`, and `emit_tool_result_consumed`. Then run the
//! same filter over every module the phase files name directly.
//!
//! (Those seven are described rather than quoted, here and everywhere else in
//! the tree. They are grep patterns somebody counts, and a quoted copy makes a
//! correct tree read as drifted — see
//! `InProcessWorkerHost::emit_projected_event`'s COUNTING NOTE, which records
//! the same hazard for its own line.)
//!
//! **What it cannot see.** A call-graph closure finds emitters a phase
//! *calls*. Five ways to reach a transport without being one:
//!
//! 1. **A queue or a broadcast subscription.** The phase enqueues; a
//!    background task emits. Site 13 is this shape, behind `LlmQueueEvent`.
//! 2. **The `tracing` registry.** The phase opens a span; a `Layer` writes a
//!    record and a forwarder task emits it. Site 14 is this shape.
//! 3. **A `tokio::spawn` inside the loop.** `SchedulerRootLane` already breaks
//!    ambient addressing for this reason — see *AN AMBIENT ADDRESS CANNOT WORK
//!    HERE* — and it breaks the sweep the same way when the spawned body is
//!    not a named `fn` in `executor.rs`.
//! 4. **A trait object.** The closure resolves identifiers to definitions in
//!    one file; a `dyn` call to an implementation elsewhere is a dead end. Two
//!    live instances: the `native_adapter` the `Decide` phase drives, which is
//!    why the operation router's sites are unresolved rather than absent; and
//!    the `dyn CapabilityProvider` that carries site 12's third reaching
//!    family, which is how a whole caller family went unnamed.
//! 5. **A NAME THAT IS NOT IN THE FILTER, and a FRAME THAT IS NOT BELOW A
//!    PHASE.** Added 2026-08-28, and it accounts for ten of the twenty-one.
//!    Two distinct failures, both of the same kind — the filter answers a
//!    question narrower than the one being asked:
//!
//!    - **The name.** Every miss is a free function in another module that
//!      *ends* at one of the seven and whose own name is not among them:
//!      `llm_tool_lineage`'s six other `emit_tool_*` entry points (sites
//!      15–19), `agent_update_emitter::publish_agent_update` (sites 20–21),
//!      `agents::events::emit_agent_cycle_*` (sites 5–7). Note that
//!      `emit_tool_result_consumed` **was** in the filter — one member of a
//!      seven-function family, hardcoded — which is precisely how the other
//!      six were missed. A filter naming members instead of funnels finds what
//!      it was told about and nothing else.
//!    - **The frame.** The closure is seeded from phase bodies and closed
//!      DOWNWARD, so it cannot see an emitter on a frame **above** the loop.
//!      `execute_agent_cycle` wraps the function that contains the iteration
//!      loop; sites 5–7 live there and no downward closure will ever reach
//!      them.
//!
//!    **The fourth sweep's method, so it can be repeated rather than
//!    reinvented:** enumerate every identifier in `executor.rs` containing
//!    `emit` — a count of about thirty distinct names — resolve each to what
//!    it actually reaches, and only then decide. That finds the funnels
//!    instead of the members. It is still blind to classes 1–4.
//!
//! **All five classes have now been swept.** Classes 3 and 4 were done on
//! 2026-08-28, and they are the second clause of condition 3 — the one closing
//! the seven owed-a-diff sites did not touch.
//!
//! - **Class 3 found nothing.** Seven production `tokio::spawn`s in
//!   `executor.rs` (four more are in `mod tests`); every spawned body writes
//!   storage or moves a lifecycle flag — a decision-record store, a work-ledger
//!   append, a program-state distillation, a frontmatter stamp and reindex, and
//!   the deadline watchdog. Each body's callee was read as well as the body:
//!   none reaches a broadcaster, an `emit_event` or the canonical sink. The
//!   callee check went ONE level deep, which is the honest limit of it.
//! - **Class 4 found one, and it is exactly the shape the class predicts.**
//!   `OwnershipRuntime::update_execution_owner_snapshot` — held by the loop as
//!   `Arc<dyn ..>` — reaches `orchestrator::v2_orchestrator::
//!   emit_execution_responsibility_changed_for`, which calls
//!   `broadcaster.emit(ExecutionResponsibilityChanged)`. The PERSISTING rail,
//!   through a `dyn` call the closure treats as a dead end. It is site 22
//!   below, and it is REFUSED. The other three `broadcaster.emit`s in that file
//!   sit on orchestrator-level operations the loop never calls through the
//!   trait, and the four `CapabilityProvider` implementations that touch
//!   `operation_llm_telemetry` are site 12's already-known third family.
//!
//!   Worth recording HOW it was nearly missed a second time: the first pass
//!   swept the trait IMPL BODIES and reported clean on a file the census
//!   already knew reaches a transport, because `execute` delegates to an
//!   inherent method outside the impl block. A sweep scoped to the trait
//!   surface answers a narrower question than the one being asked — the same
//!   failure as the name filter in class 5, one level up.
//!
//! So the honest statement of this census is now *"fifty journalled, fifteen
//! known unjournalled, every one of the fifteen carrying a written refusal, and
//! the population is still a lower bound"* — and condition 3 on
//! [`journal_and_emit`] is written to be checked against that sentence rather
//! than against the number. It remains a lower bound because the class-3 callee
//! check is one level deep and because a sixth class may exist that no sweep has
//! named yet.
//!
//! **What "phase-reachable" no longer claims.** Earlier cuts of this section
//! said the phase closure "covers that file's entire emission surface" and
//! that "there is no emission in that file a phase cannot cause". Both were
//! false in the same direction, and sites 5–7 are the counterexample: they are
//! in `executor.rs`, they fire on every agent cycle, and no phase causes them
//! — the frame above the loop does. The population is not "all of
//! `executor.rs`" and never was.
//!
//! ## FOUR VOCABULARIES REACH A TRANSPORT — AND THREE OF THE FOUR ARE ONE TYPE
//!
//! **The VOCABULARY conclusion has held through two re-sweeps; the SITE COUNTS
//! in the table have not, and are removed rather than re-guessed.** An earlier
//! version said five vocabularies reached a transport and that nine sites
//! "cannot" be held by `JournalBody::Event` because they are not
//! `RuntimeTransportEvent`s. **Seven of those nine are.** The wrong rows are
//! kept struck through, because the wrong reason is what a reader would
//! otherwise re-derive.
//!
//! The per-row counts that used to sit here were a snapshot of one sweep's
//! population, and every later sweep moved them while the vocabulary answer
//! stayed put — which is exactly the shape that makes a table look stale and
//! its conclusion look doubtful. Counts live in *WHAT IS NOT JOURNALLED*
//! above, in one place, with the greps that reproduce them.
//!
//! | vocabulary | can `JournalBody::Event` hold it? |
//! |---|---|
//! | `RuntimeTransportEvent` via `ActionExecutors::emit_event` | yes |
//! | `RuntimeTransportEvent` via `emit_transport_only`, outside `executor.rs` | yes |
//! | ~~a named event: no variant tag~~ → `emit_named` | **yes, and it must not** — see below |
//! | ~~`AgentEventEnvelope`: a different type entirely~~ | **yes** — `emit_agent_transport_event` IS a transport-only `AgentEvent` send |
//! | `ArtifactV2EventType` to `canonical_event_sink` | no — not a `RuntimeTransportEvent` at all |
//!
//! **The fourth row is the one that generalised furthest.** It was written for
//! two sites; the same answer now covers **nine** — those two, both journalled
//! since 2026-08-28, plus the seven at census numbers 15–21, which all end at
//! the same transport-only `AgentEvent` send through
//! `llm_tool_lineage::emit_tool_lineage_record` or
//! `agent_update_emitter::publish_agent_update`. Not one of the nine needs a
//! new record variant. Every one needs the same two things: an address, and a
//! builder that produces the envelope without sending it.
//!
//! ### The `AgentEventEnvelope` rail: the claim was FALSE, and both sites shipped
//!
//! `RuntimeTransportBroadcaster::emit_agent_transport_event` is two lines: a
//! transport-only send of `RuntimeTransportEvent::AgentEvent { event }`. The
//! variant tag is `AgentEvent`; there is no second type. And routing one of
//! these through `ActionExecutors::emit_event` delivers **identically**, not
//! merely similarly: `map_v2_realtime_event` has no `AgentEvent` arm at all
//! (catch-all `_ => None`), so `should_emit_runtime_fact` is `false` and
//! `emit_event` takes its transport-only branch — the same call. So these need
//! `journal_and_emit_at` and **no new variant**. What they needed instead was
//! an address.
//!
//! **Both of the two are now journalled.**
//! `emit_agentic_lineage_mapping_gap` took a `PhaseAddress` threaded through
//! `begin_agentic_tool_lineage`'s nine phase callers on 2026-08-27, and
//! `emit_agentic_tool_consumption` took one on 2026-08-28, which also required
//! splitting `analytics::llm_tool_lineage::emit_tool_lineage_record` into a
//! builder (`tool_lineage_envelope`) and an emitter so the envelope could be
//! produced without being sent. Rebuilding it at the call site was the
//! alternative and is the second event vocabulary this design refuses.
//!
//! **That split is the reusable half**, and it is why sites 15–21 of the
//! census are "owed a diff" rather than blocked: every one of them wants the
//! same builder/emitter pair, in the same shape, in one of two files.
//!
//! ### The five named sites: it fits, and putting it there would be wrong
//!
//! `emit_named` also ends at `RuntimeTransportEvent::AgentEvent`. It is a second
//! variant anyway, and the reason is the DELIVERY rather than the type — see
//! [`JournalBody::NamedEvent`](super::super::journal::JournalBody::NamedEvent)
//! and [`journal_and_emit_named`]. `emit_scoped_or_unscoped` stamps
//! `timestamp_ms`, consults the chat fan-out registry, stamps `chat_turn_id`,
//! and emits `1 + n` envelopes; a record holding the finished `AgentEvent`
//! replays one. **THE RECORD IS BUILT; THE DELIVERY END IS NOT.**
//! `JournalBody::NamedEvent` holds the call and `journal_and_emit_named`
//! produces it, so the producing half is complete.
//!
//! **This sentence read "BUILT 2026-08-28 ... and
//! `driver_worker::WorkerHost::emit_projected_named_event` replays it through
//! the same entry point" until 2026-08-28, and the third clause was false when
//! it was written.** That method exists only as the trait's REFUSING default.
//! Every definition of it in the tree is either that default or the
//! `#[cfg(test)]` `FakeHost`'s; the only caller is `HostEventSink::emit_named`.
//! **No production host implements it.** (Stated as a population rather than
//! as a grep count, for the reason *THE LAST SWITCH* gives: a number written
//! here counts this paragraph's own mentions and makes a correct tree read as
//! drifted.) `executor.rs::InProcessWorkerHost` overrides
//! `emit_projected_event` and NOT this one, which is exactly the
//! half-implemented pair `WorkerHost::emit_projected_named_event`'s own doc
//! calls out as the case "worth naming, because it compiles".
//!
//! Two comments in this module say the true thing and this one did not, which
//! is why it is corrected rather than deleted: *THE LAST SWITCH, second rail*
//! is hedged as "a host that IMPLEMENTS it puts the record back through
//! `emit_named`", and the trait doc states the hazard outright. This header
//! was the one that read as done.
//!
//! What it costs if believed: flipping `emits_projected_events` on
//! `InProcessWorkerHost` without adding the named override stalls
//! `ProjectorCursor::project` at the first `JournalBody::NamedEvent` with the
//! mark BELOW it and an unbounded retry, so every record above that one is
//! buried for the life of the run. That is not a rare record —
//! `tool.result.projected` fires on the first projected autonomous tool
//! result, and `plan.step.started` / `plan.step.finished` from the second
//! iteration on. The failure mode is a TRUNCATED timeline, not the doubled one
//! the switch comment warns about, and its only signal is one
//! `[LOOP_OUTBOX]` warn per boundary.
//!
//! ### The two `ArtifactV2EventType` sites: a FINDING, and they are left alone
//!
//! `capture_pack_action_artifacts_projection` and
//! `capture_pack_action_inline_result_artifact` (both `executor.rs`) call
//! `canonical_event_sink.emit(scope, ArtifactV2EventType::ArtifactCreated,
//! payload)`. That is the one row of the table that is genuinely not a
//! `RuntimeTransportEvent` — and it is also the one rail whose destination is
//! **already durable**, which is why it did not get a variant:
//!
//! - `RuntimeCanonicalEventSink::emit` (`artifact_v2/events.rs:17`) is
//!   documented as *"enqueue a transport-originated event for canonical
//!   persistence"*, and `FilesystemRuntimeEventSink`
//!   (`artifact_v2/service.rs:28377`) hands it to
//!   `CanonicalEventWriter::append` (`:27632`), which appends to the
//!   execution's `events.jsonl` under a task lock, with unterminated-tail repair
//!   and a commit-authority reconcile. That file **is** the persisted timeline.
//! - Neither site reaches a live transport with this event. The first has a
//!   *separate* `publish_agent_update` beside it for the operator feed; the
//!   second has nothing. So there is no ephemeral delivery for an outbox to
//!   rescue **on this rail**.
//!
//!   That neighbouring `publish_agent_update` is **census site 21, and it is
//!   NOT covered by this refusal** — it sits inside the same `if let` and
//!   answers the opposite way, because it is ephemeral and a crash loses it
//!   outright. Written here because the two lines are adjacent and one refusal
//!   read as covering both is the likeliest way this finding gets misapplied.
//! - **A projected replay would DUPLICATE rather than rescue.**
//!   `FilesystemEventWriter::append` dedupes exactly one class of event — those
//!   carrying an `app_recipe_event_id` (`service.rs:27641-27760`) — and these
//!   carry none, so a replayed `ArtifactCreated` is appended a second time with
//!   a fresh `event_id` for one artifact. The projector cannot tell "the enqueue
//!   never drained" from "it drained": both look like a committed record with an
//!   unmoved mark, and the second is overwhelmingly the likelier, because the
//!   append happens within milliseconds and the crash window is the rest of the
//!   phase.
//!
//! **This is a finding, not a deferral.** Journalling this rail is work that
//! makes the timeline worse. What it would take to change the answer is
//! idempotency on the canonical append — a deterministic `event_id` for these
//! two, the way the recipe path already has one — and that is a change in
//! `artifact_v2`, not here.
//!
//! ## The one line that would have journalled the `emit_event` rail
//!
//! Every `RuntimeTransportEvent` site in `executor.rs` that goes through
//! `ActionExecutors::emit_event` funnels through that one function, so one
//! journal call inside it would have covered all of them at once. **That is
//! not what was built**, and this section is kept because the reason it was
//! not is the reason the remaining sites are shaped the way they are.
//!
//! What blocks the one line is not plumbing: `emit_event` knows neither
//! `iteration` nor `phase`, so the record it would build has no address, and
//! the next two sections are why it cannot simply be told. What was built
//! instead is `journal_and_emit_at` with a threaded `PhaseAddress` — twenty-
//! nine call sites, each of which had to be shown to be inside a phase body.
//!
//! **And the one line would not have been a superset.** It covers only the
//! sites that reach a transport *through `emit_event`*. Fourteen of the
//! census's twenty-one unjournalled sites do not: they reach a broadcaster
//! directly, through a canonical sink, or through a free function in another
//! module. A reader treating "one call in `emit_event`" as the whole remaining
//! job would be sizing two thirds of it.
//!
//! ## AN AMBIENT ADDRESS CANNOT WORK HERE, AND THE REASON IS CHECKABLE
//!
//! The obvious way to give `emit_event` an address is an ambient one: the phase
//! sets `(iteration, phase)` on entry, `emit_event` reads it. A `thread_local!`
//! is wrong on sight — a future is polled on whatever thread the runtime picks.
//! A `tokio::task_local!` is the right primitive for a future and is **still
//! wrong here**:
//!
//! - `execute_direct_path_on_scheduler_root` moves the
//!   whole direct path into `SchedulerRootLane::schedule`,
//!   and that lane's worker runs each job through
//!   `spawn_with_execution_token_meter_in_set` into a `JoinSet`
//!   (the first three in `executor.rs`; the spawn helper itself is in
//!   `types.rs`). That is a `tokio::spawn`, and no task-local crosses
//!   one. Everything under it is on the far side, including
//!   `execute_direct_path_post_setup_tail`'s `AgenticActionExecuted`
//!   — the per-action event the whole timeline hangs off.
//! - `decide` does the same for the decision call: `schedule_agentic_decision_job`
//!   goes to the same lane (`decide.rs:350`) carrying the
//!   `OperationLlmTelemetryContext` built at `decide.rs:250`.
//!
//! What *does* cross a spawn is the thing this module already keys on — the
//! execution id, because `AgenticContext` is cloned into the job. That is not the
//! compromise *Where this buffer really belongs* below makes it sound like; on
//! this path it is the only address that survives the hop. Whatever supplies
//! `iteration` and `phase` has to travel the same way, on the context or on the
//! executors, never in an ambient slot.
//!
//! **This refuses a task-local set on the CALLER's side of the spawn, and it is
//! not a refusal of `DISPATCH_LANE`.** That one is established on the FAR
//! side — [`in_dispatch_lane`] wraps the job future the scheduler-root lane's
//! worker spawns — so it never has to cross anything. The distinction is stated
//! because this section reads as a blanket ban on `task_local!` in this module,
//! and acting on it that way would delete the fix for *AND THE ORDER OFF THAT
//! LANE IS NOT REPRODUCIBLE*.
//!
//! ## ~~AND THE ORDER OFF THAT LANE IS NOT REPRODUCIBLE~~ — REPAIRED 2026-08-29
//!
//! Kept, struck through, because the mechanism it describes is still the
//! mechanism and the repair is only meaningful next to it. What changed is the
//! last paragraph.
//!
//! An event's ordinal is its **position among the records the phase produced**
//! (`journal::PHASE_COMPLETION_ORDINAL`), and `ProjectorCursor` dedupes on it, so
//! a producer owes a deterministic emission order.
//!
//! `apply` dispatches its parallel follow-up slice as `join_all` over jobs that
//! each go through the scheduler-root lane. Every member emits at least one
//! `AgenticActionExecuted`, and the order those reach a shared buffer is
//! completion order, which is not stable across a re-run. Unrepaired, that
//! addresses the same event `(i, p, 3)` on one attempt and `(i, p, 5)` on the
//! next: the projector re-emits the one that misses its dedupe window and
//! **silently drops** the one that collides, reporting `deduped: 1, emitted: 1`
//! — indistinguishable from a correct run. That is the failure
//! `JournalAppend::event`'s docs describe for `ends_run`, arriving from a third
//! source, so a fix covering only `ends_run` and the owner-transition push does
//! not cover it.
//!
//! **AND SINCE THE INLINE EMIT WENT, IT IS A LIVE DOUBLE EMISSION.** While
//! events were also emitted inline this buffer was a recorder and a dedupe miss
//! cost a duplicate *record*. `ProjectorCursor::project` is now the only path
//! from a record to a transport, so the pair that misses the window is emitted
//! twice onto chat activity, the deep-work panel and every `/debug` timeline,
//! and the pair that collides is not emitted at all. That is what moved this
//! from a tracked gap to a repair.
//!
//! **The repair is the second of the two candidates this section used to list:
//! a member-stable sort in the drain.** [`DispatchLane`] is opened once per
//! dispatch by `executor.rs::execute_direct_path_on_scheduler_root`, before that
//! dispatch's first `.await`; every record produced under it carries it; and
//! [`sort_dispatch_lanes`] turns completion order back into open order when
//! [`take`] hands the batch over. Open order is stable across attempts because
//! it is not a function of timing — see [`DispatchLane`] for why, which is the
//! part a "deterministic within one attempt" sort would have got wrong.
//!
//! The first candidate — per-member sub-addresses — was NOT taken, and the
//! reason is worth having: an ordinal is a position in `PhaseReport::records`,
//! so a sub-address means widening the address `commit_boundary` stamps and the
//! tuple `ProjectorCursor` dedupes on. That is a change to the journal's record
//! shape and to every reader of it, to express something the batch's own order
//! already says once the race is out of it.
//!
//! **What the sort therefore does NOT repair, so the strike-through above is
//! not read as more than it is:** the batch's own order says it only while the
//! batch has the same MEMBERS on both attempts, and [`store_in`]'s byte cap
//! decides membership by arrival — which is the same race. See the *A stable
//! ordinal* bullet under *What is owed, and by whom* for the four-member
//! trigger. A member-stable sub-address is the only shape that survives a lost
//! member; this repair covers order and nothing more.
//!
//! ## The sites, by where they are reached from
//!
//! **This section used to be a list of every site with a line number, and it
//! is deliberately no longer one.** It carried roughly forty `executor.rs`
//! line references; every one of them had drifted by the time anyone read it,
//! several by thousands of lines, and two carried claims that were false by
//! then — that `emit_agentic_tool_consumption` "has no `event_type`/`data`
//! split to journal at all" (it does; it is journalled), and that the sweep of
//! `executor.rs` sees every emission a phase can cause (it does not; see
//! *METHOD*, blind spot 5).
//!
//! A line number in a fifty-thousand-line file is a claim with a very short
//! half-life. What is durable is the SHAPE, so that is what is kept:
//!
//! - **Called directly by a phase.** Ten `executor.rs` helpers, all now
//!   journalled through `journal_and_emit_at` or `journal_and_emit_named_at`
//!   with an address the phase passes. `emit_step_events_if_signaled` is the
//!   largest at eight emissions on its own — four transport, four named.
//! - **Reached through another `executor.rs` function.** Mostly off
//!   `execute_direct_path_on_scheduler_root`, which `apply` drives at three
//!   sites (the first in-turn candidate, each parallel follow-up, each serial
//!   follow-up). `execute_direct_path_post_setup_tail` is the important one:
//!   it holds an `at`, and `AgenticActionExecuted` — the per-action event the
//!   whole timeline hangs off — is journalled under it. It also calls
//!   `capture_pack_action_artifacts_projection`, which is where census sites
//!   8, 20 and 21 live, **without** passing its address.
//! - **Reached from the frame ABOVE the loop.** `execute_agent_cycle`, census
//!   sites 5–7. No phase reaches these and no phase ever will.
//! - **Not in `executor.rs` at all.** `analytics/operation_llm_telemetry.rs`
//!   (sites 10–12), `analytics/llm_tool_lineage.rs` (sites 15–19),
//!   `agents/agent_update_emitter.rs` (sites 20–21),
//!   `analytics/llm_trace_activation.rs` and
//!   `analytics/runtime_activity_layer.rs` (the off-stack pair, sites 13–14).
//!
//! The per-site detail — what each is, which phase reaches it, and what it is
//! owed — is in *THE TWENTY-ONE* above, keyed by function name rather than by
//! line, and the binding copy of each refusal is at the site itself.
//!
//! One thing worth keeping from the old text because it is still true and
//! still surprising: `phases/decide.rs` builds an
//! `OperationLlmTelemetryContext` from `executors.event_broadcaster.clone()`,
//! which makes it **the only unjournalled site whose construction happens in a
//! file `phases/` owns** — and it still cannot be converted here. The field is
//! a concrete `Arc<RuntimeTransportBroadcaster>`, not a trait object, so there
//! is nothing to substitute a journalling decorator for.
//!
//! ## Why not one of the twenty-one is convertible by editing `phases/`
//!
//! Every one of them builds its event value **inside** the helper. A producer
//! at the call site would have to rebuild it — the second event vocabulary
//! this design exists to refuse, which drifts the first time the helper's
//! payload changes.
//!
//! That is not a theory. Two conversions have now been made and **neither was
//! made by editing `phases/`**: the 2026-08-27 executor conversion and the
//! 2026-08-28 named-rail and `emit_agentic_tool_consumption` ones all threaded
//! a `PhaseAddress` down through `executor.rs`, and the last of them
//! additionally split a builder out of an emitter in `analytics/`. The only
//! `phases/` edit any of them needed was passing `Some((iteration, phase))` at
//! a call site.
//!
//! So the fix is per rail and belongs where the value is built. For what is
//! left, that means: a builder/emitter split plus an address in
//! `analytics/llm_tool_lineage.rs` and `agents/agent_update_emitter.rs` (the
//! seven owed a diff), and nothing at all for the fourteen that are refused.
//! **No phase-local change makes any of it convertible here** — which is why
//! this section is a report and not a diff.
//!
//! # No second event vocabulary
//!
//! `RuntimeTransportEvent` is `#[serde(tag = "event_type", content = "data")]`
//! and derives `Deserialize`, and `JournalBody::Event`'s `event_type` and
//! `payload` are a byte-exact split of that encoding. So [`split`] is `to_value`
//! plus two map removals and [`rejoin`] is the inverse — no mirror enum, nothing
//! to keep in step, and a projector's sink gets back the same value the transport
//! would have carried. Every variant of `RuntimeTransportEvent` is a struct
//! variant (counted 2026-08-27: 107 of them, zero unit and zero tuple), so `data`
//! is always present and the split is total.
//!
//! **The record's third field is not a second vocabulary either**, and it is worth
//! saying because it is the one thing in the body that is not the event's own
//! bytes. `RecordedEventRouting` does not describe the event; it describes what
//! the producer DID with it, which is information the event does not contain and
//! `split` therefore cannot produce. It is the answer to a question, not a second
//! spelling of the question — and 54 of the 107 variants can take either answer,
//! so no enrichment of the event vocabulary could replace it. [`rejoin`] stays the
//! exact inverse of [`split`] and is deliberately unaware of it.
//!
//! # What is owed, and by whom
//!
//! - ~~**The drain**, in `executor.rs::InProcessWorkerHost::run_phase`~~ —
//!   **LANDED 2026-08-27**, and in the right place: it is the
//!   `report.records.extend(phases::outbox::take(execution_id));` immediately
//!   before that function's `report.records.push(transition)`. Struck through
//!   rather than deleted, because the paragraph under it is *why* the line is
//!   where it is, and an edit that moves it re-opens the failure.
//!
//!   *Before*, not "beside". `commit_boundary` stamps ordinals purely by index
//!   in `report.records` (`driver_worker.rs:1295-1308`), and the owner-transition
//!   record is pushed only on the attempts where a handover actually happened.
//!   Extending after the push therefore puts the transition at index 0 and
//!   shifts every event's ordinal by one — but only on those attempts — so the
//!   same event is addressed `(i, p, k)` on an attempt with no transition and
//!   `(i, p, k+1)` on a re-attempt that had one. That is the same instability
//!   `ends_run` causes below, from a second source, and it defeats the
//!   projector's dedupe the same way.
//! - **A drain on the resident arm, or the end of that arm.**
//!   `driver_inproc::run_iteration` assembles no `PhaseReport`, so the drain
//!   above is unreachable from it. Until one of those two happens,
//!   [`journalling_has_a_drain`] keeps this module inert there rather than
//!   letting it buffer for nobody — which means the records a flip-gate
//!   differential would want to compare across the two arms **do not exist on
//!   the resident one**. That is a real limit on what such a comparison can say,
//!   and it is a limit of the arm rather than of the gate: before the gate those
//!   records existed there and were thrown away unread.
//! - **A stable ordinal**, in `driver_worker::commit_boundary`. It computes
//!   `ordinal: if ends_run { index } else { index + 1 }`, and `ends_run` is a
//!   property of how the phase happened to end on *that attempt*, not of the
//!   producer's emission order. So one event can be addressed `(i, p, 0)` on an
//!   attempt that paused and `(i, p, 1)` on the attempt that continued, which
//!   defeats the projector's dedupe in both directions — a re-emit for the pair
//!   that misses, a silent drop for the pair that collides. See
//!   `JournalAppend::event`'s own docs for the full failure. **This producer
//!   cannot fix it** and must not try: it does not choose the ordinal, because
//!   the driver re-stamps whatever it is handed. The fix has to cover the
//!   owner-transition case above too: an ordinal derived from a record's
//!   position among the *host's own* records, independent of where either the
//!   completion record or the transition sits.
//!
//!   **And that is still not the whole fix — though this half of it is now
//!   closed.** A position-derived ordinal presumes the producer hands over a
//!   deterministic order. That presumption failed the moment the scheduler-root
//!   lane's emissions were journalled, because `apply`'s parallel follow-up
//!   slice buffers in completion order. It holds again as of 2026-08-29:
//!   [`sort_dispatch_lanes`] runs inside [`take`], so the order this producer
//!   hands over is [`DispatchLane`] order and not a race. See *AND THE ORDER OFF
//!   THAT LANE IS NOT REPRODUCIBLE*. **`ends_run` is untouched by it** — that is
//!   a driver-side ordinal choice, this is a producer-side order, and closing
//!   one does not close the other.
//!
//!   **And ORDER is not MEMBERSHIP, which is the half the sort does not
//!   reach.** [`store_in`]'s byte cap decides which record is refused by
//!   `run.bytes + record_bytes > MAX_PENDING_BYTES_PER_RUN` *at the moment it
//!   arrives* — that is, by completion order, the same race
//!   [`sort_dispatch_lanes`] undoes for ordering. [`MAX_PENDING_BYTES_PER_RUN`]
//!   is four times [`MAX_JOURNAL_RECORD_BYTES`], so a parallel slice of four
//!   members each emitting a large `AgenticActionExecuted` fills one run's
//!   buffer inside a single `Apply`, and WHICH member loses its record is not
//!   stable across attempts. Attempt 1 can refuse member 3 and hand back
//!   `[m0, m1, m2]`; attempt 2 can refuse member 2 and hand back
//!   `[m0, m1, m3]`.
//!
//!   **The former silent collision is closed by content-addressed event keys.**
//!   The projector fingerprints the logical body (with producer timestamps
//!   removed) plus its occurrence within the batch, so `m3` no longer inherits
//!   `m2`'s positional identity and cannot be falsely suppressed. What remains
//!   is the ordinary cost of inline fallback: a member refused on one attempt
//!   and accepted on another may be delivered once inline and once through the
//!   outbox after a crash. That is at-least-once duplication, visible rather
//!   than loss; eliminating it would require a durable pre-admission decision,
//!   not another positional ordinal rule.
//!   `a_reattempt_addresses_a_parallel_members_event_at_the_same_ordinal`
//!   asserts `Stored::Kept` for every fixture record precisely to keep this case
//!   out of it, so nothing in the tree covers the open half.
//! - ~~**A sink**, for `ProjectorCursor::project`~~ — **BUILT AND ACTIVE**:
//!   `driver_worker::HostEventSink` wraps the host and calls [`rejoin`] per
//!   record. `executor.rs::InProcessWorkerHost` opts into projected events;
//!   accepted records are projector-only, while refused records and the
//!   no-drain in-process rollback arm retain inline delivery.
//! - ~~**The canonical-versus-transport decision, on the record**~~ —
//!   **BUILT 2026-08-27**: `JournalBody::Event` carries a
//!   `RecordedEventRouting`, [`routing_for`] writes it, and
//!   `ProjectorCursor::project` hands it to the sink. See *THE RECORD NOW
//!   CARRIES THAT DECISION* above for the trade this took against the cheaper
//!   answer.
//! - ~~**A sink that READS the routing**, in `driver_worker::HostEventSink`, and
//!   a host that acts on it, in `executor.rs::InProcessWorkerHost`~~ —
//!   **BOTH BUILT 2026-08-28. This is condition 2, and it is MET.**
//!   `HostEventSink::emit_routed` overrides the trait default that dropped the
//!   routing, and `InProcessWorkerHost::emit_projected_event` honours all three
//!   `RecordedEventRouting` arms — `CanonicalRuntimeFact` through a scope
//!   rebuilt from the RUN's execution id (never the journal's file key, which
//!   is a loop-state address), `TransportOnly` through the broadcaster, and
//!   `Unrecorded` through this process's live rule, logged rather than taken
//!   silently.
//!
//!   Kept struck through because this bullet said "still open" while both
//!   overrides were already in the tree, and so did two other sections of this
//!   file. That is how a nearly-met gate gets budgeted as far from met, and it
//!   is the same failure the census numbers had.
//! - **One implementation of the predicate**, in `ActionExecutors::emit_event`.
//!   It computes `should_emit_runtime_fact` inline; [`routing_for`]
//!   computes the same thing from the same fields. Two copies of a rule whose
//!   entire value is that it agrees with what was emitted. `emit_event` calling
//!   [`routing_for`] and branching on its answer collapses them, and that edit is
//!   in `executor.rs`.
//! - ~~**A decision on the nine sites that are not `RuntimeTransportEvent`s**~~
//!   — **ANSWERED 2026-08-28, and the premise was wrong for seven of the nine.**
//!   See *FOUR VOCABULARIES* above. What came out of it: the five `emit_named`
//!   sites are BUILT on `JournalBody::NamedEvent` (for the fan-out, not for the
//!   tag); the `AgentEventEnvelope` sites need no variant at all and were owed
//!   only an address; the two `ArtifactV2EventType` sites are deliberately NOT
//!   journalled and the reason is written up there and at both sites.
//! - ~~**An address for the two `AgentEventEnvelope` sites**~~ — **BOTH DONE.**
//!   `emit_agentic_lineage_mapping_gap` took one on 2026-08-27, threaded
//!   through `begin_agentic_tool_lineage`'s nine phase callers;
//!   `emit_agentic_tool_consumption` took one on 2026-08-28, which also
//!   required splitting `analytics::llm_tool_lineage::emit_tool_lineage_record`
//!   into `tool_lineage_envelope` (builds) and `emit_tool_lineage_record`
//!   (builds and sends), so the envelope is produced once rather than rebuilt
//!   at the call site.
//! - **THE SAME TREATMENT FOR SEVEN MORE SITES**, found on 2026-08-28 and
//!   listed as *OWED A DIFF* in the census above. This is the bullet that
//!   replaced the one struck through directly above it, and the replacement is
//!   larger than the original — which is the honest state of this gate rather
//!   than a setback:
//!   - **`analytics/llm_tool_lineage.rs`**, five sites. Three want the
//!     six-line builder/emitter split that `tool_result_consumed_envelope`
//!     already demonstrates (`emit_tool_proposed`, `emit_tool_linkage_gap`,
//!     `emit_tool_branch_materialized`); `emit_tool_rollback` wants one that
//!     returns a pair; `emit_completed_tool_dispatch` wants one that
//!     accumulates across eight early-return branches and is the only member of
//!     the family that is not mechanical.
//!   - **`agents/agent_update_emitter.rs`**, two sites. `publish_agent_update`
//!     builds its envelope and sends it in one call, exactly as
//!     `emit_tool_lineage_record` did.
//!   - **An address on two `executor.rs` functions**:
//!     `finish_agentic_tool_lineage` takes no `PhaseAddress` at all (its nine
//!     callers are all phase bodies), and
//!     `capture_pack_action_artifacts_projection` takes none although its
//!     caller `execute_direct_path_post_setup_tail` already holds one.
//!
//!   All seven then take `journal_and_emit_at` with
//!   `RuntimeTransportEvent::AgentEvent { event }`, which delivers exactly what
//!   `emit_agent_transport_event` delivers today. No variant, no new rail.
//!
//! # What it costs per event
//!
//! **Two JSON encodes — three until 2026-08-28.** [`split`]'s `to_value`, and
//! `JournalAppend::event_measured`'s size probe. The third was a separate
//! `to_string` of the same body the probe had just encoded, done only to learn
//! how many bytes to charge the run's buffer; the probe now returns what it
//! measured and there is nothing left to recompute. Stated because it is a real
//! cost on a hot-ish path — roughly a dozen events an iteration — and because
//! the probe is the store's own ceiling check and stays even when
//! [`MAX_PENDING_BYTES_PER_RUN`] goes with the move to `RunControls` below.
//!
//! The charge is now the **line** at the widest stamps rather than the body
//! alone, so it is larger by a fixed envelope of about 130 bytes — 0.05% of a
//! 256 KiB per-run bound, and in the safe direction. See [`build`].
//!
//! **Plus one `map_v2_realtime_event`**, reached from [`routing_for`] through
//! [`canonical_runtime_fact_of`](crate::magician_v2::artifact_v2::canonical_runtime_fact_of)
//! rather than called directly. It is a match over
//! 107 variants that clones a payload out of the one it hits, so on a mapped
//! event it is a fourth encode in all but name — and it is a SECOND call for
//! every event, because `ActionExecutors::emit_event` runs the same mapping a
//! moment later for the same decision. Collapsing the two is the same
//! `executor.rs` edit that collapses the two copies of the predicate, so the
//! duplicated work and the duplicated rule go together or not at all.
//!
//! **On the in-process rollback arm the cost is one cached `bool` load and nothing else**
//! — an environment read only on the first journalled event of the process,
//! since 2026-08-28 — because [`journalling_has_a_drain`] returns before either
//! [`split`] or [`routing_for`] is reached. The encodes and the mapping are paid only where a
//! drain will take the result — which is why [`routing_for`] is called from
//! [`journal`] after the gate rather than from [`journal_and_emit`] before it.
//!
//! # What the NAMED rail costs per event, which is less
//!
//! One JSON encode and one payload clone, against the transport rail's two
//! encodes plus a `map_v2_realtime_event` walk:
//!
//! - **No [`split`]**, because the five arguments *are* the record. Nothing is
//!   encoded and taken apart, so the rail also has no "did this encode to the
//!   tag/content shape" failure — it can only be refused for size.
//! - **No [`routing_for`]**, so no second `map_v2_realtime_event`. The rail has
//!   one delivery branch; see [`JournalBody::NamedEvent`](super::super::journal::JournalBody::NamedEvent).
//! - **One payload clone**, in [`build_named`]. Unavoidable while inline
//!   fallback stays: the record needs an owned copy while the original remains
//!   available if admission is refused. It is `clone_json_iteratively`, so a deep payload cannot
//!   overflow the stack the way a recursive clone would — the same function
//!   `emit_scoped_or_unscoped` uses for its own fan-out copies.
//! - **Four short `String` allocations**, named so the bullet above is not read
//!   as "and nothing else": `name` and `agent_id` are `Into<String>` at
//!   `JournalAppend::named_event_measured`, and `principal` / `workspace` are
//!   `str::to_string`d in [`build_named`]. The record outlives the borrows the
//!   call site holds, so all four are structural.
//! - **The one encode** is `JournalAppend::named_event_measured`'s size probe,
//!   whose measurement is also the byte charge — exactly as on the other rail,
//!   and it was two here too until 2026-08-28.
//!
//! Both gates come first here too — the execution id, then
//! [`journalling_has_a_drain`] — so the in-process rollback arm does not pay the clone or
//! the encode.
//!
//! **One thing IS paid on every arm**, and it is named rather than folded into
//! "one cached `bool` load": [`stamp_emit_time`], before the gates. It has to be
//! before them, because the value it writes must be identical whether the event
//! is accepted for projection or takes the inline fallback.
//!
//! Its cost is **one map lookup, one 12-byte `String` allocation, and on the
//! ordinary path one insert** — not "a lookup and at most one insert", which is
//! what this paragraph said before and which the `to_string()` at the call site
//! contradicts. `serde_json::Map::entry` takes `S: Into<String>` and calls
//! `key.into()` eagerly, so the key is allocated whether or not it is then
//! inserted; there is no borrowing form of `entry` to reach for. A
//! `contains_key` guard would drop the allocation on the already-stamped path
//! and add a second lookup to the common one — and the common one is
//! already-absent, because every `emit_named` call site in `executor.rs` builds
//! its payload from a `json!` that carries no `timestamp_ms`. So the allocation
//! is paid, deliberately, and it is the whole production-visible cost of the
//! named rail on the `Inprocess` arm, where nothing is journalled and nothing is
//! projected.
//!
//! # Where this buffer really belongs
//!
//! On `RunControls` (`run_loop/controls.rs`), beside `shell_stream_ctx` and
//! `app_labeled_tool_results` — per-execution, process-local, deliberately not
//! boundary-carried, which is exactly this buffer's shape. It is a process-global
//! map keyed by execution id instead for one reason: `RunControls` is not this
//! change's to edit, and a field there would be only half the move anyway, since
//! the drain is in `executor.rs` either way. A global keyed by execution id is
//! the ambient bag `phases/mod.rs` argues against, and it is bounded here
//! precisely because nothing else bounds it. Moving it onto `RunControls` deletes
//! [`MAX_TRACKED_RUNS`] and the process-wide cap with it, because a run's buffer would
//! then die with the run.
//!
//! **One correction to that, from the sweep above.** Keying on the execution id
//! is not only an ownership compromise. `RunControls` is reached through
//! `ActionExecutors`, which the scheduler-root lane's jobs also hold, so that
//! move is sound — but the *ambient* variants of this idea are not, and somebody
//! reading only this section would reach for one. A `thread_local!` or a
//! `tokio::task_local!` carrying `(iteration, phase)` cannot cross the
//! `tokio::spawn` in `SchedulerRootLane`, and the largest emitter in the loop is
//! on the far side of it. See *AN AMBIENT ADDRESS CANNOT WORK HERE*.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::{LazyLock, Mutex, OnceLock};

use serde_json::Value;
use tracing::warn;

use crate::magician_v2::artifact_v2::{canonical_runtime_fact_of, CanonicalEventScope};
use crate::magician_v2::execution::agentic::executor::{
    loop_state_address, runtime_execution_id_opt, ActionExecutors,
};
use crate::magician_v2::execution::agentic::run_loop::journal::{
    JournalAppend, RecordedCanonicalScope, RecordedEventRouting, MAX_JOURNAL_RECORD_BYTES,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
use crate::magician_v2::execution::agentic::run_loop::{ExecutionDriver, EXECUTION_DRIVER_ENV};
use crate::magician_v2::execution::agentic::types::AgenticContext;
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use crate::magician_v2::RuntimeTransportEvent;

/// The most one run may hold in journalled-but-undrained records.
///
/// A **byte** bound and not a record count, because [`MAX_JOURNAL_RECORD_BYTES`]
/// is 64 KiB and a count of even thirty-two would put a run's worst case at two
/// megabytes without any observability payload having to be unusual. At this
/// bound a whole process holds at most
/// `MAX_TRACKED_RUNS × MAX_PENDING_BYTES_PER_RUN` = 16 MiB, which is a number an
/// operator can reason about.
///
/// It bounds an *undrained* buffer. With the drain in place a run holds one
/// phase's events — a few kilobytes — and this never binds.
///
/// # It was sized for nine producers, and forty-three feed it today
///
/// **The premise this section used to argue from has already happened.** The
/// original text said nine small control events fed the buffer and that the fill
/// rate would change when "the other forty-two" arrived. Twenty-seven of them
/// arrived later on 2026-08-27 through `executor.rs::journal_and_emit_at`, five
/// more on 2026-08-28 through the named rail and one more the same day, so the
/// count is forty-three and the fill rate is the post-conversion one. Re-counted
/// 2026-08-28; see *WHAT IS NOT JOURNALLED* for the derivation.
///
/// That matters for one specific reason: `AgenticActionExecuted`
/// (`executor.rs::execute_direct_path_post_setup_tail`) is **in** the
/// forty-three, and it is the biggest of them — it carries a tool result, and
/// one such event can be a sizeable fraction of [`MAX_JOURNAL_RECORD_BYTES`] on
/// its own. So the undrained window is no longer "a long time to 256 KiB"; a
/// single action-heavy iteration can approach the bound by itself.
///
/// # THE FORTY-THIRD IS THE ONE THAT SCALES WITH FAN-OUT, NOT ITERATIONS
///
/// `emit_agentic_tool_consumption`, journalled 2026-08-28, is the first
/// producer whose contribution is **not one record per call**. It emits one
/// `llm.tool.lineage` result-consumed envelope per queued lineage — one per
/// tool call the turn's decision consumed — so a turn that dispatched a dozen
/// tools contributes a dozen records from a single site. The payloads are
/// small (a content-free fact row: identifiers, counters and enum tags), so
/// this is a count effect rather than a byte effect, and it does not move this
/// constant. It is written down because "forty-three producers" invites the
/// arithmetic "forty-three records an iteration", and that arithmetic is now
/// wrong in the unsafe direction.
///
/// # The twenty-one outside do not charge this buffer, whatever their size
///
/// None of them is large, and none would charge this buffer even if it were:
/// every one either has no phase to be buffered under, writes somewhere else
/// entirely, or is not journalled yet. Seven of the twenty-one are *owed a
/// diff* rather than refused, so this section's arithmetic changes again when
/// they land — and two of those seven are also fan-out shaped
/// (`emit_completed_tool_dispatch` emits one to seven records per call,
/// `emit_tool_rollback` two).
///
/// The five named producers are the small-control-event kind and were never the
/// pressure: a `plan.step.finished` payload is six short fields and a
/// `tool.result.projected` payload is a dozen counters and two URLs.
///
/// **This constant does not need to move, and moving it would be the wrong
/// answer.** The behaviour under pressure is already correct — the new record is
/// refused, the held ones survive, and both losses are counted for [`take`] to
/// report. What changes is that the once-per-run "buffer is full" warning stops
/// being an anomaly and becomes the ordinary state of any run that outlives its
/// drain, so anyone reading it as a bug report will be reading noise. Raising
/// the bound buys a longer undrained window and costs the 16 MiB process figure
/// this module sells; the actual fix is the drain, which makes the window one
/// phase long and the number irrelevant.
const MAX_PENDING_BYTES_PER_RUN: usize = 256 * 1024;

// A run's buffer must hold at least one record of the widest size the store
// will accept, and this is a compile error rather than a comment because the
// failure it prevents is silent and the two constants live in different files.
//
// Drop [`MAX_PENDING_BYTES_PER_RUN`] below
// [`MAX_JOURNAL_RECORD_BYTES`](super::super::journal::MAX_JOURNAL_RECORD_BYTES)
// and a legal record — one `JournalAppend::event_measured` accepted — can never
// be stored: `store_in`'s `run.bytes + record_bytes > MAX_PENDING_BYTES_PER_RUN`
// is true on an empty buffer, so every event of every run is discarded and the
// only trace is one "buffer is full" warning per run, which points at the
// *pressure* and not at the ceiling. Four times the record ceiling today.
const _: () = assert!(
    MAX_PENDING_BYTES_PER_RUN >= MAX_JOURNAL_RECORD_BYTES,
    "a run's outbox buffer must hold at least one record of the widest size the journal accepts, \
     or store_in discards every event on an empty buffer"
);

/// How many executions may hold pending records at once.
///
/// The cap bounds process memory at
/// `MAX_TRACKED_RUNS × MAX_PENDING_BYTES_PER_RUN` (16 MiB today). Once it is
/// occupied, a new run's event is refused before admission and takes the
/// producer's inline fallback. Records already accepted for another run are
/// never removed to make space: doing so would make a hole in that run's
/// durable timeline, while refusing the newcomer preserves both runs'
/// one-delivery-path rule.
/// Existing entries disappear at their phase drain, so capacity returns without
/// sacrificing accepted work.
const MAX_TRACKED_RUNS: usize = 64;

/// Which dispatch produced a record, for the one reordering this buffer is
/// allowed to do.
///
/// # THE ORDER OFF THE SCHEDULER-ROOT LANE IS COMPLETION ORDER, AND THIS IS THE KEY THAT UNDOES IT
///
/// `apply` dispatches its parallel follow-up slice as a `join_all` over jobs
/// that each go through `executor.rs::execute_direct_path_on_scheduler_root`.
/// Every member emits at least one `AgenticActionExecuted`, and the order those
/// reach this buffer is the order the members FINISHED — real I/O timing, which
/// no re-attempt reproduces. `driver_worker::commit_boundary` then stamps
/// `ordinal: index` over the drained list, so the same event was addressed
/// `(i, Apply, 3)` on one attempt and `(i, Apply, 5)` on the next, and
/// `ProjectorCursor::project` — which since the inline emit was cut is the ONLY
/// path from a record to a transport — re-emits the pair that misses its dedupe
/// window and silently drops the pair that collides. A live double emission,
/// not a duplicated log line.
///
/// A lane is opened once per dispatch, **before** that dispatch's first
/// `.await`, and every record the dispatch produces carries it. [`take`] sorts
/// by it, which turns completion order back into open order.
///
/// # WHY OPEN ORDER IS STABLE ACROSS ATTEMPTS, WHICH IS THE ONLY PROPERTY THAT MATTERS
///
/// A sort that is merely deterministic *within* one attempt fixes nothing: the
/// failure is a re-attempt disagreeing with the first attempt. Open order is
/// stable because it is not a function of timing at all:
///
/// - `execute_direct_path_on_scheduler_root` opens its lane synchronously, with
///   no `.await` between entering the function's body and the `schedule` call.
///   So a member's lane is taken on its FIRST POLL, before it can yield.
/// - `join_all` polls its input in order on that first pass, and its input is
///   `apply`'s `spawned` vector — the members the plan said to fire, in
///   ascending member index. So the slice's lanes are opened in member order,
///   by construction rather than by luck.
/// - The primary candidate and each sequential follow-up go through the same
///   function, one at a time, so their lanes ascend in dispatch order too. A
///   sort by lane therefore leaves the serial path in exactly the order it
///   produced, which is what makes this safe to apply to every dispatch instead
///   of only to the parallel slice.
///
/// The counter is process-global and monotone, so the lane VALUES differ between
/// two attempts. Nothing reads a value; [`take`] only compares them, and the
/// comparison is what has to agree.
///
/// # ONE CAVEAT, STATED BECAUSE THE SECOND BULLET IS ONLY TRUE UP TO A SIZE
///
/// `futures_util::future::join_all` polls its input in order **for up to
/// `SMALL` = 30 futures** (`JoinAllKind::Small`, which walks the slice). Above
/// that it collects into a `FuturesOrdered`, and the order that structure first
/// polls its members in is an internal detail of `FuturesUnordered` rather than
/// input order.
///
/// That bounds the SECOND bullet, not the property this exists for. Nothing has
/// been polled yet at that point, so there are no wakeups and no timing input:
/// whatever order `FuturesOrdered` chooses, it chooses the same one on a
/// re-attempt over the same input. So a slice of more than thirty members is
/// still addressed identically on both attempts — the dedupe still holds — and
/// what is lost is only that the timeline reads in an order other than member
/// order. Writing this down rather than leaving the bullet to be read as
/// unconditional, because a later reader sizing a change against "member order
/// is guaranteed" would be sizing something this does not promise.
///
/// # WHAT IT IS NOT
///
/// Not an address, and deliberately never written into a [`JournalAppend`]. The
/// address is still `(iteration, phase, ordinal)` and the ordinal is still the
/// position `commit_boundary` stamps. This only decides what that position is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::magician_v2::execution::agentic) struct DispatchLane(u64);

/// Hands out [`DispatchLane`]s. Monotone and never reset — see *WHY OPEN ORDER
/// IS STABLE*: only the comparison between two lanes of one drain is read.
static NEXT_DISPATCH_LANE: AtomicU64 = AtomicU64::new(0);

tokio::task_local! {
    /// The lane every record produced under one dispatch is tagged with.
    ///
    /// A `tokio::task_local!` here is NOT the ambient address this module's
    /// *AN AMBIENT ADDRESS CANNOT WORK HERE* rules out, and the distinction is
    /// the whole reason this compiles into a fix. That section refuses a
    /// task-local set on the CALLER's side of `SchedulerRootLane::schedule`,
    /// because no task-local crosses the `tokio::spawn` the lane's worker makes.
    /// This one is set on the FAR side: [`in_dispatch_lane`] wraps the job
    /// future itself, so the scope is established inside the spawned task and
    /// every emission under it — including
    /// `execute_direct_path_post_setup_tail`'s `AgenticActionExecuted` — reads
    /// it. Concurrent members are separate tasks, so they cannot see each
    /// other's.
    static DISPATCH_LANE: DispatchLane;
}

/// Take the next lane. Called once per dispatch, before that dispatch awaits.
pub(in crate::magician_v2::execution::agentic) fn open_dispatch_lane() -> DispatchLane {
    DispatchLane(NEXT_DISPATCH_LANE.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Run one dispatch's job future inside its lane.
///
/// Takes and returns the boxed future the scheduler-root lane's factory already
/// deals in, so the call site is a wrap rather than a signature change.
pub(in crate::magician_v2::execution::agentic) fn in_dispatch_lane<T>(
    lane: DispatchLane,
    future: Pin<Box<dyn Future<Output = T> + Send>>,
) -> Pin<Box<dyn Future<Output = T> + Send>>
where
    T: Send + 'static,
{
    Box::pin(DISPATCH_LANE.scope(lane, future))
}

/// The lane the calling task is inside, if any.
///
/// `None` for everything a phase emits on its own stack, which is most of what
/// this buffer holds, and `None` is what keeps those records where they are:
/// [`sort_dispatch_lanes`] never moves an unlaned record.
fn current_dispatch_lane() -> Option<DispatchLane> {
    DISPATCH_LANE.try_with(|lane| *lane).ok()
}

/// One buffered record and the dispatch that produced it.
#[derive(Debug)]
struct PendingRecord {
    /// `None` when the record was produced on the phase's own stack rather than
    /// inside a dispatch. See [`DispatchLane`].
    lane: Option<DispatchLane>,
    append: JournalAppend,
}

impl From<JournalAppend> for PendingRecord {
    /// An unlaned record — what a fixture that is not about ordering produces.
    fn from(append: JournalAppend) -> Self {
        Self { lane: None, append }
    }
}

/// One run's undrained records, plus what it has already lost.
#[derive(Debug, Default)]
struct RunOutbox {
    records: Vec<PendingRecord>,
    /// Encoded size of the bodies in `records`, against
    /// [`MAX_PENDING_BYTES_PER_RUN`].
    bytes: usize,
    /// Records this run produced and could not keep **because its buffer was
    /// full**. Drives the once-per-run warning in [`journal_for`], which is why
    /// it counts one kind of loss and not both.
    dropped: usize,
    /// Events this run produced that never became a record at all: the
    /// producer's own size refusal, or an event that did not encode to the
    /// tag/content shape.
    ///
    /// A separate counter rather than more `dropped`, for two reasons. Each now
    /// gates its own warning on `== 1` — the two are independent once-per-run
    /// firings — so a shared counter would let whichever loss happened first
    /// consume the other's single line, and the run would report only one of two
    /// different failures. And [`take`] has to report both, because **the two losses
    /// are the same loss to a projector**: a drain that is short is short
    /// whichever bound refused the record, and before this counter existed a run
    /// that lost ten oversized events handed the drain a short list with nothing
    /// saying it was short, while a run that lost one event to a full buffer did.
    refused: usize,
}

#[derive(Debug, Default)]
struct Pending {
    runs: HashMap<String, RunOutbox>,
    /// Events refused before admission because every tracked slot was occupied.
    /// Used only for logarithmic warning cadence; refusal never removes an
    /// accepted record.
    capacity_refused: u64,
}

static PENDING: LazyLock<Mutex<Pending>> = LazyLock::new(|| Mutex::new(Pending::default()));

/// What one call to [`store`] did, carried out of the lock so no warning is
/// logged while the non-reentrant `PENDING` mutex is held.
#[derive(Debug)]
enum Stored {
    Kept,
    /// The process-wide run cap is occupied by other undrained phases. No
    /// accepted record was removed; this event takes the caller's inline
    /// fallback instead.
    CapacityRefused {
        refused: u64,
    },
    /// The run's buffer was full. `dropped` is the running total for the run.
    Discarded {
        dropped: usize,
        held_bytes: usize,
    },
    /// The event did not encode to the tag/content shape, or was over the record
    /// ceiling. Nothing reached the buffer; it is counted against the run's
    /// `refused` so [`take`] can tell the drain its list is short.
    ///
    /// `refused` is that running total, not a flag: [`journal_for`] warns on the
    /// first refusal of a run and stays quiet after it, which needs the count and
    /// not merely the fact. Carried out of the lock for the same reason the whole
    /// enum is — no `warn!` in this module may run under the `PENDING` guard.
    NotJournalable {
        reason: String,
        refused: usize,
    },
}

/// Journal the event when a projector can deliver it; otherwise emit inline.
///
/// The journal result, not merely the selected driver, decides the path. That
/// preserves delivery for an unaddressed, oversized, or capacity-refused event
/// without duplicating an accepted record. The in-process arm always takes the
/// inline branch because [`journalling_has_a_drain`] prevents it from buffering
/// records nobody can consume.
///
/// # Refusing an oversized event, which is the producer's question to answer
///
/// `JournalAppend::event` refuses a body that would push a record over
/// [`MAX_JOURNAL_RECORD_BYTES`], because the store's ceiling applies to the whole
/// batch: an oversized observability payload that reached the store would fail
/// the append, fail the commit, refuse the boundary and **stall a live run**.
///
/// This producer answers that refusal by **dropping the record** and counting it.
/// The two alternatives are worse:
///
/// - *Truncating the payload* produces a record that no longer deserializes back
///   into a `RuntimeTransportEvent`, so a sink could never emit it. Per
///   `EmitRefused`'s contract a sink that cannot map a record must take it and
///   drop it on its own side, so a truncated record buys a hole in the log in
///   exchange for nothing — and a sink that answered `Err` instead would stall
///   the projector's walk permanently, since the same record is refused on every
///   retry.
/// - *Propagating the error* is the stall this refusal exists to prevent.
///
/// Dropping the record is harmless only because the event takes this function's
/// inline fallback when admission fails. Removing that fallback would either
/// require a separately resolvable payload or make oversized events disappear.
pub(in crate::magician_v2::execution::agentic) fn journal_and_emit(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    phase: Phase,
    event: RuntimeTransportEvent,
) {
    let journalled = journal(
        ctx,
        // The one field of `ActionExecutors` the record needs, rather than
        // the whole struct: `journal` does not emit, and a parameter that
        // could reach a transport would invite one that did.
        executors.canonical_event_scope.as_ref(),
        iteration,
        phase,
        &event,
    );
    if !journalled {
        executors.emit_event(event);
    }
    // ┌──────────────────────────────────────────────────────────────────────┐
    // │ Accepted records are projector-only. Inline delivery is the fallback │
    // │ for a record that was not accepted, including every event on the      │
    // │ in-process arm. Never emit an accepted record here: that duplicates   │
    // │ the projector's delivery.                                             │
    // └──────────────────────────────────────────────────────────────────────┘
    //
    // This box used to read "DO NOT DELETE IT YET", and the three conditions
    // below used to be introduced as "None of them is true today". Both were
    // true when written. When the switch landed, the evidence at the FOOT of
    // this block was amended and this verdict at its HEAD was not — so for a
    // while the block told a reader who stopped here the opposite of what it
    // told a reader who finished it. That is why the conditions are kept rather
    // than deleted: they are the record of what had to hold, and each one is
    // still checkable.
    //
    // The three conditions, ALL MET as of 2026-08-28:
    //
    // 1. SOMETHING DRAINS THE BUFFER — **on the stateless arm only, since
    //    2026-08-27**. `executor.rs::InProcessWorkerHost::run_phase` calls
    //    [`take`] once per phase, and that host is built only by the stateless
    //    arm. `driver_inproc::run_iteration` assembles no `PhaseReport` and
    //    reaches this module nowhere, so on the IN-PROCESS ROLLBACK arm there is no drain —
    //    which is why [`journalling_has_a_drain`] stops this module storing
    //    anything there at all. Removing the inline fallback would blank chat
    //    activity, the deep-work panel and `/debug` timelines on that arm.
    //    Check: `driver_inproc.rs` mentions `outbox`, or that arm is gone.
    //
    // 2. THE SINK CAN RE-DERIVE WHAT `emit_event` DECIDES — **MET, since
    //    2026-08-28.** `emit_event` chooses per event between a canonical
    //    runtime fact and a transport-only send, from `canonical_event_scope`,
    //    `canonical_event_sink` and `event_broadcaster`.
    //
    //    The RECORD now answers it: `JournalBody::Event` carries a
    //    `RecordedEventRouting`, [`routing_for`] computes it at production time,
    //    and `ProjectorCursor::project` hands it to
    //    the sink through `ProjectedEventSink::emit_routed`.
    //
    //    **It is not computed from those same three fields**, and this comment
    //    said it was until 2026-08-28. [`journal`] hands [`routing_for`] exactly
    //    ONE of them — `canonical_event_scope` — plus the event and the run's
    //    own id. `event_broadcaster` and `canonical_event_sink` are not read at
    //    all. That is deliberate and is stated on
    //    `RecordedEventRouting`'s *What it does NOT promise*: those two decide
    //    whether anything was SENT, which is a property of the producing
    //    process's transports and not of the event, so a record that carried it
    //    would be false the first time it was replayed anywhere else. The
    //    consequence for a sink is concrete: a `CanonicalRuntimeFact` record
    //    does not mean the producer delivered one, only that it classified one.
    //
    //    So the answer is
    //    durable and a foreign process replaying this journal has it too —
    //    which handing the sink a live scope would not have given, because a
    //    foreign process's scope registry is empty and `emit_event`'s
    //    `unwrap_or(false)` turns an absent scope into a SILENT transport-only
    //    downgrade.
    //
    //    **The sink landed too, on 2026-08-28**, and this comment said it had
    //    not for as long as it had. `emit_routed` has a trait default that
    //    forwards to `emit` and discards the routing;
    //    `driver_worker::HostEventSink` now OVERRIDES it and hands the routing
    //    to the host, and `executor.rs::InProcessWorkerHost::
    //    emit_projected_event` honours all three arms — `CanonicalRuntimeFact`
    //    through a scope rebuilt from the RUN's execution id,
    //    `TransportOnly` through the broadcaster, `Unrecorded` through this
    //    process's live rule with a log line rather than silently.
    //
    //    Check, and it is the check that was already written here — it simply
    //    passes now: `HostEventSink` overrides `emit_routed`, AND
    //    `InProcessWorkerHost` overrides `emit_projected_event` to put the
    //    rebuilt event back through `ActionExecutors::emit_event` (or through
    //    the recorded scope directly). Both are outside this module, which is
    //    why this comment could go stale in the met direction without anything
    //    here changing. See *CUTTING INLINE EMISSION IS NOT A ONE-LINE CHANGE*
    //    above.
    //
    //    **Condition 2 being met changes nothing about this line.** Conditions
    //    1 and 3 are both open, and deleting the inline emit while either is
    //    open still blanks live surfaces.
    //
    // 3. EVERY OTHER PATH TO A TRANSPORT IS JOURNALLED TOO — **and the
    //    denominator this is checked against is a FLOOR, not a total.**
    //    **Forty-three sites come through this module — nine on this line,
    //    twenty-nine through `executor.rs::journal_and_emit_at`, five on
    //    [`journal_and_emit_named`]'s — and twenty-one do not.** Re-counted
    //    2026-08-28, on the fourth sweep.
    //
    //    **The unjournalled number has gone UP on every re-count, and no code
    //    regressed to make that happen.** It read fourteen, then twelve, then
    //    twenty-one, while the journalled number rose from thirty-seven to
    //    forty-three. Each correction was the sweep reaching somewhere the
    //    previous sweep's method could not — most recently a filter that named
    //    seven emitter functions and so missed ten free functions in other
    //    modules that end at those same seven, plus three emissions on the
    //    frame ABOVE the loop that no downward closure from a phase can see.
    //    Read *WHAT IS NOT JOURNALLED*'s *METHOD* section before trusting any
    //    of these numbers; it says what the sweep can and cannot reach, which
    //    is the thing a reader has to know to size this condition honestly.
    //
    //    Deleting this line while the twenty-one still emit inline does not
    //    stop duplicate emission: it deletes forty-three events from the
    //    timeline and leaves the duplication where it was.
    //
    //    THE CHECK IS NOT A COUNT OF ANY GREP, and specifically not a count of
    //    the emit-event calls in `executor.rs`. That count reads five today
    //    and only three of the five are unjournalled producers; one is
    //    `journal_and_emit_at`'s own `None` fallback and one is the projector
    //    REPLAYING an already-journalled record. (Described, not quoted: a
    //    quoted pattern would put another match in this file and make a
    //    correct tree read as drifted.)
    //
    //    The check is: **every known site in *WHAT IS NOT JOURNALLED* either
    //    journals or carries a written-down refusal AT THE SITE, AND the
    //    *METHOD* section's stated blind spots have been re-swept since the
    //    last emitter was added.** As of 2026-08-28 **both clauses pass**, and
    //    they were closed by different work: the first by converting the seven
    //    owed-a-diff sites, the second by sweeping classes 3 and 4, which had
    //    never been swept at all. The second sweep found a fifteenth site — a
    //    `broadcaster.emit` behind `dyn OwnershipRuntime` — so closing the
    //    clause ADDED a refusal rather than merely confirming the list.
    //
    //    All fifteen are REFUSED, and every refusal is written at its own
    //    site as well as indexed above:
    //
    //    - **seven outside any phase body** — `AgenticMaxIterationsReached`,
    //      its paired `HitlRequested`, `build_run_setup`'s
    //      `AgenticExecutionStarted` and the `None`-addressed
    //      `emit_agent_execution_mapping` beside it, and the three
    //      `agent_events::emit_agent_cycle_*` calls in `execute_agent_cycle`.
    //      The last three are outside the LOOP, not merely outside a phase;
    //    - **three `emit_transport_only`s in
    //      `analytics/operation_llm_telemetry.rs`** — and NOT for that reason:
    //      a phase IS on the stack. They are refused for the three
    //      obstructions that file's own header lists, of which the first is
    //      that [`journal_and_emit`] is not nameable from outside
    //      `execution::agentic`. Two separate corrections have been made to
    //      WHICH phase reaches them — it is Apply for two of the three, not
    //      Decide — because a fixer told the wrong phase threads an address
    //      into the wrong place;
    //    - **two off-stack telemetry emitters** — `llm_trace_activation`'s
    //      `emit_activity_cost` and `runtime_activity_layer`'s
    //      `spawn_activity_forwarder` — on background tasks with no phase
    //      anywhere on the stack;
    //    - **two `ArtifactV2EventType` sites**, which write to the durable
    //      canonical log already, and where a replay would DUPLICATE rather
    //      than rescue — see *FOUR VOCABULARIES*;
    //    - **one behind a trait object** — `dyn OwnershipRuntime`'s
    //      `update_execution_owner_snapshot`, which reaches the orchestrator's
    //      `broadcaster.emit(ExecutionResponsibilityChanged)`. Refused because
    //      [`journal_and_emit_at`] is not nameable from `orchestrator` AND
    //      because threading an address means putting one on a trait that
    //      exists to keep the loop from knowing what the orchestrator is. The
    //      authority half already replays through
    //      `JournalBody::OwnerTransition`; what a replay loses is the
    //      notification, not the run's authority.
    //
    //    ~~**Seven are OWED A DIFF.**~~ **CONVERTED 2026-08-28**, and they
    //    were what held this condition closed: five `llm_tool_lineage`
    //    emitters reached from `begin_agentic_tool_lineage` and
    //    `finish_agentic_tool_lineage`, and two `publish_agent_update` calls
    //    in `capture_pack_action_artifacts_projection`. Every one was an
    //    `AgentEvent` needing no new record variant — only an address and a
    //    builder returning the envelope instead of sending it. Six took that
    //    shape directly; `emit_completed_tool_dispatch` was RESTRUCTURED
    //    instead, because a twin of its eight-branch pipeline is two
    //    structures that must agree forever.
    //
    // Deleting it is one line only because those three conditions are somebody
    // else's finished work. **All three are met as of 2026-08-28** — condition
    // 2 by the two sink overrides, condition 1 by the seven conversions, and
    // condition 3 by both of its clauses: every one of the fifteen remaining
    // sites carries a written refusal, and classes 3 and 4 have now been swept.
    //
    // While that line stood it was the emit the call site performed before this
    // module existed, byte for byte, and the only reason the events above
    // reached a user at all. That is no longer how they arrive: the projector
    // walks the journal and emits, and this function's job ends at the append.
    //
    // ── THE INLINE EMIT IS GONE, 2026-08-28. ────────────────────────────────
    //
    // It went in ONE change with `WorkerHost::emits_projected_events`, which
    // `executor.rs::InProcessWorkerHost` now answers `true`. Neither half was
    // ever safe alone: flipping the flag by itself DOUBLES every timeline —
    // the projector emits and the inline line still fires, and the projector's
    // dedupe cannot see the inline copy because it never went through a cursor
    // — while deleting the line by itself BLANKS every live surface, because
    // with projection off this was the only path by which a journalled event
    // reached a transport at all.
    //
    // What replaced it: the record goes on the outbox, `PhaseReport::records`
    // carries it to `commit_boundary`, and `ProjectorCursor::project` hands it
    // to `ProjectedEventSink::emit_routed`, which `driver_worker::HostEventSink`
    // overrides and forwards to the host with the record's own
    // `RecordedEventRouting`. The host replays it down the same branch
    // `emit_event` would have taken — `broadcaster.emit` for a canonical
    // runtime fact, `emit_transport_only` otherwise — so delivery is unchanged
    // in shape; what changed is that a cursor now knows it happened.
    //
    // The three conditions this waited on, and what closed each:
    //
    // 1. Every phase-reachable site journals or carries a written refusal at
    //    its own site — fifty journalled, fifteen refused, closed by the seven
    //    owed-a-diff conversions.
    // 2. The record carries the routing decision and the sink reads it — closed
    //    by `HostEventSink::emit_routed` and
    //    `InProcessWorkerHost::emit_projected_event`, which honours all three
    //    `RecordedEventRouting` arms rather than re-deriving from a scope a
    //    replaying process may not hold.
    // 3. Both clauses: the refusals above, AND the *METHOD* section's blind
    //    spots swept. Classes 3 and 4 had never been swept; doing so found a
    //    fifteenth site behind `dyn OwnershipRuntime` and refused it.
    //
    // A fourth thing, not on the original list and load-bearing anyway: the
    // host implements BOTH projection rails. `emit_projected_named_event`'s
    // trait default is a REFUSAL, and a refusal stops the projector's walk
    // below the refused record permanently — so a host with only the unnamed
    // rail would wedge on the first journalled `plan.step.*`. `run_loop::mod`'s
    // `HOST_IMPL` gate asserts the pairing.
}

/// The two gates every journalled record passes, in the order it passes them,
/// with the drain answer supplied by the caller.
///
/// # Why the id first, and why it is one function instead of two copies
///
/// No execution id is neither an error nor rare: `runtime_execution_id_opt`
/// answers `None` for a context that was never given one. Such a record has no
/// [`EventKey`](super::super::journal::EventKey) —
/// `JournalRecord::event_key` takes the id from the caller precisely because it
/// is constant per file — so there is no file to put it in and nothing to
/// address it by. The drain gate comes second because the value it reads is
/// ambient and the id is not.
///
/// [`journal`] and [`journal_named`] each used to spell this pair out for
/// themselves. Two copies of a two-line rule is two chances to drop the id half,
/// and the id half is the one whose failure is silent — `runtime_execution_id`
/// exists beside `runtime_execution_id_opt`, is total, and answers **the empty
/// string** for a context with no id (`executor.rs`). A rail that reached
/// for it would address every unaddressable record under `""`, a key no drain
/// ever asks for, and nothing anywhere would say so.
///
/// # `has_drain` is a closure, and that is what makes the id check testable
///
/// Taking a `bool` would evaluate [`journalling_has_a_drain`] — a cached load,
/// and an environment read on the first journalled event of the process —
/// before the id check had a chance to short-circuit, which is the ordering
/// this function exists to preserve. Taking a closure keeps the order exactly
/// as it was AND lets a test supply `|| true`.
///
/// The ordering matters **less** than it did now that the gate is cached: what
/// the short-circuit saves on an id-less record is one atomic load rather than
/// a lock and two allocations. It is kept because the second half of the
/// paragraph below — the closure being the only way a test can watch the id
/// check decide — never depended on the cost.
///
/// That mattered while an unset `MAGICIAN_EXECUTION_DRIVER` selected the
/// resident arm: ordinary tests then watched the drain gate rather than this id
/// check. Unset now selects the default stateless arm, but supplying the drain
/// answer explicitly still isolates the id-addressability contract from process
/// configuration and from the once-per-process cache. With the drain answer
/// forced to `true` the id check is the only gate left, and
/// `a_context_with_no_execution_id_is_unaddressable_even_when_a_drain_would_take_it`
/// fails the moment it is removed.
fn addressable_execution_id(
    ctx: &AgenticContext,
    has_drain: impl FnOnce() -> bool,
) -> Option<&str> {
    let execution_id = runtime_execution_id_opt(ctx)?;
    if !has_drain() {
        return None;
    }
    Some(execution_id)
}

/// Resolve the execution id this record is addressed under, check that anything
/// will read the record, then journal.
///
/// Split from [`journal_for`] so the id rule stays in one place —
/// `runtime_execution_id_opt` is the same fallback `run_loop::state` derives the
/// identity from — and so the buffer's own behaviour is testable without
/// building a whole `AgenticContext`. That split is also why the drain check
/// lives here and not in [`store`]: every buffer test drives `journal_for` or
/// `store_in` directly, and a gate below them would make the whole suite depend
/// on an ambient environment variable.
fn journal(
    ctx: &AgenticContext,
    scope: Option<&CanonicalEventScope>,
    iteration: usize,
    phase: Phase,
    event: &RuntimeTransportEvent,
) -> bool {
    // Both gates, in order, in [`addressable_execution_id`] — which is also the
    // only way a test can watch the id half of them decide. See that function.
    let Some(execution_id) = addressable_execution_id(ctx, journalling_has_a_drain) else {
        return false;
    };
    // AFTER the gate too, and for the same reason the gate is here rather than
    // in `store`: `routing_for` reaches `map_v2_realtime_event` through
    // `canonical_runtime_fact_of`, and it walks a hundred-odd variants and
    // clones a payload out of the one it matches. On
    // the arm with no drain that is work for a record nobody keeps, and the
    // module docs give the in-process rollback arm's cost as "one cached `bool` load and
    // nothing else".
    // ONE ID, TWO CONSUMERS, AND THEY DO NOT WANT THE SAME STRING. Read the two
    // lines below as one thing and you get the wrong answer for whichever of
    // them you were not thinking about; this has now been proposed as a fix
    // once, in the direction that breaks the half that is currently right.
    //
    // - `routing_for` wants the **RUN's** id, and gets it. It compares against
    //   `scope.execution_id` and answers `Unrecorded` when the scope names a
    //   different execution. A resumed, nested or refinement-pass segment IS the
    //   same execution — `ctx.execution_id` is deliberately untouched by both
    //   nesting sites (`executor.rs::run_single_delegate_in_context`,
    //   `executor.rs::handle_spawn_sub_goal_decision`) — so its events are
    //   canonical runtime facts of that execution and must be recorded as such.
    //   Handing this call the journal FILE's key instead would answer
    //   `Unrecorded` for every one of those runs, which is not a fix but a
    //   silent switch-off of runtime-fact recording for most of the population
    //   that has one. See `routing_for`'s *IT IS NOT THE FILE'S KEY*.
    //
    // - `journal_for` wants a **SEGMENT-unique** key, and since 2026-08-28 it
    //   gets one: `loop_state_address(ctx, execution_id)`, the same fold the
    //   loop-state key uses. Before that it was handed this string, and because
    //   the buffer is process-global and keyed by exactly it, a nested run and
    //   its parent shared one buffer — the parent's in-flight
    //   `SubGoalRequested` was discarded by the nested run's first phase, and a
    //   failed sub-run's records were drained by the PARENT's `Apply` and
    //   re-stamped with the parent's iteration and phase.
    //
    // **Both halves had to move in one change, and that is why this stood open
    // so long.** `take` is keyed by the same string and its three callers are in
    // `executor.rs` — the entry-leftover check and the end-of-phase drain in
    // `InProcessWorkerHost::run_phase`, and `discard_records_no_phase_will_drain`.
    // Re-keying only the write side would have left `take` asking for a key
    // nothing ever wrote: a buffer that fills for nobody, which is strictly
    // worse than one that is shared, and silent in exactly the same way.
    // `loop_state_address` was private to `executor.rs` and is now
    // `pub(in ..::agentic)` for this.
    // TWO KEYS, deliberately, and they differ exactly on the runs this bug was
    // about. `routing_for` gets the RUN's id, because a nested segment's events
    // are runtime facts of the execution it shares. `journal_for` gets the
    // SEGMENT's address, because the buffer is process-global and a parent and
    // its sub-goal would otherwise write into one another's.
    let routing = routing_for(execution_id, scope, event);
    journal_for(
        &loop_state_address(ctx, execution_id),
        iteration,
        phase,
        event,
        routing,
    )
}

/// Journal one `emit_named` call, then make it, exactly as the call site used
/// to.
///
/// # The second rail, and the one reason it is not [`journal_and_emit`]
///
/// `emit_named` is not a thin wrapper over a transport. It stamps `timestamp_ms`
/// into the payload, looks the payload's `task_id` / `execution_id` up in the
/// **chat fan-out** registry, stamps `chat_turn_id` onto the primary payload
/// when a chat session is listening, and then emits the primary envelope plus a
/// re-stamped copy per target (`realtime_events.rs:4686-4790`). Only after all
/// of that does it reach `RuntimeTransportEvent::AgentEvent`.
///
/// So a producer that built that `AgentEvent` itself and went through
/// [`journal_and_emit`] would journal a faithful record of the wrong thing: the
/// projector would replay it through `emit_transport_only` and deliver **one**
/// envelope where the producer delivered `1 + n`, and the chat activity card —
/// the surface the `plan.step.*` sites carry `task_id` specifically to reach —
/// would lose the step lifecycle. `JournalBody::NamedEvent` records the CALL for
/// that reason, and this function is the only producer of one.
///
/// # It takes the broadcaster, not the executors
///
/// Because the call site already has one: every `emit_named` in `executor.rs` is
/// inside an `if let Some(broadcaster) = executors.event_broadcaster.as_ref()`,
/// and the record should say what the producer DID. Taking the executors and
/// re-checking would journal a record on a run with no broadcaster, where
/// nothing was emitted at all — [`journal_and_emit`] is free to do that because
/// `ActionExecutors::emit_event` is total, and this rail's entry point is not.
///
/// # Refusal, and the same answer as the other rail
///
/// [`JournalAppend::named_event`] refuses a body that would push a record over
/// [`MAX_JOURNAL_RECORD_BYTES`], for the reason [`journal_and_emit`] sets out at
/// length: the store's ceiling applies to the whole batch, so an oversized
/// observability payload that reached it would fail the append, fail the commit
/// and stall a live run. The answer here is the same — drop the record, count
/// it, then use the inline fallback — the same exact-one-path rule as the
/// transport-event rail.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) fn journal_and_emit_named(
    ctx: &AgenticContext,
    broadcaster: &RuntimeTransportBroadcaster,
    iteration: usize,
    phase: Phase,
    name: &str,
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
    mut payload: Value,
) {
    // BEFORE the journal call, and this is the one thing this function does that
    // the call site did not.
    //
    // `emit_scoped_or_unscoped` stamps `timestamp_ms` with
    // `entry(..).or_insert_with(Utc::now)` (`realtime_events.rs:4694`) — AFTER
    // this function would otherwise have captured the payload. So a record
    // journalled first carries no `timestamp_ms` at all, and a projector
    // replaying it a boundary later gets one stamped at REPLAY time: the
    // timeline would show the step finishing when the outbox drained rather than
    // when the step finished.
    //
    // Stamping here fixes it in the direction the broadcaster already supports
    // rather than by adding a rule. `or_insert` means "a caller that already
    // populated this keeps it", which that method's own doc names as the case
    // for an emitter re-emitting an event at its original time. So the live emit
    // below sees the value this line wrote and does not overwrite it, the record
    // carries the same value, and the two cannot disagree.
    //
    // It is NOT a second implementation of the stamp: it is the same key, the
    // same units, and the same or-insert semantics, placed one call earlier so
    // that both consumers see one value. What would be a second implementation
    // is a `timestamp_ms` derived some other way — a monotonic counter, a
    // phase-entry time — and that is the thing not to do here.
    //
    // Paid on every arm, including the one with no drain: one map lookup, one
    // `String` allocation for the key, and on the ordinary path one insert. No
    // encode and no clone. The allocation is unconditional — `Map::entry` takes
    // `S: Into<String>` and converts eagerly — and it is stated rather than
    // rounded off, because on the `Inprocess` arm this line IS the named rail's
    // entire production-visible cost. See [`stamp_emit_time`].
    //
    // The module docs' "one cached `bool` load and nothing else" is stated
    // about the ENCODES, and this is not one; it is named there too so the
    // claim stays exactly true.
    stamp_emit_time(&mut payload);
    let journalled = journal_named(
        ctx, iteration, phase, name, agent_id, principal, workspace, &payload,
    );
    // ┌──────────────────────────────────────────────────────────────────────┐
    // │ Same exact-one-path switch as the transport rail: the projector owns │
    // │ accepted records and inline delivery owns every refusal/no-drain.     │
    // └──────────────────────────────────────────────────────────────────────┘
    //
    // The three conditions are `journal_and_emit`'s, unchanged and not restated
    // — one copy of them is already one more than can be maintained. Two things
    // are worth adding here because they are specific to this rail:
    //
    // - Condition 2 (the sink can re-derive what the producer decided) is
    //   VACUOUS here rather than met. This rail has one delivery branch, so
    //   there is nothing for a sink to re-derive; `JournalBody::NamedEvent`
    //   carries no `RecordedEventRouting` for that reason.
    // - Condition 3 is what this rail was built for and it is now closer:
    //   `driver_worker::WorkerHost::emit_projected_named_event` is the delivery
    //   end, and a host that implements it puts the record back through
    //   `emit_named` — the same entry point, so the fan-out happens on replay
    //   too.
    //
    // Removing the fallback while this arm has no drain blanks
    // `plan.step.started`, `plan.step.finished` and `tool.result.projected` on
    // every live surface; making it unconditional duplicates accepted records.
    if !journalled {
        broadcaster.emit_named(name, agent_id, principal, workspace, payload);
    }
}

/// Put the producer's own emit time into the payload, if the payload does not
/// already carry one.
///
/// # It has to be or-insert, and that is the whole reason it can exist here
///
/// `RuntimeTransportBroadcaster::emit_scoped_or_unscoped` does the same thing a
/// call later, with the same key and the same `entry(..).or_insert_with(..)`
/// (`realtime_events.rs:4694`). Because the broadcaster's is an *or*-insert, a
/// value written here survives it, so the record and the emitted envelope carry
/// **one** timestamp rather than two that differ by however long the producer
/// took. An unconditional insert in either place would break that; this one is
/// or-insert for the same reason the broadcaster's is, and the broadcaster's own
/// doc names the case — "lets specific emitters carry the original event time
/// when re-emitting".
///
/// # A non-object payload is left alone, and that is not a silent skip
///
/// The broadcaster does the same: its stamp is inside `if let Some(obj) =
/// payload.as_object_mut()`. There is nowhere to put a field on a `Value` that
/// is not a map, and inventing a wrapper object here would change the shape the
/// transport carries. Every call site in `executor.rs` passes a `json!({..})`.
///
/// # IT ALLOCATES, ONCE, ON EVERY NAMED EMIT — including the ones it no-ops on
///
/// `"timestamp_ms".to_string()` is built before `entry` is entered, and no
/// version of this avoids it: `serde_json::Map::entry` is generic over
/// `S: Into<String>` and calls `key.into()` eagerly, so the owned key exists
/// whether the entry is occupied or vacant. `&str` at the call site would only
/// move the `to_string()` inside the callee. The only shape that skips the
/// allocation is a `contains_key` guard, and it is **not** taken: it trades one
/// allocation on the already-stamped path for a second lookup on the common
/// path, and the common path is the vacant one — every named call site in
/// `executor.rs` builds its payload from a `json!` with no `timestamp_ms` in it,
/// so the entry is vacant and the key is consumed by the insert anyway.
///
/// Written down because this function runs **before both gates**, so on the
/// `Inprocess` arm — no drain, nothing journalled, inline delivery — this
/// allocation and stamp are paid before the broadcaster call. A cost on the
/// fallback path is one a doc must not round off.
fn stamp_emit_time(payload: &mut Value) {
    if let Some(object) = payload.as_object_mut() {
        object
            .entry("timestamp_ms".to_string())
            .or_insert_with(|| Value::from(chrono::Utc::now().timestamp_millis()));
    }
}

/// Resolve the execution id, check that anything will read the record, then
/// journal one named call.
///
/// The same two gates as [`journal`] and literally the same code —
/// [`addressable_execution_id`], which both rails call. It used to be a second
/// copy of the pair; see that function for why one copy matters here and for the
/// `runtime_execution_id` shape that a lost id check turns into an empty-string
/// address.
///
/// There is no `routing_for` step, so this rail does not pay the
/// `map_v2_realtime_event` walk the transport rail does. What it pays instead is
/// [`build_named`]'s clone and four `String`s, and those are also after both
/// gates. What it pays *before* them, on every arm, is [`stamp_emit_time`].
#[allow(clippy::too_many_arguments)]
fn journal_named(
    ctx: &AgenticContext,
    iteration: usize,
    phase: Phase,
    name: &str,
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
    payload: &Value,
) -> bool {
    let Some(execution_id) = addressable_execution_id(ctx, journalling_has_a_drain) else {
        return false;
    };
    // The SEGMENT's address, for the same reason the transport rail uses it.
    journal_named_for(
        &loop_state_address(ctx, execution_id),
        iteration,
        phase,
        name,
        agent_id,
        principal,
        workspace,
        payload,
    )
}

/// Which of the two things `ActionExecutors::emit_event` does to this event.
///
/// # This is the CLASSIFICATION, not a copy of `emit_event`'s branch selection
///
/// The distinction matters and is easy to lose. `emit_event`
/// (`executor.rs`) picks a branch from two things multiplied together:
///
/// 1. **What the event is** — does `map_v2_realtime_event` map it, and does the
///    mapped `execution_id` name *this run's own* execution? That is the
///    classification, and it is what this function answers.
/// 2. **What transports the producer happens to hold** — a broadcaster, a
///    canonical sink, or neither.
///
/// A producer with a canonical event and no `canonical_event_sink` emits
/// nothing at all. Recording *that* would tell a replayer "this event was not a
/// runtime fact", which is false: it was one, and the producer had nowhere to
/// put it. So this records what the event **is**, and the replaying process
/// applies its own availability — which is the only division that survives the
/// event being replayed somewhere else.
///
/// # It WAS a third implementation of the predicate — closed 2026-08-28
///
/// The shared rule now lives once, as
/// [`canonical_runtime_fact_of`](crate::magician_v2::artifact_v2::canonical_runtime_fact_of),
/// and this function calls it. It was written out three times before that: here,
/// and twice inside `executor.rs::ActionExecutors::emit_event` — as
/// `should_emit_runtime_fact` in the broadcaster branch and inlined again in the
/// canonical-sink branch.
///
/// The copies agreed, and nothing kept them agreeing. That mattered more after
/// the final switch than before it: this function's answer is what the projector
/// replays an event by, and a record replays under the rule in force **when it
/// was written**, so a divergence would bake into the journal permanently and a
/// later correction would not reach records already on disk.
///
/// **This function is still not merely that call.** After the shared rule
/// answers yes, it asks one more thing the rule cannot: whether `scope`'s
/// execution is the one whose journal this record is being filed in. That is a
/// property of the RECORD rather than of the event, and its answer —
/// `Unrecorded` — is one a live emit has no use for, because a live emit has no
/// record. That asymmetry is why the fix was an extraction and not, as three
/// documents said, `emit_event` calling this function.
///
fn routing_for(
    journal_execution_id: &str,
    scope: Option<&CanonicalEventScope>,
    event: &RuntimeTransportEvent,
) -> RecordedEventRouting {
    // No scope: this run has no canonical identity, so nothing it emits is a
    // runtime fact. `emit_event`'s own `None` handling says the same thing.
    let Some(scope) = scope else {
        return RecordedEventRouting::TransportOnly;
    };
    // The shared rule, and since 2026-08-28 the ONLY copy of it — see
    // [`canonical_runtime_fact_of`]. It folds the two checks that used to be
    // written out here: unmapped (`map_v2_realtime_event` has no canonical type
    // for the variant, so there is nothing to persist even in principle), and
    // another run's id (a child's, most often — a runtime fact of THAT run,
    // which this one must not persist under its own scope).
    //
    // Both answered `TransportOnly` separately and still do together; the
    // distinction between them was never expressed in the result, only in the
    // comments, which are kept here because they are the reason the answer is
    // right rather than merely what it is.
    if canonical_runtime_fact_of(scope, event).is_none() {
        return RecordedEventRouting::TransportOnly;
    }
    // The scope names an execution, and the record is about to be filed in
    // ANOTHER run's journal. The fifth field is not recorded, so a scope written
    // here would be rebuilt against this journal's run while carrying the other
    // run's principal, workspace and task — three right answers under a wrong
    // subject, which is worse than no answer.
    //
    // `journal_execution_id` is the RUN's id, not the journal file's key. Those
    // differ for every resumed, nested and refinement-pass invocation, and
    // comparing against the file key here would answer `Unrecorded` for all of
    // them — which would be wrong, because their events are canonical runtime
    // facts of the execution they share. See *IT IS NOT THE FILE'S KEY* above.
    //
    // `Unrecorded` because it is exactly true: this producer cannot express
    // the decision in this record. It is NOT `TransportOnly`, which would
    // claim a decision the producer did not make and would downgrade a real
    // runtime fact silently — the failure this whole field exists to end.
    if scope.execution_id != journal_execution_id {
        return RecordedEventRouting::Unrecorded;
    }
    RecordedEventRouting::CanonicalRuntimeFact {
        // Four fields, not five. `execution_id` is a run-level constant and is
        // not written per line; a reader must supply it from the run and NOT
        // from the journal's key — see `RecordedCanonicalScope`'s *THE FILE KEY
        // IS NOT THE EXECUTION ID*.
        scope: RecordedCanonicalScope {
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            task_id: scope.task_id.clone(),
            ui_thread_id: scope.ui_thread_id.clone(),
        },
    }
}

/// Whether anything in this process will ever take what [`store`] keeps.
///
/// # A stopgap, and the shape of the thing it is standing in for
///
/// The drain is `executor.rs::InProcessWorkerHost::run_phase`, which is built
/// only by the **stateless** arm of `execute_agentically_inner`. The explicit
/// rollback arm — `run_loop::driver_inproc::run_iteration` — calls the six phases
/// directly, assembles no `PhaseReport`, and names this module nowhere. So on
/// that arm a stored record has no reader and never will.
///
/// Storing anyway is not free and not silent. It costs two JSON encodes an
/// event, holds up to [`MAX_PENDING_BYTES_PER_RUN`] per run and
/// `MAX_TRACKED_RUNS × MAX_PENDING_BYTES_PER_RUN` per process, and — because a
/// long run really does reach 256 KiB of `AgenticDecisionMade` and
/// `LLMResponseReceived` payloads — turns two `[LOOP_OUTBOX]` warnings written
/// for anomalies into the ordinary output of a healthy run. An operator who
/// grepped for either would be reading noise.
///
/// # Why the flag and not something on the context
///
/// Because there is nothing on the context to read. `execute_agentically_inner`
/// resolves the arm once per run into a local (`RunBindings::execution_driver`)
/// and hands the phases borrows, not the choice. Reading the environment here is
/// reading the same variable `run_loop::driver_for_run` read for this run, by the
/// same pure resolver, so the two cannot disagree about what a value *means* —
/// only, and only under the mutation below, about *when* it was read.
///
/// # IT IS CACHED, and this reverses a decision recorded here until 2026-08-28
///
/// The paragraph that used to be here said the environment was read *per event*
/// and that a cache was refused, "because a cache moves the same hazard to the
/// first run in the process and makes it permanent". The reversal is deliberate
/// and the argument it replaces was in tension with the sentence right after it,
/// which is still true and is the whole basis for caching: **the flag is
/// process-grain and a revert needs a restart** (`run_loop::driver_for_run`'s
/// own docs say so — the variable is read from the process environment, so
/// reverting kills every in-flight resident execution).
///
/// What the per-event read cost, per journalled event, on every arm:
///
/// - one acquisition of the standard library's process-wide environment lock,
///   which is **shared with every concurrently executing run** — the one part of
///   this that does not merely get slower with load, it gets slower with
///   concurrency;
/// - one `String` allocation for the value in `std::env::var`;
/// - one more in [`ExecutionDriver::select`], which does
///   `raw.trim().to_ascii_lowercase()`.
///
/// A [`OnceLock`] load replaces all three. It is `bool`, so the fast path is one
/// acquire load and a copy.
///
/// **What changes, stated rather than waved past.** Mutating
/// `MAGICIAN_EXECUTION_DRIVER` inside a live process is not something a deployed
/// process does — nothing in this workspace calls `set_var` on it, and
/// `magician/tests/driver_phase_differential.rs` sets it on a **child process**
/// — but if it happened, the two shapes differ:
///
/// - *Uncached, as before:* the split lands **inside one run**. Some of a run's
///   events are journalled and the rest are not, which is a hole in the middle
///   of one log — the loss this module works hardest to avoid.
/// - *Cached, as now:* the answer is whatever the first journalled event of the
///   process resolved. A later run that `driver_for_run` puts on the other arm
///   disagrees with this gate for its whole life: on `Stateless`-after-cached-
///   `Inprocess` it journals nothing and its drain finds an empty list every
///   phase; on the reverse it buffers for a drain that never comes, which the
///   two `[LOOP_OUTBOX]` warnings do report.
///
/// Neither shape is detectable from here without the read that caching removes,
/// so this is a trade and not a strict improvement. It is taken because the
/// cached failure requires an unsupported mid-process mutation, while the
/// per-event cost was paid by every run on every event; and because "no effect
/// until restart" is what the flag already promises everywhere else.
///
/// The cache is seeded by the first **journalled** event, not at process start —
/// the environment is inherited at exec and nothing writes it afterwards, so
/// those are the same value.
///
/// No logging, on purpose. [`ExecutionDriver::from_env`] shouts at `error` when
/// the value is unrecognised, and it is already called once per run;
/// [`ExecutionDriver::select`] is the pure half, so this cannot turn a typo into
/// one log line per event — and now not even one per process.
fn journalling_has_a_drain() -> bool {
    /// Resolved once per process. See this function's docs for why the
    /// per-event read it replaced was not the safer choice it looked like.
    static HAS_A_DRAIN: OnceLock<bool> = OnceLock::new();
    *HAS_A_DRAIN.get_or_init(|| {
        let raw = std::env::var(EXECUTION_DRIVER_ENV).ok();
        arm_has_a_drain(ExecutionDriver::select(raw.as_deref()).driver)
    })
}

/// Which arms assemble a `driver_worker::PhaseReport`, and therefore call
/// [`take`].
///
/// Split from [`journalling_has_a_drain`] so the RULE is testable without a
/// process environment. Mutating `MAGICIAN_EXECUTION_DRIVER` from a test is
/// process-global and races every concurrently running test that resolves an
/// arm, so the environment read is left uncovered on purpose and the decision it
/// feeds is covered instead.
///
/// Exhaustive rather than `matches!`, deliberately: a third arm added to
/// [`ExecutionDriver`] must fail to compile here rather than default to "no
/// drain" and silently stop journalling on it.
fn arm_has_a_drain(driver: ExecutionDriver) -> bool {
    match driver {
        // `executor.rs::InProcessWorkerHost::run_phase` is the only assembler of
        // a `PhaseReport` in the tree, and it drains once per phase.
        ExecutionDriver::Stateless => true,
        // `driver_inproc::run_iteration` calls the six phases directly, assembles
        // no `PhaseReport`, and names this module nowhere.
        ExecutionDriver::Inprocess => false,
    }
}

fn journal_for(
    execution_id: &str,
    iteration: usize,
    phase: Phase,
    event: &RuntimeTransportEvent,
    routing: RecordedEventRouting,
) -> bool {
    journal_produced(
        execution_id,
        iteration,
        phase,
        build(iteration, phase, event, routing),
    )
}

/// The same, for one `emit_named` call.
///
/// Its own entry point rather than a parameter on [`journal_for`], so the two
/// rails cannot be confused at a call site and so the transport-event rail's
/// signature — which five tests in this module drive directly — is unchanged.
/// Everything after the build is [`journal_produced`], which both share: the
/// buffer, the two bounds, the two once-per-run warnings and the drain's
/// accounting are the same for a named record as for any other, because a
/// projector's view of a short list is the same whichever rail was short.
#[allow(clippy::too_many_arguments)]
fn journal_named_for(
    execution_id: &str,
    iteration: usize,
    phase: Phase,
    name: &str,
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
    payload: &Value,
) -> bool {
    journal_produced(
        execution_id,
        iteration,
        phase,
        build_named(
            iteration, phase, name, agent_id, principal, workspace, payload,
        ),
    )
}

/// Store one built record and answer the two losses it can take.
///
/// The tail both rails share. `iteration` and `phase` reach here only to be
/// named in a warning — the driver re-stamps both on the record itself — and
/// they are still passed rather than dropped because an operator reading
/// *"an event could not be journalled"* needs to know which phase produced it,
/// and the record that would have said so is the one that does not exist.
fn journal_produced(
    execution_id: &str,
    iteration: usize,
    phase: Phase,
    produced: Result<(JournalAppend, usize), String>,
) -> bool {
    let outcome = store(execution_id, produced);
    match outcome {
        Stored::Kept => true,
        Stored::CapacityRefused { refused } => {
            // Logarithmic reporting keeps a stuck/orphaned set visible without
            // turning every event on every newly arriving run into a warning.
            if refused.is_power_of_two() {
                warn!(
                    execution_id = %execution_id,
                    refused,
                    tracked = MAX_TRACKED_RUNS,
                    "[LOOP_OUTBOX] the process outbox is at its run cap; this event was not \
                     accepted and is emitted inline. Held records were preserved"
                );
            }
            false
        },
        Stored::Discarded {
            dropped,
            held_bytes,
        } => {
            // Once per run, not once per event: a full buffer stays full, and a
            // warning per discarded event would bury the one that says why.
            if dropped == 1 {
                warn!(
                    execution_id = %execution_id,
                    held_bytes,
                    limit_bytes = MAX_PENDING_BYTES_PER_RUN,
                    "[LOOP_OUTBOX] this run's undrained journal buffer is full, so events are \
                     being produced and discarded. Its phase has emitted more than the buffer \
                     holds between two drains. Emission is unaffected"
                );
            }
            false
        },
        Stored::NotJournalable { reason, refused } => {
            // Once per run, like the full-buffer case above, and this was NOT
            // always so. It used to fire per event, on the argument that "a
            // producer that emits an over-ceiling event is an anomaly worth a
            // line each time". That argument holds only while over-ceiling
            // events are anomalous, and they are not: `LLMResponseReceived`
            // carries `reasoning_summary` and `AgenticDecisionMade` carries
            // `raw_decision` + `thinking` + `evidence`, and on a thinking model
            // over a long turn those routinely pass MAX_JOURNAL_RECORD_BYTES.
            // Per-event, one such run turns the line an operator greps for a
            // real anomaly into per-iteration background.
            //
            // `refused` is the run's own running total, carried out of the lock
            // by `Stored` for exactly this test — the same shape `dropped` uses,
            // and for the same reason it is a separate counter from `dropped`:
            // whichever fires first must not consume the other's one firing. The
            // total, and the reasons behind the first one, still reach the drain
            // through [`take`].
            if refused == 1 {
                warn!(
                    execution_id = %execution_id,
                    iteration,
                    ?phase,
                    reason = %reason,
                    "[LOOP_OUTBOX] an event could not be journalled, so it was dropped rather \
                     than carried into a batch that would fail the commit and stall the run. It \
                     was still emitted inline. Logged once per run; the drain reports the total"
                );
            }
            false
        },
    }
}

/// Turn an event into the record it becomes and the bytes it is charged, or into
/// the reason it can become neither.
///
/// Outside the lock on purpose: the JSON encodes are the expensive half of this
/// module and neither of them needs the map.
///
/// # TWO ENCODES, NOT THREE — changed 2026-08-28
///
/// It used to be three: [`split`]'s `to_value`, the size probe inside
/// `JournalAppend::event`, and then `serde_json::to_string(&append.body)` to
/// learn how many bytes to charge the run's buffer. The third was the same body
/// the probe had encoded one call earlier, so a long run paid a full re-encode
/// of every `AgenticDecisionMade` and `LLMResponseReceived` payload — the two
/// that are kilobytes each — to recompute something already measured.
///
/// [`JournalAppend::event_measured`] hands that measurement back and the third
/// encode is gone. **The number is not identical to the one it replaces**, and
/// that is worth stating rather than glossing: the probe measures the whole
/// LINE at the widest stamps, so the charge is larger than the old body-only
/// figure by a fixed envelope of roughly 130 bytes. Against
/// [`MAX_PENDING_BYTES_PER_RUN`] — 256 KiB — that is an over-charge of about
/// 0.05% per record, in the direction of holding *less*, and it is the number
/// the store will actually write. Nothing asserts the old figure: every buffer
/// test that needs a full run sets `RunOutbox::bytes` directly.
fn build(
    iteration: usize,
    phase: Phase,
    event: &RuntimeTransportEvent,
    routing: RecordedEventRouting,
) -> Result<(JournalAppend, usize), String> {
    let (event_type, payload) = split(event)?;
    // `JournalAppend::event_measured` runs the store's own encode and the
    // store's own ceiling rather than an estimate of them, so a record that
    // passes here cannot be one the store refuses — and it returns what that
    // encode measured, so nothing below re-encodes to find out.
    // The routing goes through the size probe with everything else, so a record
    // this accepts is one the store accepts INCLUDING the scope it carries. An
    // earlier shape that stamped the routing on afterwards would have measured a
    // body smaller than the one written.
    JournalAppend::event_measured(iteration, phase, event_type, payload, routing)
        .map_err(|error| error.to_string())
}

/// The same, for one `emit_named` call.
///
/// Deliberately parallel to [`build`] rather than folded into it: there is no
/// [`split`] step, because the five arguments ARE the record — nothing is
/// encoded and taken apart, so the "did this encode to the tag/content shape"
/// failure has no analogue here and this rail can only be refused for size.
///
/// # What one record costs here, stated in full rather than as "one clone"
///
/// - **One payload clone**, and it is the only *deep* copy: `emit_named` takes
///   the payload by value and the record needs an owned copy, so one of the two
///   has to be a copy while inline fallback remains possible.
/// - **Four `String` allocations**, which an earlier version of this paragraph
///   left out by saying the clone was "the only clone the named rail adds":
///   `name.into()` and `agent_id.into()` inside
///   [`JournalAppend::named_event_measured`](super::super::journal::JournalAppend::named_event_measured),
///   plus `principal.map(str::to_string)` and `workspace.map(str::to_string)`
///   here. All four are short — a taxonomy name, an agent id, a principal, a
///   workspace — and all four are unavoidable for the same reason the clone is:
///   the record outlives the borrows the call site holds.
/// - **ONE JSON encode**, the size probe, whose measurement is also the byte
///   charge — exactly as on the transport rail, and for the reason [`build`]'s
///   *TWO ENCODES, NOT THREE* gives. It was two until 2026-08-28, the second
///   being a re-encode of the body the probe had just encoded.
///
/// Every one of them is paid **after** `journal_named`'s execution-id check and
/// **after** its drain gate, so a run on an arm with no drain pays none of it.
/// The one cost the in-process rollback arm does pay is [`stamp_emit_time`]'s, which sits
/// above both gates and is documented there.
fn build_named(
    iteration: usize,
    phase: Phase,
    name: &str,
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
    payload: &Value,
) -> Result<(JournalAppend, usize), String> {
    // `JournalAppend::named_event_measured` runs the store's own encode and the
    // store's own ceiling — the same `measure_against_the_widest_record` the
    // transport rail goes through — so a record that passes here cannot be one
    // the store refuses, and the size it charges is the one that encode already
    // measured.
    JournalAppend::named_event_measured(
        iteration,
        phase,
        name,
        agent_id,
        principal.map(str::to_string),
        workspace.map(str::to_string),
        crate::magician_v2::json_traversal::clone_json_iteratively(payload),
    )
    .map_err(|error| error.to_string())
}

/// Put the record in the run's buffer.
///
/// The only place the `PENDING` guard is taken on the write path, and it is
/// released before the returned [`Stored`] result is logged, so a `tracing`
/// subscriber cannot re-enter this module under the lock.
///
/// Takes an already-[`build`]-ed record rather than an event, so the two rails
/// share the buffer, the run cap and refusal accounting without this
/// function knowing there are two.
fn store(execution_id: &str, produced: Result<(JournalAppend, usize), String>) -> Stored {
    let mut pending = PENDING.lock().unwrap_or_else(|error| error.into_inner());
    store_in(&mut pending, execution_id, produced)
}

/// The map half of [`store`], over a caller-supplied [`Pending`].
///
/// Split out so capacity refusal is testable without filling the process-global
/// buffer or interfering with concurrently running tests.
///
/// # The one input that is not a parameter
///
/// [`current_dispatch_lane`]. It is read here rather than passed because the
/// party that knows the lane is the dispatch, not the call site — the call
/// chain between `execute_direct_path_on_scheduler_root` and this function is
/// the whole of `executor.rs`'s direct path, and threading a value down it is
/// the plumbing job *AN AMBIENT ADDRESS CANNOT WORK HERE* sizes.
///
/// It costs this function nothing in testability: the lane is set by a scope a
/// test establishes explicitly, so it is as controllable as an argument and
/// **not** timing-dependent. Every existing case runs outside any scope and
/// therefore stores `None`, which is what a record produced on a phase's own
/// stack should carry.
fn store_in(
    pending: &mut Pending,
    execution_id: &str,
    produced: Result<(JournalAppend, usize), String>,
) -> Stored {
    // One thread-local read, taken before `pending` is borrowed. It DOES run
    // under the `PENDING` guard when the caller is [`store`] — that is fine and
    // is not the discipline [`Stored`] describes, which is about `warn!` and
    // re-entrant `tracing` subscribers. A task-local read cannot re-enter this
    // module.
    let lane = current_dispatch_lane();
    let is_new_run = !pending.runs.contains_key(execution_id);

    // Never make room by deleting records that were already accepted. At the
    // process bound, the new event remains unaccepted and therefore follows the
    // exact-once-path rule's inline fallback. Existing entries continue to drain
    // normally, so capacity returns without creating a hole in any journal.
    if is_new_run && pending.runs.len() >= MAX_TRACKED_RUNS {
        pending.capacity_refused = pending.capacity_refused.saturating_add(1);
        return Stored::CapacityRefused {
            refused: pending.capacity_refused,
        };
    }

    let outcome = {
        // The entry is created even for a refusal, so the loss is attributed to
        // the run that produced it and [`take`] can report it.
        let run = pending.runs.entry(execution_id.to_string()).or_default();
        match produced {
            Err(reason) => {
                run.refused += 1;
                Stored::NotJournalable {
                    reason,
                    refused: run.refused,
                }
            },
            // `record_bytes`, not `body_bytes`, and the rename is the point:
            // since 2026-08-28 [`build`] charges what the size probe measured,
            // which is the whole encoded LINE at the widest stamps rather than
            // the body alone. The old name said the smaller thing.
            Ok((append, record_bytes)) => {
                if run.bytes + record_bytes > MAX_PENDING_BYTES_PER_RUN {
                    run.dropped += 1;
                    Stored::Discarded {
                        dropped: run.dropped,
                        held_bytes: run.bytes,
                    }
                } else {
                    run.bytes += record_bytes;
                    // The lane travels with the record, not with the run: one
                    // run's buffer holds records from the phase's own stack and
                    // from every dispatch the phase made, and [`take`] has to
                    // tell them apart.
                    run.records.push(PendingRecord { lane, append });
                    Stored::Kept
                }
            },
        }
    };

    outcome
}

/// What one drain took, and everything that did not survive to be in it.
///
/// Two counts and not one, because the producer can lose a tail in two
/// independently diagnosed ways:
///
/// - `dropped` — the run's buffer was full, so events after that point were not
///   kept. A **tail**.
/// - `refused` — the event never became a record: it did not encode to the
///   tag/content shape, or the record would have been over
///   [`MAX_JOURNAL_RECORD_BYTES`]. Also a tail, and a scattered one.
struct Drained {
    records: Vec<JournalAppend>,
    dropped: usize,
    refused: usize,
}

/// Take everything this run has journalled and not yet had drained.
///
/// # ONE PRODUCTION CALLER, ON THE DEFAULT STATELESS ARM
///
/// Stated at the drain because a drain reads as a thing that always runs. Since
/// 2026-08-27 `executor.rs::InProcessWorkerHost::run_phase` calls this once per
/// phase, immediately before it pushes any owner-transition record — the only
/// assembler of a `PhaseReport` there is, and the only place this belongs.
///
/// That host is built only by the **stateless** arm.
/// `run_loop::driver_inproc::run_iteration` — the rollback arm — assembles no
/// `PhaseReport`, so nothing calls this there. Rather than let the buffer fill
/// for a reader that does not exist, [`journalling_has_a_drain`] stops [`store`]
/// keeping anything on that arm, so an absent run there is absent because
/// nothing was written and not because something was lost. See the module docs.
///
/// The returned order is the order the phase produced them, which is what
/// `driver_worker::commit_boundary` turns into ordinals and what
/// `ProjectorCursor` dedupes on. A caller must not reorder it.
///
/// # ONE reordering happens HERE, and it is what makes that sentence true
///
/// Records produced inside a dispatch carry a [`DispatchLane`], and
/// [`sort_dispatch_lanes`] puts them back in the order the dispatches were
/// opened before this returns. That is not a caller's reorder sneaking in: the
/// order a `join_all` over the scheduler-root lane buffers records in is
/// completion order — a race, not an order the phase chose — so undoing it is
/// how the drained list comes to have the produced order this paragraph
/// promises. Records the phase emitted on its own stack carry no lane and are
/// never moved. See [`DispatchLane`].
///
/// # What it says about a list that is not everything the run produced
///
/// Two tail losses can sit between what a phase emitted and what comes back
/// here: an event can exceed the per-run byte budget or fail the journal-record
/// encoding/size contract. Both are reported below. A run with no entry is not
/// a loss; a phase that emitted nothing has no entry, and that is ordinary.
pub(in crate::magician_v2::execution::agentic) fn take(execution_id: &str) -> Vec<JournalAppend> {
    let mut pending = PENDING.lock().unwrap_or_else(|error| error.into_inner());
    let drained = take_in(&mut pending, execution_id);
    drop(pending);

    // Both counters are losses to whoever takes this list. Gating only on
    // `dropped` would make a run that lost oversized/unencodable events look
    // complete.
    if drained.dropped > 0 || drained.refused > 0 {
        warn!(
            execution_id = %execution_id,
            dropped_buffer_full = drained.dropped,
            refused_unjournalable = drained.refused,
            taken = drained.records.len(),
            "[LOOP_OUTBOX] this drain is short: events were produced and never reached this list, \
             because the run's buffer was full or the record was one the store would refuse"
        );
    }

    drained.records
}

/// The map half of [`take`], over a caller-supplied [`Pending`].
///
/// Split out for the same reason [`store_in`] is: drain accounting and ordering
/// can be tested without touching the process-global buffer.
///
/// An absent run is not an error. A phase that emitted nothing has no entry,
/// and that is the ordinary case.
fn take_in(pending: &mut Pending, execution_id: &str) -> Drained {
    let (records, dropped, refused) = match pending.runs.remove(execution_id) {
        Some(mut run) => {
            // THE ONE REORDERING THIS BUFFER PERFORMS, and the reason [`take`]'s
            // "the returned order is the order the phase produced them" is still
            // true afterwards: what a `join_all` produces is not an order, it is
            // a race. See [`sort_dispatch_lanes`].
            sort_dispatch_lanes(&mut run.records);
            (
                run.records
                    .into_iter()
                    .map(|record| record.append)
                    .collect::<Vec<_>>(),
                run.dropped,
                run.refused,
            )
        },
        None => (Vec::new(), 0, 0),
    };
    Drained {
        records,
        dropped,
        refused,
    }
}

/// Put the records a dispatch produced back in the order the dispatches were
/// OPENED, undoing the completion order a `join_all` buffered them in.
///
/// # It sorts inside a contiguous laned run, and never across an unlaned record
///
/// An unlaned record is one the phase emitted on its own stack, and its position
/// is already an address that both attempts agree on — so this function must not
/// be able to move one. Sorting the whole list by lane would: it would hoist
/// every laned record past every unlaned one, which re-addresses records that
/// were never in question to fix records that were.
///
/// So the list is walked as maximal runs of laned records and each run is sorted
/// on its own. An unlaned record is a wall. In practice `apply` emits nothing on
/// its own stack between the primary dispatch and the end of the sequential
/// tail, so the whole batch is one run; the walk exists so that a phase which
/// DOES emit in between keeps that emission where it put it, instead of the
/// answer depending on a fact about `apply` that nothing enforces.
///
/// # A STABLE sort, and that is load-bearing rather than a default
///
/// `slice::sort_by_key` is stable, so records sharing a lane keep the order that
/// dispatch produced them in. That order is already deterministic — it is one
/// task's own emission sequence — and re-deriving it from anything else would be
/// inventing an order where a correct one exists. The sort therefore changes the
/// relative position of two records only when they came from DIFFERENT
/// dispatches, which is exactly the pair whose relative position was a race.
///
/// # Cost
///
/// One `sort_by_key` per drain over a list that is normally under a dozen
/// records and is bounded by [`MAX_PENDING_BYTES_PER_RUN`] regardless. The
/// `is_none` fast path means a phase that made no dispatch pays one pass and no
/// allocation.
fn sort_dispatch_lanes(records: &mut [PendingRecord]) {
    let mut start = 0usize;
    while start < records.len() {
        if records[start].lane.is_none() {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < records.len() && records[end].lane.is_some() {
            end += 1;
        }
        // A run of one is already sorted, and skipping it keeps the ordinary
        // single-dispatch phase allocation-free.
        if end - start > 1 {
            records[start..end].sort_by_key(|record| record.lane);
        }
        start = end;
    }
}

/// Split a transport event into the two halves a `JournalBody::Event` carries.
///
/// `RuntimeTransportEvent` is adjacently tagged, so its encoding is
/// `{"event_type": "<Variant>", "data": {…}}` and this is that object taken
/// apart. Nothing is re-derived and nothing is re-named: the string this returns
/// is the tag serde wrote, so renaming a variant moves both ends together.
fn split(event: &RuntimeTransportEvent) -> Result<(String, Value), String> {
    let mut encoded =
        serde_json::to_value(event).map_err(|error| format!("encode failed: {error}"))?;
    let object = encoded.as_object_mut().ok_or_else(|| {
        "a transport event encoded to something that is not an object".to_string()
    })?;
    let event_type = match object.remove("event_type") {
        Some(Value::String(event_type)) => event_type,
        Some(other) => return Err(format!("the `event_type` tag encoded as {other}")),
        None => return Err("the encoded event carries no `event_type` tag".to_string()),
    };
    // `Null` rather than an error for a missing content field: adjacent tagging
    // omits it for a unit variant. This enum has none today, and if one is ever
    // added a `null` payload rejoins into it correctly rather than failing here.
    let payload = object.remove("data").unwrap_or(Value::Null);
    Ok((event_type, payload))
}

/// Put a journalled event back together into the value a transport carries.
///
/// The inverse of [`split`], and the input half of the `EventSink` that
/// `ProjectorCursor::project` needs. It lives beside `split` so the two cannot
/// disagree about the key names.
///
/// The sink exists: `driver_worker::HostEventSink::emit` is built by
/// `project_outbox` and calls this for every record it walks. The trait remains
/// safely disabled by default, while the stateless production host explicitly
/// opts in and implements both routed and named delivery. The separate
/// in-process rollback driver has no durable drain and retains inline delivery.
///
/// # THIS RESTORES THE EVENT AND NOT WHAT WAS DONE WITH IT
///
/// Stated here because "rejoin" reads like the whole inverse of the record, and
/// it is only the inverse of [`split`]. The value this returns is what the
/// transport carried; whether that value was a **canonical runtime fact** or a
/// transport-only send is `JournalBody::Event`'s third field, which `split` never
/// produced and this never consumes.
///
/// A sink built on this alone is therefore complete for delivery and silent
/// about persistence: every live surface keeps working and the persisted
/// runtime-fact stream stops, with nothing looking wrong. The routing reaches a
/// sink through `ProjectedEventSink::emit_routed`, beside the call to this — see
/// [`routing_for`] and the module docs.
pub(in crate::magician_v2::execution::agentic) fn rejoin(
    event_type: &str,
    payload: Value,
) -> Result<RuntimeTransportEvent, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "event_type": event_type,
        "data": payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Not in the module's own imports: production reaches the mapper through
    // `canonical_runtime_fact_of`, so a module-level import would be unused on
    // the lib target and kept alive only by this module — which is how it read
    // until 2026-08-28.
    use crate::magician_v2::artifact_v2::map_v2_realtime_event;
    use crate::magician_v2::execution::agentic::run_loop::journal::{JournalBody, JournalRecord};

    /// A recorded scope with all four fields distinct.
    ///
    /// Distinct on purpose: a fixture repeating one string would pass a producer
    /// that put `workspace` where `task_id` goes, which is the mistake four
    /// same-typed strings invite.
    fn a_recorded_scope() -> RecordedCanonicalScope {
        RecordedCanonicalScope {
            principal: "principal-p".to_string(),
            workspace: "workspace-w".to_string(),
            task_id: "task-t".to_string(),
            ui_thread_id: "thread-u".to_string(),
        }
    }

    /// The live scope a run's `ActionExecutors` would carry, for the execution
    /// id given.
    ///
    /// Built from [`a_recorded_scope`] so the two cannot disagree about the four
    /// names — a fixture pair that spelled them independently would let a
    /// producer transpose two fields and still match.
    fn a_live_scope(execution_id: &str) -> CanonicalEventScope {
        let recorded = a_recorded_scope();
        CanonicalEventScope {
            principal: recorded.principal,
            workspace: recorded.workspace,
            task_id: recorded.task_id,
            execution_id: execution_id.to_string(),
            ui_thread_id: recorded.ui_thread_id,
        }
    }

    /// A small, real event — the one `phases::prepare` emits — rather than a
    /// fixture shaped for the assertion. Every test here is about what
    /// `RuntimeTransportEvent`'s own serde attributes do, so a stand-in type
    /// would test nothing.
    fn iteration_started(execution_id: &str, scoped: bool) -> RuntimeTransportEvent {
        RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: execution_id.to_string(),
            principal: scoped.then(|| "p".to_string()),
            workspace: scoped.then(|| "w".to_string()),
            plan_id: "plan".to_string(),
            step_id: "step".to_string(),
            iteration: 3,
            environment_type: "shell".to_string(),
            timestamp: 1_700_000_000_000,
        }
    }

    /// The same real event, carrying a field big enough to put the record over
    /// [`MAX_JOURNAL_RECORD_BYTES`].
    ///
    /// A `RuntimeTransportEvent` and not a hand-built body, because the question
    /// under test is what the **producer** does with an event it cannot journal,
    /// and a producer only ever sees events.
    fn oversized_iteration_started(execution_id: &str) -> RuntimeTransportEvent {
        RuntimeTransportEvent::AgenticIterationStarted {
            execution_id: execution_id.to_string(),
            principal: Some("p".to_string()),
            workspace: Some("w".to_string()),
            plan_id: "plan".to_string(),
            step_id: "step".to_string(),
            iteration: 3,
            environment_type: "x".repeat(MAX_JOURNAL_RECORD_BYTES),
            timestamp: 1_700_000_000_000,
        }
    }

    #[test]
    fn the_split_takes_the_tag_serde_wrote_and_leaves_the_rest_alone() {
        let event = iteration_started("exec-split", true);
        let (event_type, payload) = split(&event).expect("a struct variant splits");

        // Compared against the encoding itself, not against a constant declared
        // here: the claim is about `#[serde(tag = …, content = …)]` on somebody
        // else's type, and a local constant would agree with itself after that
        // attribute changed.
        let whole = serde_json::to_value(&event).expect("encode");
        assert_eq!(
            whole.get("event_type").and_then(Value::as_str),
            Some(event_type.as_str()),
            "the journalled event_type must be the tag serde wrote, not a re-derived name"
        );
        assert_eq!(
            whole.get("data"),
            Some(&payload),
            "the journalled payload must be the content serde wrote, byte for byte"
        );
    }

    #[test]
    fn a_journalled_event_rejoins_into_the_same_wire_value() {
        let event = iteration_started("exec-round", true);
        let before = serde_json::to_value(&event).expect("encode");

        let (event_type, payload) = split(&event).expect("split");
        let restored = rejoin(&event_type, payload).expect("rejoin");

        // `RuntimeTransportEvent` derives no `PartialEq`, and comparing the
        // encodings is the stronger claim anyway: what a projector's sink hands
        // a transport must be the value the inline emit would have handed it.
        assert_eq!(
            serde_json::to_value(&restored).expect("re-encode"),
            before,
            "a round trip through the journal must not change what reaches the transport"
        );
    }

    #[test]
    fn an_event_whose_optional_scope_is_absent_still_rejoins() {
        // REGRESSION GUARD for the serde `Option` trap, in the direction that
        // matters here. `principal` and `workspace` carry
        // `skip_serializing_if = "Option::is_none"` and **no**
        // `#[serde(default)]`, so a `None` is omitted from the wire entirely and
        // only serde's `Option` special case brings it back. If that stopped
        // holding, every unscoped event would fail to rejoin — and it would fail
        // in the projector, one process away from the producer that wrote it.
        let event = iteration_started("exec-unscoped", false);
        let (event_type, payload) = split(&event).expect("split");
        assert!(
            payload.get("principal").is_none(),
            "the fixture must actually exercise an omitted field, or this test proves nothing"
        );

        let restored = rejoin(&event_type, payload).expect("an unscoped event must rejoin");
        match restored {
            RuntimeTransportEvent::AgenticIterationStarted {
                principal,
                workspace,
                ..
            } => {
                assert!(principal.is_none());
                assert!(workspace.is_none());
            },
            other => panic!("rejoined as the wrong variant: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // The routing: what `emit_event` decides, and what the record remembers
    // ---------------------------------------------------------------------

    #[test]
    fn a_runs_own_event_is_recorded_as_a_canonical_runtime_fact_carrying_its_scope() {
        let execution_id = "exec-own";
        let scope = a_live_scope(execution_id);
        let event = iteration_started(execution_id, true);

        // The mapping has to cover this variant, or the assertion below would be
        // green for the unmapped reason rather than the matching-id one.
        assert!(
            map_v2_realtime_event(&event).is_some(),
            "the fixture must be an event the canonical mapping covers, or this case is about \
             the wrong branch"
        );

        assert_eq!(
            routing_for(execution_id, Some(&scope), &event),
            RecordedEventRouting::CanonicalRuntimeFact {
                scope: a_recorded_scope()
            },
            "an event naming the run's own execution is a runtime fact, and the record must \
             carry the scope a foreign process cannot look up"
        );
    }

    #[test]
    fn the_same_variant_for_another_execution_is_recorded_transport_only() {
        // THE CASE THAT SAYS THE DECISION IS NOT A PROPERTY OF THE VARIANT.
        //
        // One `RuntimeTransportEvent` variant, two answers, and nothing about
        // the value distinguishes them except whose execution it names. A
        // projector that switched on `event_type` — the obvious shortcut, and
        // the one that reads as sufficient — would take both of these down one
        // branch and be wrong for exactly the events a parent carries on behalf
        // of a child.
        let scope = a_live_scope("exec-parent");
        let childs_event = iteration_started("exec-child", true);
        let own_event = iteration_started("exec-parent", true);

        assert_eq!(
            split(&childs_event).expect("split").0,
            split(&own_event).expect("split").0,
            "the two fixtures must be the SAME variant, or this case proves nothing about the \
             variant not being enough"
        );

        assert_eq!(
            routing_for("exec-parent", Some(&scope), &childs_event),
            RecordedEventRouting::TransportOnly,
            "another run's event must not be persisted as this run's runtime fact"
        );
        assert_eq!(
            routing_for("exec-parent", Some(&scope), &own_event),
            RecordedEventRouting::CanonicalRuntimeFact {
                scope: a_recorded_scope()
            },
            "and the same variant for this run's own execution must be"
        );
    }

    #[test]
    fn a_parents_fact_journalled_under_another_runs_id_records_no_decision() {
        // NOT THE SUB-GOAL CASE, and an earlier version of this comment said it
        // was. A sub-goal runs under the PARENT's execution id on purpose
        // (`run_single_delegate_in_context`, `handle_spawn_sub_goal_decision`), so it
        // never produces the pair below.
        // See `routing_for`'s *NO PRODUCTION PATH REACHES THAT ARM TODAY*.
        //
        // What this pins is the CLASSIFIER: given a scope that names a
        // different execution than the RUN whose journal the record goes in — not
        // than the journal FILE, which is a loop-state address and legitimately
        // differs from the run's id — the answer is `Unrecorded` and not
        // `TransportOnly`, because the four recorded fields would come back
        // attached to a run they do not describe and `TransportOnly` would claim
        // a decision the producer never made.
        //
        // What production change leaves this green? **Every change to how
        // production pairs a scope with a journal id**, because the pair below
        // is hand-built and production cannot supply it. Only a change to
        // `routing_for` itself turns this red — dropping `journal_execution_id`
        // and trusting `scope.execution_id` does, and that is the whole of its
        // reach. The pairing invariant is checked by reading the two
        // `with_canonical_event_scope` sites and the writers of
        // `ctx.execution_id`; no test in this repo covers it.
        let parents_scope = a_live_scope("exec-parent");
        let parents_event = iteration_started("exec-parent", true);

        // Under the parent's own journal it is a fact, so the fixture is not one
        // that would be `Unrecorded` for some other reason.
        assert_eq!(
            routing_for("exec-parent", Some(&parents_scope), &parents_event),
            RecordedEventRouting::CanonicalRuntimeFact {
                scope: a_recorded_scope()
            },
            "the same scope and the same event under the parent's own key must still be a fact, \
             or this case is about a broken fixture rather than about the id it is journalled \
             under"
        );

        assert_eq!(
            routing_for("exec-child", Some(&parents_scope), &parents_event),
            RecordedEventRouting::Unrecorded,
            "a scope that names a run other than the one this record is journalled under has no \
             honest fifth field to be rebuilt with, so the record must say it cannot say — never \
             `TransportOnly`, which would claim a decision nobody made and downgrade a real \
             runtime fact in silence"
        );
    }

    #[test]
    fn a_segments_fact_is_recorded_under_the_runs_id_and_refused_under_its_file_key() {
        // THE LOOSENING THIS ARM MUST NOT TAKE, and the sibling case above
        // cannot see it. That case pairs `exec-parent` with `exec-child`, and
        // NEITHER is an extension of the other. So a comparison "repaired" to
        // tolerate the journal FILE's suffixes — `starts_with`, or a strip of a
        // trailing `-n{…}` / `-r{n}` / `-p{n}` — leaves that pair answering
        // `Unrecorded` exactly as it does now, stays green, and silently changes
        // the answer for every nested, resumed and refinement-pass run. The pair
        // below is the pair that moves under both.
        //
        // Not every botched repair hides there: a split on the first `-` folds
        // `exec-parent` and `exec-child` onto the same `exec` and turns the
        // sibling red. This case is for the ones that do not.
        //
        // Why that loosening gets proposed: `journal` resolves ONE id and hands
        // it to two consumers. `routing_for` gets the RUN's id and wants it;
        // `journal_for` gets the same string as a buffer key and wants a
        // segment-unique one. A reader who notices the second and reaches for
        // `executor.rs::loop_state_address` will be looking at this comparison
        // when they do. The folded key belongs on the buffer, never here — see
        // `journal`'s own comment, and *IT IS NOT THE FILE'S KEY* above.
        //
        // WHAT PRODUCTION CHANGE LEAVES THIS GREEN? A change at the RESOLUTION
        // site — `journal` passing a folded address into `routing_for` — does,
        // for the same reason the sibling case above confesses: this drives
        // `routing_for` directly and cannot see its caller. This pins the
        // classifier's half only, and that is the half a "fix" lands in first.
        let scope = a_live_scope("exec");
        let event = iteration_started("exec", true);

        assert_eq!(
            routing_for("exec", Some(&scope), &event),
            RecordedEventRouting::CanonicalRuntimeFact {
                scope: a_recorded_scope()
            },
            "a nested, resumed or refinement-pass segment journals under the execution it shares, \
             and its events are canonical runtime facts of that execution"
        );

        // `-n3` is what `executor.rs::loop_state_execution_id` appends for a
        // one-frame nesting chain. Spelled as a literal on purpose:
        // `loop_state_address` is private to that module, and re-deriving it
        // here would be a second spelling of one key — the shape of bug this
        // area already has one of.
        assert_eq!(
            routing_for("exec-n3", Some(&scope), &event),
            RecordedEventRouting::Unrecorded,
            "and the file's key is not an id this comparison may accept: tolerating it is how a \
             scope silently becomes rebuildable against a string no execution was registered \
             under, which is the failure `Unrecorded` exists to name"
        );
    }

    #[test]
    fn a_run_with_no_canonical_scope_records_every_event_transport_only() {
        // `emit_event`'s `unwrap_or(false)`, from this side. A run whose scope
        // was never registered has no canonical identity to persist under, and
        // the record has to say so rather than leaving a reader to guess from a
        // missing field.
        let event = iteration_started("exec-unscoped", true);
        assert!(
            map_v2_realtime_event(&event).is_some(),
            "the fixture must be mapped, or the absence of a scope is not what is being tested"
        );
        assert_eq!(
            routing_for("exec-unscoped", None, &event),
            RecordedEventRouting::TransportOnly
        );
    }

    #[test]
    fn an_event_the_canonical_mapping_does_not_cover_is_transport_only() {
        // The one part of the decision that IS a function of the variant, pinned
        // so the claim in `routing_for`'s docs is checked rather than
        // asserted. `Heartbeat` is one of the 53 variants `map_v2_realtime_event`
        // answers `None` for; it carries no execution id at all, so there is
        // nothing it could ever be a runtime fact OF.
        let event = RuntimeTransportEvent::Heartbeat {
            timestamp: 1_700_000_000_000,
        };
        assert!(
            map_v2_realtime_event(&event).is_none(),
            "the fixture must be an UNMAPPED variant, or this case duplicates the one above"
        );
        // With a scope present, so the `TransportOnly` result is attributable to
        // the mapping rather than to a missing scope.
        assert_eq!(
            routing_for("exec-any", Some(&a_live_scope("exec-any")), &event),
            RecordedEventRouting::TransportOnly
        );
    }

    #[test]
    fn the_recorded_scope_does_not_carry_an_execution_id() {
        // A FORMAT LOCK, and it is not a style preference: `RecordedCanonicalScope`
        // is durable, `Journal::parse` turns one unreadable record into a
        // whole-file `CorruptRecord`, and a field added here without
        // `#[serde(default)]` quarantines every run whose journal predates it.
        // So a fifth field must be a decision somebody made on purpose.
        //
        // WHAT IT DOES NOT ASSERT, corrected 2026-08-28: that the file key is a
        // source for the id. It is not — it is a loop-state address, and for a
        // resumed, nested or refinement-pass run it names no execution at all.
        // The earlier rationale here said "the journal file already names the
        // run", which is the same false premise the type docs used to carry.
        // The id is omitted because it is a run-level constant, not because the
        // file supplies it.
        //
        // Adding `execution_id` back is therefore a live candidate rather than a
        // thing to refuse: it is one of the three ways to give a reader an
        // honest fifth field (the others are `store::ExecutionKey` carrying the
        // run's id beside its address, and `driver_worker::project_outbox` being
        // handed it). Whoever takes that route updates this case, gives the field
        // `#[serde(default)]`, and updates the two literals in
        // `store/mod.rs` (`:2969`, `:3006`) that this crate's shared store
        // conformance suite builds by hand.
        //
        // Asserted on the ENCODING rather than by counting struct fields,
        // because the encoding is what a foreign process reads.
        let encoded = serde_json::to_value(a_recorded_scope()).expect("a scope must encode");
        let object = encoded.as_object().expect("a scope encodes to an object");
        assert!(
            !object.contains_key("execution_id"),
            "the run id is not written per record; adding it is a deliberate format change and \
             every reader of an older journal has to survive it: {encoded}"
        );
        assert_eq!(
            object.len(),
            4,
            "four fields, and a fifth added here must come with `#[serde(default)]` and with \
             the store conformance suite's two literals updated: {encoded}"
        );
    }

    #[test]
    fn a_produced_record_is_one_the_store_would_accept() {
        // The producer's whole contract with the store: a record it hands over
        // cannot be one the store refuses, because a refused append fails the
        // commit and refuses the boundary.
        //
        // Two claims, and the stamps matter. An earlier cut of this test rebuilt
        // the record with `seq: 1`, `at_ms: 0`, the caller's `iteration` and
        // `phase`, and `ordinal: 0`, then asserted `to_line()`. That assertion
        // could not fire: `JournalAppend::event`'s own probe had already run
        // `to_line()` on the SAME body with `seq: u64::MAX`, `at_ms: i64::MIN`,
        // `iteration: usize::MAX`, the widest `Phase` and `ordinal: u32::MAX` —
        // every field strictly wider — and `to_line` only bounds-checks,
        // encodes and measures. Given the first assert passed, the second could
        // not fail for any body.
        let execution_id = "exec-accept";
        take(execution_id);
        let event = iteration_started(execution_id, true);
        // A REAL routing, not `Unrecorded`. The claim below is that the body in
        // the buffer is the body the producer built, and a routing the record
        // could have defaulted to would leave that claim green for a producer
        // that dropped the field on the way into `JournalAppend::event`.
        let routing = RecordedEventRouting::CanonicalRuntimeFact {
            scope: a_recorded_scope(),
        };
        assert!(journal_for(
            execution_id,
            7,
            Phase::Prepare,
            &event,
            routing.clone()
        ));

        let produced = take(execution_id);
        assert_eq!(produced.len(), 1);
        let append = produced.into_iter().next().expect("one record");

        // 1. The body that reached the buffer is the body `split` produced —
        //    the assertion with teeth, because it is the one a producer that
        //    truncated a payload, rewrote it, or hand-built a `JournalAppend`
        //    around the size probe would fail. `JournalAppend`'s fields are
        //    `pub`, so bypassing the probe is one struct literal away.
        let (event_type, payload) = split(&event).expect("split");
        assert_eq!(
            append.body,
            JournalBody::Event {
                event_type,
                payload,
                routing
            },
            "the buffered body must be the event's own encoding and the producer's own routing, \
             unmodified after the probe"
        );

        // 2. And it survives every stamp the driver and the store can put on it.
        //    `commit_boundary` re-stamps `iteration`, `phase` and `ordinal`; the
        //    store assigns `seq` and `at_ms`. These are the widest values any of
        //    them can write, and the loop over `Phase::ORDER` means a phase
        //    added later is covered without this test naming a widest one.
        for phase in Phase::ORDER {
            JournalRecord {
                seq: u64::MAX,
                iteration: usize::MAX,
                phase,
                ordinal: u32::MAX,
                at_ms: i64::MIN,
                body: append.body.clone(),
            }
            .to_line()
            .expect("the store's own writer must accept what the producer built");
        }
    }

    #[test]
    fn an_oversized_event_is_refused_at_the_producer_rather_than_at_the_store() {
        // Through the producer, not through `JournalAppend::event`. The
        // behaviour under test is the producer's ANSWER to the refusal — drop
        // the record, count it, leave the records already held alone, and let
        // the event still be emitted — and none of that lives in the
        // constructor. An assertion that only called the constructor stayed
        // green through: making `store` panic on `Err`, making it push the
        // record anyway, making `journal_for` propagate rather than warn and
        // continue, or deleting the warning. The first two are what this module's
        // longest doc section exists to argue against; the last two stall a live
        // run at the store or lose the operator's only signal.
        let execution_id = "exec-oversized";
        take(execution_id);

        // A held record first, so the assertion below tells "the refusal left
        // the buffer alone" apart from "the buffer happened to be empty".
        assert!(journal_for(
            execution_id,
            1,
            Phase::Prepare,
            &iteration_started(execution_id, true),
            RecordedEventRouting::Unrecorded
        ));

        let oversized = oversized_iteration_started(execution_id);
        let (event_type, payload) = split(&oversized).expect("a struct variant splits");
        assert!(
            JournalAppend::event(
                1,
                Phase::Decide,
                event_type,
                payload,
                RecordedEventRouting::Unrecorded
            )
            .is_err(),
            "the fixture must actually be over the ceiling, or this test proves nothing"
        );

        assert!(
            !journal_for(
                execution_id,
                2,
                Phase::Decide,
                &oversized,
                RecordedEventRouting::Unrecorded
            ),
            "the refusal must land in the producer, where the answer is to drop the record, \
             rather than in the store's batch encode, where the answer would be to stall the run"
        );

        let drained = take(execution_id);
        assert_eq!(
            drained.len(),
            1,
            "a refused record must not reach the buffer and must not disturb what is in it"
        );
        assert_eq!(drained[0].iteration, 1);
    }

    #[test]
    fn a_refused_event_is_counted_so_the_drain_learns_its_list_is_short() {
        // The two losses are the same loss to a projector. Before `refused`
        // existed, a run that lost ten events to the producer's size refusal
        // handed the drain a short list with nothing saying so, while a run that
        // lost one event to a full buffer did say so.
        let mut pending = Pending::default();

        let outcome = store_in(
            &mut pending,
            "exec-refused",
            Err("did not encode".to_string()),
        );

        assert!(matches!(outcome, Stored::NotJournalable { refused: 1, .. }));
        let run = pending
            .runs
            .get("exec-refused")
            .expect("the run is tracked");
        assert_eq!(run.refused, 1, "the loss is attributed to the run");
        assert_eq!(
            run.dropped, 0,
            "and NOT to `dropped`, whose `== 1` test fires the full-buffer warning exactly once \
             per run — a refusal counted there would consume that one firing"
        );
        // And the running total is CARRIED OUT, not merely accumulated. The
        // warning in `journal_for` fires on `refused == 1` and is silent after
        // it, which is impossible to do from the count left behind the `PENDING`
        // guard — a warning logged under that lock is the deadlock this module's
        // `Stored` enum exists to prevent. A change that reverted `Stored` to a
        // bare reason would compile, keep the counter above, and warn once per
        // event again.
        let second = store_in(
            &mut pending,
            "exec-refused",
            Err("did not encode either".to_string()),
        );
        match second {
            Stored::NotJournalable { refused, .. } => assert_eq!(
                refused, 2,
                "the second refusal must report the run's total, so the warning can stay quiet"
            ),
            other => panic!("a refusal must report as one: {other:?}"),
        }
    }

    #[test]
    fn every_driver_selects_exactly_one_outbox_delivery_path() {
        // The decision `journalling_has_a_drain` makes, without the process
        // environment it makes it from — see `arm_has_a_drain` for why the read
        // itself is left uncovered.
        //
        // What this does NOT cover, stated rather than implied: that `journal`
        // consults it at all. Deleting the gate from `journal` leaves this green.
        assert!(
            arm_has_a_drain(ExecutionDriver::Stateless),
            "the stateless arm builds `InProcessWorkerHost`, whose `run_phase` drains per phase"
        );
        assert!(
            !arm_has_a_drain(ExecutionDriver::Inprocess),
            "`driver_inproc::run_iteration` assembles no `PhaseReport`, so a record stored for a \
             run on that arm has no reader and `journal_and_emit` must deliver it inline"
        );
    }

    #[test]
    fn the_insert_that_reaches_the_cap_preserves_every_accepted_run() {
        let mut pending = Pending::default();
        let event = iteration_started("exec-protect", true);
        for index in 0..MAX_TRACKED_RUNS {
            let outcome = store_in(
                &mut pending,
                &format!("exec-protect-filler-{index}"),
                build(1, Phase::Decide, &event, RecordedEventRouting::Unrecorded),
            );
            assert!(matches!(outcome, Stored::Kept));
        }
        assert_eq!(
            pending.runs.len(),
            MAX_TRACKED_RUNS,
            "the fixture must sit exactly at the cap, or the refusal below proves nothing"
        );

        let outcome = store_in(
            &mut pending,
            "exec-protect-new",
            Err("did not encode".to_string()),
        );

        assert!(matches!(outcome, Stored::CapacityRefused { refused: 1 }));
        assert!(!pending.runs.contains_key("exec-protect-new"));
        assert_eq!(pending.runs.len(), MAX_TRACKED_RUNS);
    }

    #[test]
    fn the_buffer_keeps_the_order_the_phase_produced() {
        let execution_id = "exec-order";
        take(execution_id);

        for iteration in 1..=3usize {
            assert!(journal_for(
                execution_id,
                iteration,
                Phase::Decide,
                &iteration_started(execution_id, true),
                RecordedEventRouting::Unrecorded
            ));
        }

        let drained = take(execution_id);
        assert_eq!(
            drained
                .iter()
                .map(|record| record.iteration)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the projector dedupes on a position in this list, so a drain that reorders it \
             re-addresses every event in the batch"
        );
        assert!(
            take(execution_id).is_empty(),
            "a drain must not leave the records behind for a second drain to emit again"
        );
    }

    /// The id gate, watched with the drain gate forced open.
    ///
    /// **This is the case that can fail.** Its two siblings below drive
    /// [`journal`] and [`journal_named`] with an id-less context, but those
    /// siblings also depend on the process-grain driver cache. Only a caller
    /// that supplies the drain answer isolates this id check from that ambient
    /// selection and remains deterministic when another test seeded the cache.
    ///
    /// **What production change leaves this green?** None of the ones that
    /// matter. Deleting the `?` in [`addressable_execution_id`] fails the first
    /// assertion; swapping `runtime_execution_id_opt` for the total
    /// `runtime_execution_id`, which answers `""` for this context, fails it
    /// too — and `""` is exactly the address that would otherwise be written
    /// into a buffer key no drain ever asks for. Deleting the drain gate fails
    /// the third.
    #[test]
    fn a_context_with_no_execution_id_is_unaddressable_even_when_a_drain_would_take_it() {
        let ctx = AgenticContext::default();
        assert!(
            addressable_execution_id(&ctx, || true).is_none(),
            "a context with no execution id has no file to write to and no EventKey to address \
             by, so it must be unaddressable even on an arm that would drain the record"
        );

        // Positive control. Without it the assertion above passes for a helper
        // that answered `None` for everything, including the ordinary case.
        //
        // Struct-update rather than `default()` then assign: the latter is
        // `clippy::field_reassign_with_default`, and `make clippy` runs with
        // `-D warnings`.
        let addressed = AgenticContext {
            execution_id: Some("exec-addressable".to_string()),
            ..AgenticContext::default()
        };
        assert_eq!(
            addressable_execution_id(&addressed, || true),
            Some("exec-addressable"),
            "and an addressed context on a draining arm must be journalled"
        );

        // And the drain gate is still a gate, in the same helper, below the id.
        assert!(
            addressable_execution_id(&addressed, || false).is_none(),
            "an arm with no drain keeps nothing, so building a record for it is work for a \
             record nobody takes"
        );
    }

    #[test]
    fn a_context_with_no_execution_id_journals_nothing_and_does_not_panic() {
        // The `None` arm is ordinary, not exceptional — a delegated context can
        // legitimately reach a phase before an id is allocated — so it has to be
        // a return rather than a refusal a caller has to answer.
        //
        // WHAT THIS CATCHES, and it is less than its name suggests: a panic on
        // the entry point. `MAGICIAN_EXECUTION_DRIVER` is unset here, so
        // `journal` returns at the drain gate whether or not the id check above
        // it survives, and this case cannot see the difference. The id check
        // itself is covered by
        // `a_context_with_no_execution_id_is_unaddressable_even_when_a_drain_would_take_it`,
        // which forces the drain answer so the id gate is the only one left.
        let ctx = AgenticContext::default();
        assert!(
            runtime_execution_id_opt(&ctx).is_none(),
            "the fixture must actually exercise the unaddressed case"
        );
        assert!(!journal(
            &ctx,
            // A scope is passed, so a `journal` that returned early for the
            // WRONG reason — no scope rather than no execution id — would not
            // be covered by this case.
            Some(&a_live_scope("ignored")),
            1,
            Phase::Prepare,
            &iteration_started("ignored", true),
        ));
    }

    #[test]
    fn storing_a_new_run_is_what_triggers_the_run_cap() {
        // This exercises the production cap rather than a helper: deleting the
        // `is_new_run` guard leaves the map unbounded at
        // `MAX_TRACKED_RUNS × MAX_PENDING_BYTES_PER_RUN`.
        let mut pending = Pending::default();
        let event = iteration_started("exec-trigger", true);
        let mut refusals = 0usize;

        for index in 0..(MAX_TRACKED_RUNS + 5) {
            let outcome = store_in(
                &mut pending,
                &format!("exec-trigger-{index}"),
                build(1, Phase::Decide, &event, RecordedEventRouting::Unrecorded),
            );
            match outcome {
                Stored::Kept => {},
                Stored::CapacityRefused { .. } => refusals += 1,
                other => panic!("unexpected admission result: {other:?}"),
            }
            assert!(
                pending.runs.len() <= MAX_TRACKED_RUNS,
                "the map must remain within the cap when `store_in` returns"
            );
        }

        assert_eq!(
            refusals, 5,
            "each insert past the cap must use inline fallback"
        );
        assert!(pending.runs.contains_key("exec-trigger-0"));
        assert!(!pending
            .runs
            .contains_key(&format!("exec-trigger-{}", MAX_TRACKED_RUNS + 4)));
    }

    #[test]
    fn a_full_buffer_discards_the_new_record_rather_than_the_ones_already_held() {
        // A hole in the middle of the log is worse than a truncated tail: the
        // projector walks in seq order and dedupes by address, so the records
        // already produced keep their meaning only if the earliest ones stay.
        let execution_id = "exec-full";
        take(execution_id);
        {
            let mut pending = PENDING.lock().unwrap_or_else(|error| error.into_inner());
            let run = pending.runs.entry(execution_id.to_string()).or_default();
            run.bytes = MAX_PENDING_BYTES_PER_RUN;
            run.records.push(
                JournalAppend::event(
                    1,
                    Phase::Prepare,
                    "Held",
                    serde_json::json!({}),
                    RecordedEventRouting::Unrecorded,
                )
                .expect("small")
                .into(),
            );
        }

        assert!(
            !journal_for(
                execution_id,
                2,
                Phase::Decide,
                &iteration_started(execution_id, true),
                RecordedEventRouting::Unrecorded
            ),
            "a full buffer must refuse the new record"
        );

        let drained = take(execution_id);
        assert_eq!(
            drained.len(),
            1,
            "the record already held must survive the refusal"
        );
        assert_eq!(drained[0].iteration, 1);
    }

    // ========================================================================
    // Dispatch lanes: the order the parallel slice is addressed in
    // ========================================================================

    /// One transport-event record, labelled so a drained list can be read back
    /// as the order it came home in.
    ///
    /// Built through the same constructor [`build`] uses, so a fixture cannot
    /// journal a record the store would refuse and the byte charge is the one
    /// production would pay.
    fn a_labelled_record(label: &str) -> Result<(JournalAppend, usize), String> {
        JournalAppend::event_measured(
            4,
            Phase::Apply,
            label,
            serde_json::json!({ "label": label }),
            // These cases are about ordering, not routing.
            RecordedEventRouting::Unrecorded,
        )
        .map_err(|error| error.to_string())
    }

    /// The labels a drain handed back, in the order it handed them back.
    fn drained_labels(drained: Drained) -> Vec<String> {
        drained
            .records
            .into_iter()
            .map(|record| match record.body {
                JournalBody::Event { event_type, .. } => event_type,
                other => panic!("this fixture journals transport events only; got {other:?}"),
            })
            .collect()
    }

    /// Two attempts at one `Apply`, whose members finish in DIFFERENT orders,
    /// must address the same event at the same ordinal.
    ///
    /// # What this pins, and what it would take to make it green while broken
    ///
    /// `driver_worker::commit_boundary` stamps `ordinal: index` over the list
    /// [`take`] hands back, so a record's position in that list IS its journal
    /// address, and `journal::ProjectorCursor::project` dedupes a re-run on
    /// exactly `(iteration, phase, ordinal)`.
    ///
    /// `apply`'s parallel follow-up slice is a `join_all` over jobs on the
    /// scheduler-root lane, so the order its members' events reach this buffer
    /// is the order they FINISHED — real I/O timing, which a re-attempt does not
    /// reproduce. Two attempts is not a contrived pairing: a phase that ends on
    /// a resumable terminal re-runs at the same cursor.
    ///
    /// **Deleting the [`sort_dispatch_lanes`] call in [`take_in`] — the one-line
    /// revert to the addressing that shipped before 2026-08-29 — fails this
    /// case**, and fails it on the second attempt only, which is the shape of
    /// the live defect: the first attempt looks perfect.
    ///
    /// The consequence it stands for, since the inline emit was cut: the
    /// projector re-emits the record that misses its dedupe window and silently
    /// drops the one that collides, reporting `deduped: 1, emitted: 1`. One
    /// action is announced twice on chat activity and the deep-work panel, and
    /// another is not announced at all.
    ///
    /// The assertion is NOT "the drained order equals the member order" alone.
    /// That spelling would pass a build that sorted by something merely
    /// deterministic within one attempt — a hash of the payload, say — while
    /// still disagreeing between attempts. Both are asserted, and the
    /// cross-attempt one is the load-bearing half.
    #[tokio::test]
    async fn a_reattempt_addresses_a_parallel_members_event_at_the_same_ordinal() {
        /// One attempt: three members, opened in member order, buffering in the
        /// completion order given.
        async fn attempt(completion_order: [usize; 3]) -> Vec<String> {
            let mut pending = Pending::default();
            // OPENED IN MEMBER ORDER, before any member awaits — which is what
            // `execute_direct_path_on_scheduler_root` does under `join_all`,
            // and the reason the lane is taken synchronously there. Fresh lanes
            // per attempt, so the two attempts share no lane VALUE: only the
            // comparison between them may be relied on.
            let lanes: Vec<DispatchLane> = (0..3).map(|_| open_dispatch_lane()).collect();

            for member in completion_order {
                let produced = a_labelled_record(&format!("member.{member}.executed"));
                let outcome = DISPATCH_LANE
                    .scope(lanes[member], async {
                        store_in(&mut pending, "exec-parallel", produced)
                    })
                    .await;
                assert!(
                    matches!(outcome, Stored::Kept),
                    "the fixture must not lose a record to a bound; this case is about order"
                );
            }

            drained_labels(take_in(&mut pending, "exec-parallel"))
        }

        let member_order = vec![
            "member.0.executed".to_string(),
            "member.1.executed".to_string(),
            "member.2.executed".to_string(),
        ];

        let finished_in_order = attempt([0, 1, 2]).await;
        let finished_reversed = attempt([2, 0, 1]).await;

        assert_eq!(
            finished_in_order, member_order,
            "a batch whose members happened to finish in member order must be addressed in \
             member order"
        );
        assert_eq!(
            finished_reversed, member_order,
            "and so must the SAME batch when its members finish in a different order. The \
             members' completion order is I/O timing; the drained order is an address"
        );
        assert_eq!(
            finished_in_order, finished_reversed,
            "the two attempts disagree, so the event addressed `(4, Apply, k)` on one attempt is \
             addressed at a different ordinal on the other. `ProjectorCursor::project` dedupes \
             on `(iteration, phase, ordinal)`, so it re-emits the pair that misses its window \
             and silently drops the pair that collides — and since the inline emit was cut the \
             projector is the ONLY path to a transport, so that is a double emission on a live \
             surface rather than a duplicated log line"
        );
    }

    /// A record the phase emitted on its own stack is a WALL the sort may not
    /// reorder across.
    ///
    /// The sort exists to undo a race between dispatches. A record with no lane
    /// was not in that race: the phase emitted it inline, at a position both
    /// attempts already agree on. A drain that sorted the whole list by lane
    /// would hoist every laned record past it — re-addressing records that were
    /// never in question in order to fix records that were — so the walk in
    /// [`sort_dispatch_lanes`] sorts only within a contiguous laned run.
    ///
    /// The fixture is deliberately the pathological one: the LATER dispatch's
    /// record is buffered first, then the wall, then the EARLIER dispatch's. A
    /// whole-list sort answers `early, late, wall` or `early, wall, late`
    /// depending on how it treats `None`; both fail here.
    #[tokio::test]
    async fn the_drain_never_moves_a_record_the_phase_emitted_on_its_own_stack() {
        let mut pending = Pending::default();
        let early = open_dispatch_lane();
        let late = open_dispatch_lane();

        let produced = a_labelled_record("late.before.the.wall");
        let outcome = DISPATCH_LANE
            .scope(late, async {
                store_in(&mut pending, "exec-wall", produced)
            })
            .await;
        assert!(matches!(outcome, Stored::Kept));

        // No lane: this is the phase's own stack, which is where most of what
        // this buffer holds comes from.
        let outcome = store_in(
            &mut pending,
            "exec-wall",
            a_labelled_record("phase.own.stack"),
        );
        assert!(matches!(outcome, Stored::Kept));
        assert!(
            current_dispatch_lane().is_none(),
            "the scope above must not have leaked past the future it wrapped, or this fixture is \
             not testing what it says"
        );

        let produced = a_labelled_record("early.after.the.wall");
        let outcome = DISPATCH_LANE
            .scope(early, async {
                store_in(&mut pending, "exec-wall", produced)
            })
            .await;
        assert!(matches!(outcome, Stored::Kept));

        assert_eq!(
            drained_labels(take_in(&mut pending, "exec-wall")),
            vec![
                "late.before.the.wall".to_string(),
                "phase.own.stack".to_string(),
                "early.after.the.wall".to_string(),
            ],
            "the two laned records are separated by an unlaned one, so they were never in one \
             race and the drain must leave all three where the phase put them. A sort that \
             reached across the wall would re-address a record the phase emitted inline, which \
             is the failure this repair exists to prevent rather than one it may cause"
        );
    }

    /// The production wrapper is what puts a job inside its lane, and it is the
    /// half a refactor loses silently.
    ///
    /// `open_dispatch_lane` returning a fresh lane proves nothing on its own: if
    /// `execute_direct_path_on_scheduler_root` stopped wrapping the job future,
    /// every record would store `None`, every drain would keep completion order,
    /// and nothing would warn. This pins that [`in_dispatch_lane`] — the exact
    /// entry point that call site uses — establishes the scope for the future it
    /// is handed, and that the scope ends with it.
    #[tokio::test]
    async fn the_production_wrapper_puts_a_job_future_inside_its_lane() {
        let lane = open_dispatch_lane();

        // Annotated at the `let`, so the unsizing coercion happens where the
        // target type is written down rather than inside an inference the
        // production call site does not rely on either.
        let job: Pin<Box<dyn Future<Output = Option<DispatchLane>> + Send>> =
            Box::pin(async move { current_dispatch_lane() });
        let seen = in_dispatch_lane(lane, job).await;

        assert_eq!(
            seen,
            Some(lane),
            "a record produced anywhere under the job future must be able to name the dispatch \
             that produced it; without this the sort in the drain has nothing to sort by"
        );
        assert_eq!(
            current_dispatch_lane(),
            None,
            "and the scope must end with the job. A lane that outlived its dispatch would tag \
             the phase's own later emissions as if a dispatch had produced them, and the drain \
             would then be entitled to move them"
        );
    }

    // ========================================================================
    // The named rail
    // ========================================================================

    /// The agent id every named fixture carries.
    ///
    /// NOT `"__system__"` — the fallback production substitutes when a context
    /// has no agent — so a producer that dropped the field and re-derived the
    /// fallback fails rather than matching.
    const NAMED_AGENT: &str = "agent-named-rail";

    /// The payload of a real `plan.step.finished` call, at the size a real one
    /// is.
    fn a_named_payload(step_id: &str) -> Value {
        serde_json::json!({
            "execution_id": "exec-named",
            "task_id": "task-t",
            "plan_id": "plan",
            "step_id": step_id,
            "status": "completed",
            "finished_at": 1_700_000_000_000i64,
        })
    }

    /// The named record reaches the buffer with all five arguments intact.
    ///
    /// Through `journal_named_for` — the producer — rather than through
    /// `JournalAppend::named_event`, because the behaviour under test is what
    /// the PRODUCER puts in the buffer. A case that only called the constructor
    /// would stay green through a producer that built the record and then stored
    /// a different one, which is exactly the shape
    /// `an_oversized_event_is_refused_at_the_producer_rather_than_at_the_store`
    /// exists to rule out on the other rail.
    ///
    /// **What production change leaves this green?** One that changed a field
    /// this does not read, and there is none: all five are asserted, and the
    /// scope halves are `Some` with values that are not their serde default, so
    /// a producer that dropped them cannot round-trip `None` into `None`.
    #[test]
    fn a_named_call_reaches_the_buffer_with_all_five_arguments() {
        let execution_id = "exec-named-kept";
        take(execution_id);

        assert!(journal_named_for(
            execution_id,
            7,
            Phase::Prepare,
            "plan.step.finished",
            NAMED_AGENT,
            Some("principal-p"),
            Some("workspace-w"),
            &a_named_payload("s-1"),
        ));

        let produced = take(execution_id);
        assert_eq!(produced.len(), 1);
        let append = produced.into_iter().next().expect("one record");
        assert_eq!(append.iteration, 7);
        assert_eq!(append.phase, Phase::Prepare);
        match append.body {
            JournalBody::NamedEvent {
                name,
                agent_id,
                principal,
                workspace,
                payload,
            } => {
                assert_eq!(name, "plan.step.finished");
                assert_eq!(agent_id, NAMED_AGENT);
                assert_eq!(principal.as_deref(), Some("principal-p"));
                assert_eq!(workspace.as_deref(), Some("workspace-w"));
                assert_eq!(
                    payload,
                    a_named_payload("s-1"),
                    "the payload in the buffer must be the payload the producer was handed, \
                     whole: a producer that truncated or rewrote it would still be delivering \
                     the inline copy and nothing else would notice"
                );
            },
            other => panic!("got {other:?}"),
        }
    }

    /// An unscoped named call is buffered unscoped.
    ///
    /// The pair `(None, None)` is what `emit_scoped_or_unscoped` reads to choose
    /// `AgentEventEnvelope::new` over `new_scoped`, so a producer that
    /// substituted empty strings would make a replay send a scoped envelope the
    /// producer never sent — a different wire shape reaching a different set of
    /// subscriptions.
    #[test]
    fn a_named_call_with_no_scope_is_buffered_with_no_scope() {
        let execution_id = "exec-named-unscoped";
        take(execution_id);

        assert!(journal_named_for(
            execution_id,
            1,
            Phase::Prepare,
            "plan.step.started",
            NAMED_AGENT,
            None,
            None,
            &a_named_payload("s-1"),
        ));

        let produced = take(execution_id);
        match produced.into_iter().next().expect("one record").body {
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

    /// An oversized named payload is refused by the producer, not by the store.
    ///
    /// The same property `an_oversized_event_is_refused_at_the_producer_rather_
    /// than_at_the_store` pins for the transport rail, on this one — and it has
    /// to be pinned separately, because the two rails have two `build`
    /// functions and only a case that drives THIS one can say the refusal is
    /// wired up here.
    ///
    /// **What production change leaves this green?** Making `build_named` use a
    /// probe of its own that measured the caller's narrow `iteration`/`phase`
    /// would not: the fixture is over the ceiling by more than the 22 bytes that
    /// buys. What it catches is `build_named` skipping the constructor and
    /// building a `JournalBody` literal, which is one struct literal away —
    /// `JournalAppend`'s fields are `pub`.
    #[test]
    fn an_oversized_named_payload_is_refused_at_the_producer() {
        let execution_id = "exec-named-oversized";
        take(execution_id);

        // A held record first, so the assertion below tells "the refusal left
        // the buffer alone" apart from "the buffer happened to be empty".
        assert!(journal_named_for(
            execution_id,
            1,
            Phase::Prepare,
            "plan.step.started",
            NAMED_AGENT,
            Some("principal-p"),
            Some("workspace-w"),
            &a_named_payload("s-1"),
        ));

        let oversized = serde_json::json!({
            "blob": "x".repeat(MAX_JOURNAL_RECORD_BYTES),
        });
        assert!(
            JournalAppend::named_event(
                1,
                Phase::Decide,
                "tool.result.projected",
                NAMED_AGENT,
                Some("principal-p".to_string()),
                Some("workspace-w".to_string()),
                oversized.clone(),
            )
            .is_err(),
            "the fixture must actually be over the ceiling, or this test proves nothing"
        );

        assert!(
            !journal_named_for(
                execution_id,
                2,
                Phase::Decide,
                "tool.result.projected",
                NAMED_AGENT,
                Some("principal-p"),
                Some("workspace-w"),
                &oversized,
            ),
            "the refusal must land in the producer, where the answer is to drop the record and \
             keep emitting, rather than in the store's batch encode, where the answer would be \
             to fail the commit and stall the run"
        );

        let drained = take(execution_id);
        assert_eq!(
            drained.len(),
            1,
            "a refused record must not reach the buffer and must not disturb what is in it"
        );
        assert_eq!(drained[0].iteration, 1);
    }

    /// Both rails share one buffer and one order.
    ///
    /// The property the drain depends on: `commit_boundary` stamps ordinals by
    /// position in the list `take` returns, so a producer that kept the two
    /// rails in separate buffers — or that returned one rail's records ahead of
    /// the other's — would re-address every record in the batch.
    ///
    /// **What production change leaves this green?** One that changed the
    /// ordering rule inside a single rail; that is what
    /// `the_buffer_keeps_the_order_the_phase_produced` covers. This one catches
    /// a second buffer, which is the change adding a rail invites.
    #[test]
    fn both_rails_share_one_buffer_in_one_order() {
        let execution_id = "exec-two-rails";
        take(execution_id);

        // The real interleaving: `emit_step_events_if_signaled` emits
        // `AgenticStepStarted`, then `plan.step.started`, then
        // `AgenticStepCompleted`, then `plan.step.finished`.
        assert!(journal_for(
            execution_id,
            4,
            Phase::Prepare,
            &iteration_started(execution_id, true),
            RecordedEventRouting::Unrecorded,
        ));
        assert!(journal_named_for(
            execution_id,
            4,
            Phase::Prepare,
            "plan.step.started",
            NAMED_AGENT,
            Some("principal-p"),
            Some("workspace-w"),
            &a_named_payload("s-1"),
        ));
        assert!(journal_for(
            execution_id,
            4,
            Phase::Prepare,
            &iteration_started(execution_id, true),
            RecordedEventRouting::Unrecorded,
        ));
        assert!(journal_named_for(
            execution_id,
            4,
            Phase::Prepare,
            "plan.step.finished",
            NAMED_AGENT,
            Some("principal-p"),
            Some("workspace-w"),
            &a_named_payload("s-1"),
        ));

        let drained = take(execution_id);
        let rails: Vec<&'static str> = drained
            .iter()
            .map(|append| match &append.body {
                JournalBody::Event { .. } => "event",
                JournalBody::NamedEvent { .. } => "named",
                JournalBody::PhaseCompleted { .. } => "completed",
                JournalBody::RecoveryRewind { .. } => "recovery_rewind",
                JournalBody::OwnerTransition { .. } => "transition",
            })
            .collect();
        assert_eq!(
            rails,
            vec!["event", "named", "event", "named"],
            "one buffer, one order: the projector dedupes on a position in this list, so a \
             drain that grouped the rails would give every record after the first a different \
             address than the attempt before it produced"
        );
        assert!(
            take(execution_id).is_empty(),
            "a drain must not leave either rail's records behind for a second drain to emit again"
        );
    }

    /// An emit time a caller already chose is KEPT.
    ///
    /// The load-bearing half, and the reason this is a separate function rather
    /// than three lines inline. `emit_scoped_or_unscoped` runs the same
    /// `entry(..).or_insert_with(..)` a call later, so a value written here
    /// survives it and the record and the envelope carry ONE timestamp. Change
    /// this to an unconditional insert and the two diverge by however long the
    /// producer took — invisibly, because both values are plausible.
    ///
    /// **What production change leaves this green?** Only one that keeps the
    /// or-insert. An unconditional write fails the first assertion; dropping the
    /// stamp entirely fails the second.
    #[test]
    fn the_emit_time_stamp_is_or_insert_and_leaves_a_callers_own_value_alone() {
        // Not the current clock, and not zero: a sentinel a re-stamp could not
        // coincidentally produce.
        let mut chosen = serde_json::json!({ "step_id": "s-1", "timestamp_ms": 1i64 });
        stamp_emit_time(&mut chosen);
        assert_eq!(
            chosen["timestamp_ms"], 1,
            "a caller's own emit time must survive: the broadcaster's own stamp is an \
             or-insert, so an unconditional write here is what makes the record and the \
             envelope disagree"
        );

        let mut bare = serde_json::json!({ "step_id": "s-1" });
        stamp_emit_time(&mut bare);
        assert!(
            bare["timestamp_ms"].as_i64().is_some_and(|ms| ms > 0),
            "and a payload with none must get one, or a replayed record is stamped at REPLAY \
             time and the timeline shows the step finishing when the outbox drained: {bare}"
        );

        // A non-object payload is left alone rather than wrapped. The
        // broadcaster does the same; a wrapper here would change the shape the
        // transport carries.
        let mut not_an_object = serde_json::json!("just a string");
        stamp_emit_time(&mut not_an_object);
        assert_eq!(not_an_object, serde_json::json!("just a string"));
    }

    /// A context with no execution id journals no named record and does not
    /// panic.
    ///
    /// The mirror of `a_context_with_no_execution_id_journals_nothing_and_does_
    /// not_panic` on this rail, and it catches exactly as much: a panic on this
    /// entry point. It does **not** catch a lost id check. Both rails now take
    /// the same [`addressable_execution_id`], and under `cargo test` the drain
    /// gate inside it returns first, so nothing observable changes if the id
    /// half goes. `a_context_with_no_execution_id_is_unaddressable_even_when_a_
    /// drain_would_take_it` is where that half is actually watched — one case,
    /// because there is now one implementation for it to watch.
    ///
    /// Kept as its own case anyway: this rail reaches the gates through
    /// [`stamp_emit_time`] and a different signature, and a panic in either is
    /// this case's to find.
    #[test]
    fn a_named_call_with_no_execution_id_journals_nothing_and_does_not_panic() {
        let ctx = AgenticContext::default();
        assert!(
            runtime_execution_id_opt(&ctx).is_none(),
            "the fixture must actually exercise the unaddressed case"
        );
        assert!(!journal_named(
            &ctx,
            1,
            Phase::Prepare,
            "plan.step.started",
            NAMED_AGENT,
            Some("principal-p"),
            Some("workspace-w"),
            &a_named_payload("s-1"),
        ));
    }
}
