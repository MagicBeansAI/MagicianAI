//! Finds runs that are parked with nothing left to wake them, and says so.
//!
//! # The row this closes, and the half of it that was missing
//!
//! `docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Error handling*:
//!
//! > **Parent parked with no live child** — the `WaitingChildren` hang. Parking
//! > on `Children` requires at least one live child; already-complete children
//! > are consumed as results. **A reconciler detects orphaned parks rather than
//! > a per-poll scan.**
//!
//! The first sentence is built: [`super::state::WaitReason::children`] refuses to
//! construct a park with no children, and
//! [`super::driver_worker::Refusal::ParkWithNoLiveChild`] refuses to commit one.
//! Both act on the *shape* of the park at the moment it is made.
//!
//! Neither can see the case the row is actually about. A park whose children
//! were **already complete when it was made** is well-shaped — a non-empty list
//! of real execution ids — and no check available at commit time can tell it
//! from a park on children that are still running, because answering that
//! question means reading other executions' journals. This module is where that
//! read happens, on a cadence, after the fact.
//!
//! # What it recovers, and the three recoveries it still refuses
//!
//! A pass **retires** a park it has proved unwakeable: it appends a
//! non-resumable terminal to the run's own journal and commits the state with
//! the wait cleared, under the lease. That is one shape, applied to two of the
//! three [`ParkVerdict::Unwakeable`] grounds and to nothing else — never to a
//! [`ParkVerdict::Stalled`] row, however old. [`Recovery`] is where a caller
//! turns it on and [`LoopReconciler::retire_under_lease`] is the sequence.
//!
//! The three recoveries below were each considered and are each still refused.
//! They are kept because two of them are the obvious things to reach for and the
//! reasons have not stopped being true — what changed is that a fourth shape,
//! *retire*, is none of them.
//!
//! - **Clear the wait and let the run go again.** The run re-enters the phase
//!   whose boundary parked it, with the same committed state and the same
//!   children, and there is nothing to stop it reaching the same conclusion and
//!   parking again on the same token. **Nothing bounds that cycle.**
//!   [`super::state::LoopState::phase_attempts`] does not: it counts phases that
//!   *failed*, incremented only on `commit_failed_attempt`'s error path and
//!   reset by every successful boundary — and a park is a successful boundary.
//!   Nothing in the state records that a reconciler already un-parked this run
//!   once, so there is no counter to exhaust and no backoff to grow. Whether the
//!   phase would in fact re-park has not been traced through `phases::apply` and
//!   is not claimed here; what is claimed is the part that matters for the
//!   decision — if it does, the run un-parks and re-parks forever, hot, spending
//!   a claim and a commit each time. A silent hang is bad; a silent hot loop
//!   that also writes is worse. **Retiring is not this**: the wait is cleared
//!   *and* a non-resumable terminal goes into the log in the same round, and
//!   that commit publishes the store's ending marker — so no scan offers the
//!   key again at all, and a claim that arrives by some other route answers
//!   [`super::driver_worker::Advanced::RunAlreadyEnded`] before any phase is
//!   entered. Both halves close the cycle; the marker closes it a step earlier.
//!   The cycle this bullet describes needs a run that can still run a phase.
//! - **Mint an outcome for a run this process never executed.** An earlier
//!   version of this bullet said *failing the run* was blocked by two things:
//!   appending without the lease, and having to invent an `AgenticOutcome`. The
//!   first is real and is why retiring claims first. The second was **wrong on
//!   the code**, and the correction is what made retirement possible:
//!   [`JournalBody::PhaseCompleted`] carries a [`TerminalKind`] and nothing
//!   else, precisely because `AgenticOutcome` has no `Deserialize` — see that
//!   type's own docs. Recording *that a run ended and which way* needs no
//!   outcome. What is still refused is the other half: nothing here publishes an
//!   `AgenticOutcome` to a result path, so retiring records an ending and does
//!   **not** deliver one to a waiter. Under this composition there is no waiter
//!   — the delegating run was resumed under a different key long before — but a
//!   composition that grows one must give it its own answer rather than reading
//!   this record as delivery.
//! - **Resolve the wake token itself.** Still refused, and now for a sharper
//!   reason than before. `resolve_wake` is deliberately a lease-free write, so a
//!   reconciler *could* publish a synthetic resolution and let the parent wake,
//!   re-read its children and consume them as results. Two things are wrong with
//!   it. The published resolution is indistinguishable at the store from a real
//!   one, so a mistake leaves nothing to audit — and the mistake is reachable,
//!   because the judgement behind it is an inference over `load` and
//!   `read_journal` of each child, and a child slow to commit its first state
//!   reads as absent. **A wrong hang is visible and a wrong answer is not.** And
//!   waking a parent buys almost nothing under today's composition: production
//!   has no foreign [`super::worker_runner::HostFleet`]. Exact continuation is
//!   owned by the durable pause/handoff path; the boot lifecycle deliberately
//!   does not treat a synthetic wake as enough information to compose a host.
//!   Retiring writes into the run's **own** log, which no other writer is
//!   publishing to, and it moves the run to a state a claim refuses rather than
//!   to one a claim would try to run.
//!
//! ## What a retirement asserts, and what it does not
//!
//! It asserts exactly one thing: *this execution key ended and cannot come
//! back*. It does not assert that the work failed, that a person should be told,
//! or that nothing continued elsewhere — and the last of those matters, because
//! under today's composition **most parks are abandoned rather than hung**. A
//! delegating run is resumed through its pause record under
//! `{execution_id}-r{n}`, so the parked key holds a segment whose continuation
//! is alive somewhere this module cannot address. The journal is append-only, so
//! the retirement record does not erase the `RunEnded { WaitingForChildren }`
//! above it: the log reads *"this invocation ended waiting for children; later,
//! nothing could wake it"*, which is true of the key in both cases.
//!
//! **The one thing a retirement cannot make auditable from the store alone is
//! its own authorship** — still true on 2026-08-28, re-checked after that day's
//! journal changes, and the sentence that carried it was wrong about why.
//!
//! It said *"[`JournalBody`] has three variants and none carries a writer"*.
//! There are **four**: `PhaseCompleted`, `Event`, `NamedEvent` — added the same
//! week, for the `emit_named` rail — and `OwnerTransition`. The claim survives
//! the recount, but only because of what the fourth one actually holds:
//!
//! - A retirement appends exactly one record, `JournalAppend::phase_completed`
//!   with `RecordedStep::RunEnded { terminal }` — see
//!   [`LoopReconciler::retire_under_lease`]. `PhaseCompleted` carries a
//!   [`RecordedStep`] and nothing else, so a `CannotProceed` this module wrote
//!   is byte-identical in the log to one `phases::apply` wrote.
//! - `OwnerTransition` is the only variant that names an agent at all, and it
//!   names **who ends up holding the run**, not who wrote the record. Its
//!   `transition_authorization` field is documented as unpopulated — no writer
//!   sets it — so it would not close this even if a retirement appended one,
//!   which it does not.
//!
//! The audit trail is therefore still the `warn!` at [`push_finding`] and
//! [`ParkAction::Retired`] in the report. Closing it needs a field on the
//! record, which is `journal.rs`'s to add. **Re-check this claim by counting
//! `JournalBody`'s variants rather than by trusting the number in this
//! sentence** — the number is the part that went stale, the reasoning did not,
//! and the count is one grep.
//!
//! ## The second claimant, and why it is not a new strand
//!
//! A retirement takes a lease, so for as long as it holds one it is a **second
//! claimant** on a key. `executor.rs::StatelessArm` turns
//! [`super::driver_worker::Advanced::LeaseHeld`] into a hard error, so the
//! question is whether this can hand a live run one.
//!
//! It cannot, in this composition, and the reason is the arm's own shape rather
//! than luck: a run whose **committed** state is parked is one the arm has
//! already finished with. The park it takes arrives as `Advanced::RunEnded`
//! carrying `WaitingForChildren`, the arm returns it to
//! `execute_agentically_inner`, and the resume gets a different key. Only
//! `list_parked` rows are ever claimed here, and a row is there because the park
//! is committed.
//!
//! The residual case is a process restarting under the **same** key. That claim
//! races this one and may answer `LeaseHeld` instead of `Parked` — both of which
//! that arm turns into an error, so what changes is the message and not the
//! outcome. The lease is [`RETIREMENT_LEASE_TTL`] and the work under it is four
//! store calls, which is what keeps that window short.
//!
//! `worker_runner` is the other claimant and needs no argument: its `classify`
//! answers `LeaseHeld` with a holdoff rather than an error.
//!
//! # What one pass costs
//!
//! Stated concretely, because the design asks for detection that is *"cheap and
//! correct, not a per-poll scan"* and a cost claim is the only way to tell
//! whether that was achieved.
//!
//! | Per pass | Cost |
//! |---|---|
//! | **A.** Enumerate parked runs | one walk of every execution directory — the same walk `list_runnable` already does — plus one `stat` and one lease read per **parked** run |
//! | **B.** Parks younger than their grace | **nothing**: the age gate is decided from the listing alone, with no further store call |
//! | **C.** Parks past their deadline | **no diagnosis cost** — the verdict is decided from the listing alone — but **row E applies to every one of them under a [`Recovery::RetireProvedParks`] that names [`RetirableGround::DeadlinePassedWhileParked`]**, because the deadline ground is retirable and is judged *before* the age gate. Under [`Recovery::ReportOnly`], or under a recovery that did not name that ground, this row is free |
//! | **D.** Each park past the grace | one wake-ledger listing, plus one `load` and one journal read per named child **segment**, plus one chain-closure receipt read per child whose furthest committed segment ended `Success`, plus a second wake-ledger listing before any proof is issued |
//! | **E.** Each park **retired** | one claim, one `load`, one journal read, one projector-cursor read, one append, one commit, one release — and, on the children ground only, a third wake-ledger listing taken under the lease |
//!
//! Row **B** is what the age gate bought: every read costing more than a
//! directory entry sits behind it, so a store whose parks are few and young
//! costs one directory walk and nothing else. Row **C** is not that, and the
//! difference is the one an operator sizing a pass will get wrong: it says the
//! *detection* is free, not the pass. The deadline population is the one
//! guaranteed to be non-empty and to grow — it accumulates monotonically for the
//! reason *"Few and young is not this store"* gives below — so the first pass
//! after recovery is enabled pays row **E** for every row in it. Row **E** is
//! paid **once per park, ever** — the row it retires is gone from the next
//! listing — which is the whole reason it is worth paying.
//!
//! # "Few and young" is not this store, and retiring is what changes that
//!
//! Legacy `Children` parks that predate the exact source/successor address
//! remain until recovery proves and retires them, and neither store removes an
//! execution directory — there is no `remove_dir` anywhere in `store::fs`.
//! New V3 delegation checkpoints instead use [`prepare_children_handoff`]: the
//! lifecycle owner terminalizes the exact source segment, publishes a durable
//! readiness receipt, and later consumes it after dispatch ownership transfers.
//! Thus current parks do not accumulate on the healthy path, while a scope with
//! old parks can still reach the row-**D** steady-state cost below until those
//! rows are retired. Two consequences a reader budgeting from the table alone
//! would miss:
//!
//! - **A pass costs `n` diagnoses, not one walk.** At `n` aged parks that is `n`
//!   × (two wake-ledger listings + one `load` and one journal read per named
//!   child **segment**), plus one `warn!` per finding — `push_finding` logs
//!   every finding on every pass and nothing suppresses one already reported.
//!
//!   **Per segment, not per child**, and that multiplier arrived with the
//!   address. `child_state` walks the chain the park names, which for a
//!   delegated child is its pass-0 key plus one key per refinement pass
//!   `executor.rs` may commission — two at `MAX_REFINEMENT_PASSES = 1`. So the
//!   per-child bill doubled, bounded by that constant and by
//!   [`MAX_CHILDREN_DIAGNOSED`], and it is a bill for reading keys that mostly
//!   do not exist: a `load` that answers `None` is the common case and the walk
//!   stops nothing early because a gap is not the end of a chain. The
//!   alternative was not a cheaper walk — it was the false proof the chain
//!   exists to remove. A park committed before the address existed still costs
//!   one segment per child, which is what it cost before.
//! - **Past [`DEFAULT_SCAN_LIMIT`] the tail is late, never unreachable.**
//!   `scan_parked` stops at a bounded ordered position and returns it through
//!   [`ReconcileReport::resume`]. The production lifecycle retains that cursor
//!   in [`ReconcilerPolicy::scan_after`] and starts the next cadence strictly
//!   after it; reaching the end resets the following sweep to the beginning.
//!   `incomplete` still matters — it says this one report did not cover the
//!   whole store — but a stable prefix can no longer starve the tail forever.
//!
//! [`Recovery::RetireProvedParks`] reduces recurring diagnosis cost rather than
//! providing pagination. A retired park clears its `wait`, so later sweeps no
//! longer spend child/wake reads on it. Parks this module deliberately leaves
//! alone remain visible on each round-robin sweep: every `Stalled` row,
//! [`ParkReason::NoChildrenNamed`], and every retirable ground the active policy
//! did not name.
//!
//! ## What a retired run costs discovery, and the one write that decides it
//!
//! Clearing the wait moves the run out of `list_parked`. It does **not**, on its
//! own, move it into `list_runnable` — what stops it is a *second* write the
//! retiring commit performs, not anything the scan reads off the state.
//! `retire_under_lease` appends `RecordedStep::RunEnded { terminal }` —
//! `terminal_for` yields only [`TerminalKind::CannotProceed`], which
//! `TerminalKind::is_resumable` excludes — sets `state.journal_seq` to that
//! record's seq, and only then commits. Both stores derive the ending marker
//! *inside* `commit`, from the journal prefix the new watermark vouches for:
//! `store::non_resumable_terminal` over `journal.authoritative(state.journal_seq)`
//! in `store::fs`, over the same prefix of `slot.journal` in `store::memory`. So
//! the retiring commit publishes `ended = { seq: N, terminal: CannotProceed }`
//! with `N` equal to the watermark it just committed, and both scans withhold
//! exactly that shape — `ended.seq == state.journal_seq` skips the key. On that
//! path a retired run is never offered, never claimed, and never answered
//! [`super::driver_worker::Advanced::RunAlreadyEnded`].
//!
//! ### The filesystem marker is a revision-bound prepublication transaction
//!
//! `ended.json` is a second file, so `store::fs` cannot atomically swap it with
//! the snapshot. It closes that boundary by publishing a bounded marker with
//! the currently committed revision binding and the proposed next binding
//! before the snapshot CAS. A failed CAS or crash leaves the old binding
//! selectable; a successful CAS makes the new binding selectable without a
//! post-commit gap. Non-terminal revisions carry an explicit `None` tombstone,
//! so clearing an older ending has the same crash semantics as publishing one.
//! Marker publication failure refuses the commit before its snapshot is visible.
//! Readers load the snapshot first, select only its exact revision, and fail
//! closed on malformed/missing bindings. Bare `EndedRun` markers from older
//! builds remain readable, and the claim-time repair rewrites the exact current
//! revision from its authoritative journal prefix.
//!
//! What the marker does **not** cover is the reason retirement writes a
//! non-resumable terminal rather than a gentler one. A *resumable* terminal
//! publishes no marker at all, so a park retired to one would keep every
//! `runnable_at` test — claimable, unleased, unparked, inside-deadline,
//! past-retry — and be offered by every scan for the life of the store, with the
//! holdoff set as the only thing between that and a scan-rate spin.
//! `terminal_for` answering only `CannotProceed` is what keeps this module on
//! the covered side of that line, and it is a constraint on adding grounds to
//! [`RetirableGround`] rather than a coincidence — `terminal_for` is now a
//! projection of that type, so the constraint is discharged by
//! `RetirableGround::terminal` and a new ground has to answer it.
//!
//! The deadline ground is withheld twice over: `runnable_at` tests
//! `deadline_passed` and withholds the run whatever its wait or its marker says.
//!
//! **There is deliberately no parked index**, which would make the enumeration
//! `O(parked)` instead of `O(executions)`. An index has to be written on the
//! commit path, and that leaves two bad choices: fail the commit when the index
//! write fails — a new way to strand a run, for a reason the run does not care
//! about — or let it fail silently, which produces an index that quietly misses
//! entries and a reconciler reporting "nothing is wrong" about a store it did not
//! fully see. The scan is correct by construction, and bounded twice: by
//! `store::fs`'s `MAX_EXECUTIONS_SCANNED` on the walk, and by
//! [`DEFAULT_SCAN_LIMIT`] on the answer. The cursored lifecycle prevents either
//! ceiling from starving a stable tail, but it does not make a full sweep cheap:
//! an index remains the future cost optimisation to make, carrying a
//! rebuild-from-scan fallback rather than being the only source of truth.
//!
//! # Production composition and remaining boundaries
//!
//! - **WIRED as a process lifecycle.** `magician-bin/src/main.rs` owns a
//!   boot-immediate, thirty-minute interval independent of whether any new
//!   stateless execution arrives. It retains [`ReconcileReport::resume`] across
//!   calls, feeds it back through [`ReconcilerPolicy::scan_after`], and resets at
//!   the end of a sweep. The task selects on the supervisor shutdown token; a
//!   failed pass retains its cursor for retry. This keeps the `O(executions)`
//!   walk off a live run's critical path and makes the explicit `inprocess`
//!   rollback irrelevant to reconciliation availability.
//! - **Recovery is the caller's to switch on.** The production wiring names both
//!   retirable grounds — the expired-deadline one, and the completed-child one
//!   now that [`super::store::ChainClosure`] makes its proof a read rather than
//!   an inference; [`ReconcilerPolicy::default`] remains
//!   [`Recovery::ReportOnly`], because a retirement takes a lease, a lease needs a [`WorkerId`],
//!   and there is no honest default for one — a `Default` that minted an id would
//!   let a caller enable a lease-taking write by copying a struct literal it
//!   never read. So the worker travels **inside**
//!   [`Recovery::RetireProvedParks`], where recovery cannot be switched on
//!   without naming who does it — and, since the per-ground filter landed, the
//!   [`RetirableGround`] set travels inside the same variant for the same
//!   reason, so it cannot be switched on without naming *what* either. The
//!   worker travels as a [`ReconcilerWorker`]
//!   rather than a bare [`WorkerId`], because the variant constrains *whether*
//!   an id is named and only the newtype can constrain *which*. That
//!   distinction is the whole of [`ReconcilerWorker`]'s docs: the namespace
//!   keeps reconciler ownership recognisable in lease errors and operational
//!   records, while the store's fenced lease rules enforce exclusion.
//!
//!   The production caller names its identity with
//!   [`ReconcilerWorker::for_this_process`], which is
//!   `reconciler-<pid>-<instance nonce>`. Every call mints a fresh identity, so
//!   two reconciler instances in one process remain distinguishable and two
//!   containers that happen to reuse a pid do not alias. The lease store still
//!   supplies the actual safety boundary: every live lease refuses every new
//!   claim, including one presenting the same worker id, and only a fenced
//!   `renew` may extend it.
//! - **Children completion is wired as a terminal-first handoff.** The pause
//!   carries both its exact source segment and
//!   [`super::state::ResumeAddress::run_resumes_as`]. Once the lifecycle owner
//!   has authoritative child results, [`prepare_children_handoff`] verifies
//!   those addresses and the parked child multiset under a lease, requires any
//!   accepted outbox debt to be drained, commits `RunEnded { HandedOff }`, and only then calls
//!   `resolve_wake`. The wake is a readiness receipt for the pause-owned
//!   continuation, never permission to re-enter the old mid-phase cursor.
//!   Checkpoints written before either address existed stay on the conservative
//!   legacy path: `None` means *not stated*, and recovery may end only that old
//!   key, never claim the logical work failed.
//! - **External job completion still has no production producer.** The store
//!   vocabulary and `WaitReason::Job` remain bounded and tested, but no runtime
//!   path currently parks on a job or owns its completion callback. Wiring a
//!   resolver without that owner would manufacture evidence rather than bridge
//!   a real lifecycle.
//! - **A child that resumed is invisible, and reads as a live child.** The same
//!   "different execution key" applies to the *children*. Two axes move a child
//!   off its bare id and they are **not** symmetric:
//!   - `-r{n}`, a resume. Still not followed, and still not followable: the
//!     generations are strictly increasing and not contiguous, no parent can
//!     know at park time which one a child will take, and the store cannot
//!     enumerate by prefix. It costs a FALSE NEGATIVE — the segment that paused
//!     left a *resumable* terminal, which reads as live, so the park is left
//!     alone. That direction hides findings rather than inventing them.
//!   - `-p{n}`, a refinement pass. **This one is followed now.** It left a
//!     *non-resumable* terminal on the bare key while pass 1 did real work, so
//!     a stale read manufactured a proof rather than hiding one. The park
//!     carries [`super::state::ChildSegments`] — every key the child's work may
//!     be at, spelled by `executor.rs`'s `child_segment_addresses` — and
//!     `child_state` walks it. `diagnose_children` carries the full argument.
//!
//! # The assumption the CHILDREN proof rests on
//!
//! Only [`ParkReason::EveryChildHasFinished`] rests on it — the other two
//! [`ParkVerdict::Unwakeable`] grounds do not.
//! [`ParkReason::NoChildrenNamed`] is unwakeable by construction, and
//! [`ParkReason::DeadlinePassedWhileParked`] is about the run rather than the
//! token and reads no child at all. The verdict's own doc splits the three.
//!
//! `EveryChildHasFinished` is issued only when every named child has recorded a
//! terminal it cannot come back from **and** the wake ledger is empty. That is a
//! proof only if a completer resolves the wake within the grace period of the
//! child's own terminal record — otherwise a pass could look into the window
//! between the two. The window is a function call and the grace is minutes, so it
//! is not reachable in practice; it is nonetheless the assumption, which is why
//! the ledger is re-read **after** the children are judged and before any proof
//! is issued, so a completion landing during the diagnosis downgrades the verdict
//! instead of being raced. A pass that goes on to **retire** the park reads it a
//! third time, under the lease, immediately before the append — see
//! [`LoopReconciler::retire_under_lease`].
//!
//! **There was a second assumption of the same class, and it is now a read
//! rather than an assumption.** A child that ended `Success` used to be proved
//! finished when every address a refinement pass could run under was read and
//! was empty. The window that made that wrong is the stretch between pass 0's
//! terminal commit and pass 1's first commit — `build_run_setup` and
//! successor-pass seeding, seconds rather than a function call — during which
//! the successor address is empty because the pass has not started writing.
//! Unlike the ledger window above, no amount of re-reading closes it: the thing
//! that would distinguish the two cases is not on disk.
//!
//! So it is put on disk. `executor.rs`'s `execute_agentically_with_refinement`
//! publishes a [`super::store::ChainClosure`] for the segment it finished on,
//! once it has decided no further pass will be commissioned, and
//! [`LoopReconciler::child_state`] proves a `Success` child finished only when
//! it read one. **Absence is never evidence**: a crash before the receipt, a
//! store that publishes none, and a chain that is genuinely still open are one
//! answer — [`ParkReason::ChildChainNotClosed`], a suspicion — so the receipt
//! can only ever turn a refusal into a proof and never the other way round.
//!
//! # Which grounds are acted on, one paragraph each
//!
//! Three grounds reach [`ParkVerdict::Unwakeable`] and **two** of them are
//! retirable. The line between them is not how strong the proof is; it is
//! whether the state was produced by a writer this module can reason about.
//!
//! *Retirable* is not *retired*, and the gap is a policy. Which of the two a
//! given pass acts on is named by the [`RetirableGround`] set inside
//! [`Recovery::RetireProvedParks`]; a retirable ground the policy left out is
//! reported [`RetirementRefused::GroundNotOptedIn`] and written to no more than
//! a `Stalled` row is. This section says what **can** be retired and why.
//!
//! **Production now names both**, which it did not until the receipt existed.
//! The completed-child ground was held at diagnosis-only for one specific
//! reason — a refinement pass can commit `Success` before its successor address
//! has been durably seeded, so a walk over the chain read an empty successor
//! that meant *not started writing yet* and not *never commissioned*. That is
//! closed by [`super::store::ChainClosure`]: the writer that decides no further
//! pass will run publishes a receipt for the segment it finished on, and
//! [`LoopReconciler::child_state`] proves a `Success` child finished only when
//! it read one. A park judged inside that window now reports
//! [`ParkReason::ChildChainNotClosed`] and is left alone. The two spellings of
//! "not retired" below describe grounds the policy cannot prove or did not opt
//! into.
//!
//! - **[`ParkReason::DeadlinePassedWhileParked`] — retirable, and the sound
//!   one.** [`RetirableGround::DeadlinePassedWhileParked`] is the ground a
//!   policy names to act on it. The proof reads
//!   nothing outside the run: it is the state's own `deadline_at_ms` against the
//!   clock, and nothing in the workspace moves a deadline forward. Checked by
//!   sweeping every writer of the field rather than by reading the one in front
//!   of us: there is exactly one production writer,
//!   `StatelessArm::seed_if_absent`, which commits against `Revision::INITIAL`
//!   and so only ever runs when nothing is committed under the key;
//!   [`super::state::LoopState::for_commit`] writes `placement` and
//!   `work_budget_consumed_ms` and nothing else; and `commit_boundary` carries
//!   the field forward from the loaded state untouched. A deadline is therefore
//!   fixed for the life of a key. Both routes a claim can take run **no phase**
//!   — `advance_under_lease`
//!   checks PARK before DEADLINE, so it answers `Parked` while the wait stands
//!   and commits the canonical timeout terminal once it does not — so nothing can append behind the
//!   retirement. *Why a terminal rather than un-parking:* un-parking a
//!   deadline-passed run leaves it withheld by `runnable_at` anyway, so it would
//!   still be stuck, only less legibly. *Why non-resumable:* a resumable terminal
//!   invites a resume that the deadline guard refuses at its first phase entry.
//! - **[`ParkReason::EveryChildHasFinished`] — retirable, and the one to think
//!   twice before naming.** Every named child
//!   recorded a terminal it cannot come back from **and** cannot be offered back
//!   to a person — the second half is [`ChildState::MayStillBeContinued`], which
//!   stops the proof for the two kinds whose `AgenticOutcome` may carry a pause
//!   record — and the wake ledger was empty
//!   on a read taken after the children were judged *and* on a third read taken
//!   under the lease. The token is computed from those same ids, so there is no
//!   other completer to be wrong about. *Why a terminal rather than un-parking:*
//!   the first bullet of *What it recovers* — nothing bounds a re-park cycle.
//!   *Why not resolving the wake:* the third bullet there.
//!
//!   **The ADDRESS the children are read at is now part of that argument, and
//!   for a long time it was not.** Everything above reasons about the child
//!   *records*; `child_state` used to read them under a child's **bare**
//!   execution id, while that child's refinement pass 1 ran under `{child}-p1`
//!   and pass 0's non-resumable terminal sat on the bare key. For the whole of
//!   pass 1 the bare key read *finished* about a child that was working — past
//!   a fifteen-minute grace a refinement pass routinely outlives. That is
//!   closed: the park carries [`super::state::ResumeAddress`], `child_state`
//!   **reads** every address a further pass could run under, and it issues
//!   `Finished` only when it read them and they were empty — or when the last
//!   terminal is one `refinement_gaps_if_warranted` cannot commission a pass
//!   from at all.
//!
//!   **And the RECEIPT is the rest of that argument.** Reading every named
//!   address and finding the successors empty is not the same claim as *nothing
//!   followed*: a refinement pass that has been commissioned and has not yet
//!   reached `seed_if_absent` is empty at exactly the address the walk reads,
//!   for the seconds `build_run_setup` and seeding take. That gap is not
//!   closable by reading harder, so the writer publishes a
//!   [`super::store::ChainClosure`] for the segment it finished on and
//!   `child_state` proves a `Success` child finished only when it read one.
//!   Without a receipt the row is [`ParkReason::ChildChainNotClosed`] — a
//!   suspicion, never retired.
//!
//!   Two things that still do **not** hold, so nobody reads the ground as more
//!   than it is. A park committed **before** the address existed carries an
//!   unstated chain and there is nowhere for a pass to look, so every child of
//!   it that succeeded reports [`ParkReason::ChildAddressNotCarried`] and the
//!   park is never proved — the permanent state of the existing backlog. And the
//!   `-r{n}` axis is still not followed — see the *remaining boundaries*
//!   bullet — where it costs a false negative rather than a false proof. This
//!   stays a [`RetirableGround`] of its own because it reads state **outside
//!   the run**, which the deadline ground never does, and `magician-bin` names
//!   it beside the deadline ground rather than instead of it.
//! - **[`ParkReason::NoChildrenNamed`] — DECLINED, deliberately, and not by a
//!   policy.** [`RetirableGround`] has no variant for it, so declining it is not
//!   a setting anybody can change — which is the difference the report spells as
//!   [`RetirementRefused::GroundIsNotActedOn`] rather than
//!   [`RetirementRefused::GroundNotOptedIn`]. It is the cheapest proof of the
//!   three and it is still not acted on, because it is the one ground that is
//!   **evidence of a writer this module cannot reason about**.
//!   [`super::state::WaitReason::children`] cannot construct it and
//!   `driver_worker::check_park` refuses to commit it — `executor.rs`'s
//!   `wait_for_outcome` passes an empty list through *precisely* so that refusal
//!   fires — so no correct writer in this tree produces it. Every other safety
//!   argument here rests on the state having been published by a driver that
//!   obeys the same rules: that its cursor is its journal's cursor, that its
//!   watermark is real, that its placement means what it says. A state that
//!   proves one of those writers was not obeying the rules is not a state to
//!   append a permanent terminal to on the strength of the rest of it. Reporting
//!   is the right answer to a writer bug; writing is not. It also costs nothing
//!   to decline: with `check_park` in force the ground is unreachable from this
//!   tree, so it cannot accumulate and cannot starve the scan.
//!
//! ## Retirement cascades, and that is correct rather than merely tolerated
//!
//! A retired child records a non-resumable terminal, so its own parent's next
//! pass reads it as `Finished` instead of `Live` and may then prove
//! `EveryChildHasFinished` about the parent. A dead delegation tree therefore
//! drains bottom-up over successive passes, one level per pass. That is sound
//! exactly as far as each step's proof is: a child is only retired when it is
//! itself provably unwakeable, and a parent waiting on a provably unwakeable
//! child is genuinely waiting on nothing. What it is **not** is a licence to
//! weaken a single step — a wrong retirement does not stay local, it propagates
//! up the tree one pass at a time.
//!
//! There is no false-`Finished` route through an ordinary resume, which is the
//! case that would have made the cascade unsound: a run that pauses and resumes
//! leaves a **resumable** terminal at the bare key, and `segment_state` reads that
//! as `Live`.
//!
//! There **was** one through a refinement pass, and it is the reason to read
//! this section twice rather than once. A pass 0 that ended `Success` on the
//! bare key while pass 1 worked under `{child}-p1` read as `Finished`, so a
//! parent could be proved unwakeable on a child that was running — and by the
//! paragraph above, that wrong proof would then propagate up the tree one pass
//! at a time. `child_state` now reads `{child}-p1` before it concludes
//! anything, so a live refinement pass answers `Live` and the cascade's
//! per-step proof holds on this axis. It is worth restating that the cascade is
//! what makes a single weak step expensive: the fix is not a local one.
//!
//! ### The route that IS reachable, and what it cost to find out
//!
//! `segment_state` asks [`TerminalKind::is_resumable`], and two kinds answer
//! **false** for outcomes that carry a pause record and are offered to a person
//! as continue-or-cancel: `AgenticOutcome::MaxIterationsReached` and
//! `AgenticOutcome::BudgetExhausted` are resumable when their `pause_state` is
//! `Some`, and `TerminalKind::from` maps both spellings to the one non-resumable
//! kind. A child ending that way would read as `Finished` here while a person
//! could still continue it — a false proof, and one a retirement would make
//! permanent.
//!
//! **An earlier version of this section said the route was unreachable, and it
//! was wrong.** The claim was that `run_loop/phases/` constructs exactly five
//! outcomes and none of the non-resumable ones carries a pause record. It does
//! not: `phases/resolve.rs`'s token-budget check calls `conclude_budget_exhausted`,
//! which builds `AgenticOutcome::BudgetExhausted { pause_state: Some(..) }`
//! **unconditionally** and emits a `HitlRequested` reading *"Continue or
//! cancel?"* beside it. `commit_boundary` journals that as
//! `RunEnded { BudgetExhausted }`, which `is_resumable` answers false for. The
//! budget in question is `ctx.max_tokens_per_cycle`, which
//! `v2_orchestrator` sets for orchestrated runs and `executor.rs` scopes around
//! the whole loop — so it is live on exactly the delegated children this module
//! judges. The gate that was supposed to catch the day this changed could not:
//! it scanned the six phase files for the literal string
//! `AgenticOutcome::BudgetExhausted`, and the construction lives one function
//! call away in `executor.rs`.
//!
//! So it is handled here rather than assumed away. `child_state` answers
//! [`ChildState::MayStillBeContinued`] for those two kinds, `diagnose_children`
//! holds it as a fault exactly as it holds an unreadable child, and the park is
//! reported [`ParkVerdict::Stalled`] with
//! [`ParkReason::ChildMayStillBeContinued`] — never proved, therefore never
//! retired. That is a **false negative** on a child that ended with
//! `pause_state: None`: the park is reported as a suspicion instead of a proof,
//! and the parent keeps its slot in the listing. It is the direction this module
//! takes everywhere else, and it is the only one available from here — the
//! journal records the kind and not the payload, so *"pause record present"* is
//! not a question the store can be asked.
//!
//! **The real fix is still not here.** The collapse is `journal.rs`'s
//! `TerminalKind::from`, which maps a pause-carrying `BudgetExhausted` and a
//! spent one onto one kind; a kind that kept them apart would let this module go
//! back to proving the spent ones. Until then the cost of the conservative
//! reading is borne here and named in the report.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::Utc;
use tracing::warn;

use super::journal::{
    replay, Journal, JournalAppend, JournalBody, ProjectorCursor, RecordedStep, TerminalKind,
};
use super::state::{Placement, ResumeAddress, WaitReason, WorkerId};
use super::store::{
    ExecutionKey, LastLease, LoopStateStore, ParkedExecution, Revision, ScanCursor, StoreError,
    StoreResult,
};

/// How long a park on **children** may sit before it is examined at all.
///
/// Short enough that a hang is found in one working session rather than in a
/// postmortem, and long enough to leave room for the terminal-first lifecycle
/// handoff. The figure should be checked against the longest observed gap
/// between authoritative child readiness and publication of the parent's
/// durable receipt; it is a recovery budget, not a scheduling interval.
pub const DEFAULT_PARK_GRACE: Duration = Duration::from_secs(15 * 60);

/// How long a park on an external **job** may sit before it is examined.
///
/// Much longer, and the asymmetry is the point rather than an oversight. A park
/// on children is *meant* to end in seconds — see [`DEFAULT_PARK_GRACE`] for how
/// much of that is built and how much is a budget — so fifteen minutes of
/// silence is already anomalous. A park on a job is *designed* to be long: the
/// design says an execution *"can sit in `awaiting_job` for 8 hours holding
/// nothing but a row on disk"*, and the coding budget is where that figure comes
/// from. Judging a
/// job park on the children grace would report every legitimate long run every
/// pass for most of its life, and a list that is mostly noise is a list nobody
/// reads — which costs the real findings beside it.
///
/// Twelve hours, so it sits clear of the longest run the design contemplates
/// rather than at its edge. A job park past its own **deadline** is reported
/// immediately regardless, which is the case where waiting would be wrong.
pub const DEFAULT_JOB_PARK_GRACE: Duration = Duration::from_secs(12 * 60 * 60);

/// The most parked executions one pass will take.
///
/// Filling it sets [`super::store::ParkedListing::incomplete`] rather than
/// silently truncating, because a reconciler that stopped short has not checked
/// the tail and must not read as though it had.
///
/// The scheduler retains [`ReconcileReport::resume`] and feeds it back through
/// [`ReconcilerPolicy::scan_after`], so a full page advances to a bounded next
/// window instead of repeatedly diagnosing the same prefix. Reaching the end
/// resets the next sweep to the beginning.
pub const DEFAULT_SCAN_LIMIT: usize = 1_000;

/// The most children one park's diagnosis will load.
///
/// A ceiling rather than a policy knob, for the same reason the store's bounded
/// reads are: it exists so one malformed state cannot make a pass unbounded, not
/// so an operator can tune it. A park naming more than this is reported as
/// [`ParkReason::TooManyChildrenToDiagnose`] — **stalled, never proved** —
/// because a diagnosis that skipped children would be a proof about a subset
/// dressed as one about the whole.
///
/// # It bounds the CHILDREN and the reads are per SEGMENT
///
/// Said here rather than left to arithmetic at the call site, because the two
/// numbers stopped being the same when the park started carrying addresses.
/// `child_state` reads every key in the chain a park names for a child, so the
/// per-park read bill is `children × chain length`, and the chain length is
/// `executor.rs`'s business — `MAX_REFINEMENT_PASSES + 1`, two today. This
/// constant bounds the first factor only.
///
/// It is deliberately not tightened to compensate. The second factor is a small
/// constant chosen by a module this one cannot see, and lowering the child
/// ceiling to keep a product fixed would make a legitimate 64-child park
/// undiagnosable the day somebody added a refinement pass — a *reported* park
/// turning into a `TooManyChildrenToDiagnose` row for a reason that has nothing
/// to do with its children.
const MAX_CHILDREN_DIAGNOSED: usize = 64;

/// How long a retirement holds the lease.
///
/// Short, because the work under it is four store calls and no phase. The cost
/// of it being too short is a `Conflict` on the commit, which is refused and
/// retried next pass; the cost of it being too long is an execution nothing else
/// may touch for that long after this process dies mid-retirement. The second is
/// the one worth minimising, and unlike a phase there is no long tail to cover.
const RETIREMENT_LEASE_TTL: Duration = Duration::from_secs(30);

/// The namespace every reconciler identity lives in.
///
/// A convention that is now checked. See [`ReconcilerWorker`] for what it buys
/// and, precisely, what it does not.
pub const RECONCILER_WORKER_PREFIX: &str = "reconciler-";

/// A [`WorkerId`] that has been kept out of the phase workers' namespace.
///
/// A worker id is an operational identity, not the lease capability. Both
/// stores refuse every new claim while a lease is live — even a claim carrying
/// the holder's id — and require the current fence to renew it. The separate
/// namespace therefore buys attribution rather than exclusion: `LeaseHeld`
/// reports and persisted lease history identify a reconciler as a reconciler.
///
/// # Why a newtype and not a check at the call site
///
/// The field is inside [`Recovery::RetireProvedParks`] so that recovery cannot
/// be switched on without naming who takes the lease. That is a shape, and it
/// works. But the hazard is the id's **value**, and a `WorkerId` field would
/// have left the value unconstrained however the variant was arranged — a
/// paragraph asking the caller to spell it right, which is the thing this
/// module's own doc calls "a runtime check standing in for a shape".
///
/// So the value is constrained by the only mechanism that can constrain it: a
/// private field with no public constructor that skips the check. An enum
/// variant's fields are always public, so the check has to live one type down.
///
/// # What this does NOT establish
///
/// It is a **namespace**, not a global registry. Nothing stops a phase worker
/// being named `reconciler-7` too — this module cannot see the other workers'
/// ids and never will. [`Self::for_this_process`] adds a random per-instance
/// nonce so normal construction is distinct within one process and across
/// processes or containers that reuse a pid; [`Self::named`] deliberately lets
/// a caller supply a stable identity when operational policy requires one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilerWorker(WorkerId);

impl ReconcilerWorker {
    /// Mint `reconciler-<pid>-<instance nonce>` for one reconciler instance.
    ///
    /// The pid remains useful when reading logs; the nonce is the part that
    /// prevents concurrent instances in one process, or equal pids in separate
    /// containers, from becoming operationally indistinguishable. A fresh id
    /// is returned on every call.
    pub fn for_this_process() -> Self {
        Self(WorkerId::new(format!(
            "{RECONCILER_WORKER_PREFIX}{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        )))
    }

    /// Any id inside the reconciler namespace. Refuses everything else.
    pub fn named(id: impl Into<String>) -> Result<Self, NotAReconcilerId> {
        let id = id.into();
        if id.starts_with(RECONCILER_WORKER_PREFIX) && id.len() > RECONCILER_WORKER_PREFIX.len() {
            Ok(Self(WorkerId::new(id)))
        } else {
            Err(NotAReconcilerId { offered: id })
        }
    }

    /// The id the lease is taken under.
    pub fn as_worker_id(&self) -> &WorkerId {
        &self.0
    }
}

/// [`ReconcilerWorker::named`] was handed an id outside the reconciler namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAReconcilerId {
    pub offered: String,
}

impl std::fmt::Display for NotAReconcilerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a reconciler id must start with `{RECONCILER_WORKER_PREFIX}` and carry something \
             after it, so reconciler lease ownership remains distinct from phase-worker \
             ownership; got {:?}",
            self.offered
        )
    }
}

impl std::error::Error for NotAReconcilerId {}

/// Durable receipt prepared before a delegation checkpoint is dispatched.
///
/// The source segment has already committed [`TerminalKind::HandedOff`] when
/// this value is returned, so no worker can re-enter its mid-phase cursor. The
/// wake resolution remains outstanding until the lifecycle owner has handed
/// the exact checkpoint to its successor address and calls
/// [`consume_prepared_children_handoff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedChildrenHandoff {
    pub source: ExecutionKey,
    pub resumes_as: String,
    pub wake_token: String,
    pub resolution_id: String,
    /// `false` on a crash/retry that found the source already handed off and
    /// merely restored its durable wake receipt.
    pub newly_retired: bool,
}

/// Why an exact delegation handoff could not be prepared.
#[derive(Debug)]
pub enum ChildrenHandoffError {
    InvalidRequest {
        detail: String,
    },
    StateMismatch {
        key: ExecutionKey,
        detail: String,
    },
    JournalUnusable {
        key: ExecutionKey,
        detail: String,
    },
    OutboxNotCaughtUp {
        key: ExecutionKey,
        projected_through: u64,
        committed_through: u64,
    },
    Store {
        key: ExecutionKey,
        during: &'static str,
        source: StoreError,
    },
}

impl std::fmt::Display for ChildrenHandoffError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest { detail } => formatter.write_str(detail),
            Self::StateMismatch { key, detail } => {
                write!(
                    formatter,
                    "loop-state segment {key} cannot be handed off: {detail}"
                )
            },
            Self::JournalUnusable { key, detail } => {
                write!(
                    formatter,
                    "loop-state segment {key} has an unusable journal: {detail}"
                )
            },
            Self::OutboxNotCaughtUp {
                key,
                projected_through,
                committed_through,
            } => write!(
                formatter,
                "loop-state segment {key} still owes accepted events: projector is through \
                 {projected_through}, committed journal is through {committed_through}"
            ),
            Self::Store {
                key,
                during,
                source,
            } => write!(
                formatter,
                "loop-state handoff for {key} failed during {during}: {source}"
            ),
        }
    }
}

impl std::error::Error for ChildrenHandoffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn handoff_store_error(
    key: &ExecutionKey,
    during: &'static str,
    source: StoreError,
) -> ChildrenHandoffError {
    ChildrenHandoffError::Store {
        key: key.clone(),
        during,
        source,
    }
}

/// Prepare the exact source-segment handoff for a delegation checkpoint.
///
/// The order is intentionally terminal-then-wake. Publishing a wake first
/// opens a crash window in which the ordinary worker may leave the park and
/// re-enter the old mid-`Apply` cursor. Here the source first commits the
/// non-resumable [`TerminalKind::HandedOff`], then receives an idempotent wake
/// receipt while the lease still excludes every claimant. A crash at any point
/// is retryable:
///
/// - before the terminal commit, the original park is intact;
/// - after the terminal but before the wake, a retry recognizes `HandedOff` and
///   restores the same receipt;
/// - after the wake, the receipt remains until the lifecycle owner dispatches
///   the checkpoint and explicitly consumes it.
///
/// The caller must provide both addresses captured on the same durable pause:
/// `source` from `AgenticPauseState::stateless_parked_segment` and `resumes_as`
/// from its pending-pause projection. The state under the lease must name the
/// same child multiset and successor address; a bare execution-id guess cannot
/// retire a segment.
pub async fn prepare_children_handoff(
    store: &dyn LoopStateStore,
    source: &ExecutionKey,
    expected_child_execution_ids: &[String],
    resumes_as: &str,
    worker: &ReconcilerWorker,
) -> Result<PreparedChildrenHandoff, ChildrenHandoffError> {
    if expected_child_execution_ids.is_empty() {
        return Err(ChildrenHandoffError::InvalidRequest {
            detail: "a delegation handoff requires at least one child execution".to_string(),
        });
    }
    if expected_child_execution_ids.len() > MAX_CHILDREN_DIAGNOSED {
        return Err(ChildrenHandoffError::InvalidRequest {
            detail: format!(
                "a delegation handoff names {} children, over the bounded limit of {}",
                expected_child_execution_ids.len(),
                MAX_CHILDREN_DIAGNOSED
            ),
        });
    }
    if resumes_as.is_empty() {
        return Err(ChildrenHandoffError::InvalidRequest {
            detail: "a delegation handoff requires the checkpoint successor address".to_string(),
        });
    }

    let mut expected_children = expected_child_execution_ids.to_vec();
    expected_children.sort();
    for child in &expected_children {
        ExecutionKey::new(source.principal(), source.workspace(), child.clone()).map_err(
            |error| ChildrenHandoffError::InvalidRequest {
                detail: format!("delegation child {child:?} cannot address loop state: {error}"),
            },
        )?;
    }
    let wake_token = format!("children:{}", expected_children.join(","));
    // A source segment can hand off at most once: after this boundary it is
    // terminal. Its address is therefore the stable event identity needed to
    // make a retried readiness publication idempotent without collapsing a
    // later park on another resume generation.
    let intent = serde_json::to_vec(&(
        source.execution_id(),
        resumes_as,
        expected_children.as_slice(),
    ))
    .map_err(|error| ChildrenHandoffError::InvalidRequest {
        detail: format!("the delegation handoff identity could not be encoded: {error}"),
    })?;
    // Bind the readiness id to the checkpoint intent. On a retry after the wait
    // has been cleared, the existing receipt is the durable evidence that the
    // caller named the same successor and child multiset as the first handoff.
    let resolution_id = format!("delegation-ready:{}", blake3::hash(&intent).to_hex());

    let lease = store
        .claim(source, worker.as_worker_id(), RETIREMENT_LEASE_TTL)
        .await
        .map_err(|error| handoff_store_error(source, "claim", error))?;

    let prepared = prepare_children_handoff_under_lease(
        store,
        source,
        &expected_children,
        resumes_as,
        &wake_token,
        &resolution_id,
        &lease,
    )
    .await;

    if let Err(error) = store.release(lease).await {
        warn!(
            execution = %source,
            worker = %worker.as_worker_id(),
            %error,
            "[LOOP-RECONCILER] releasing a delegation-handoff lease failed; it will expire instead"
        );
    }
    prepared
}

async fn prepare_children_handoff_under_lease(
    store: &dyn LoopStateStore,
    source: &ExecutionKey,
    expected_children: &[String],
    resumes_as: &str,
    wake_token: &str,
    resolution_id: &str,
    lease: &super::store::Lease,
) -> Result<PreparedChildrenHandoff, ChildrenHandoffError> {
    let committed = store
        .load(source)
        .await
        .map_err(|error| handoff_store_error(source, "load", error))?
        .ok_or_else(|| ChildrenHandoffError::StateMismatch {
            key: source.clone(),
            detail: "nothing is committed under the checkpoint's source address".to_string(),
        })?;
    let revision = committed.revision;
    let mut state = committed.state;
    let journal = store
        .read_journal_verified(source)
        .await
        .map_err(|error| handoff_store_error(source, "read_journal_verified", error))?;
    let replayed = replay(journal.authoritative(state.journal_seq)).map_err(|error| {
        ChildrenHandoffError::JournalUnusable {
            key: source.clone(),
            detail: error.to_string(),
        }
    })?;
    if (replayed.iteration, replayed.phase) != (state.cursor.iteration, state.cursor.phase) {
        return Err(ChildrenHandoffError::JournalUnusable {
            key: source.clone(),
            detail: format!(
                "committed cursor {:?} disagrees with journal cursor ({}, {:?})",
                state.cursor, replayed.iteration, replayed.phase
            ),
        });
    }

    let already_retired = replayed.terminal == Some(TerminalKind::HandedOff);
    if state.wait.is_none() {
        if !already_retired {
            return Err(ChildrenHandoffError::StateMismatch {
                key: source.clone(),
                detail: "the segment is no longer parked and has no handoff terminal".to_string(),
            });
        }
        let resolutions = store
            .wake_resolutions(source, wake_token)
            .await
            .map_err(|error| handoff_store_error(source, "wake_resolutions", error))?;
        if !resolutions.iter().any(|existing| existing == resolution_id) {
            return Err(ChildrenHandoffError::StateMismatch {
                key: source.clone(),
                detail:
                    "the cleared handoff does not carry this checkpoint's bound readiness receipt"
                        .to_string(),
            });
        }
    } else {
        let wait = state
            .wait
            .as_ref()
            .expect("the preceding branch handled None");
        let WaitReason::Children {
            child_execution_ids,
            resume,
        } = wait
        else {
            return Err(ChildrenHandoffError::StateMismatch {
                key: source.clone(),
                detail: "the checkpoint source is not parked on delegated children".to_string(),
            });
        };
        let mut actual_children = child_execution_ids.clone();
        actual_children.sort();
        if actual_children.as_slice() != expected_children {
            return Err(ChildrenHandoffError::StateMismatch {
                key: source.clone(),
                detail: "the parked child set differs from the lifecycle owner's ready set"
                    .to_string(),
            });
        }
        if resume.run_resumes_as.as_deref() != Some(resumes_as) {
            return Err(ChildrenHandoffError::StateMismatch {
                key: source.clone(),
                detail: "the parked successor address differs from the durable checkpoint"
                    .to_string(),
            });
        }
    }

    let mut revision_for_clear = revision;
    if !already_retired {
        if replayed.terminal != Some(TerminalKind::WaitingForChildren) {
            return Err(ChildrenHandoffError::JournalUnusable {
                key: source.clone(),
                detail: format!(
                    "a child park must end on WaitingForChildren, found {:?}",
                    replayed.terminal
                ),
            });
        }

        let projected_through = store
            .load_projector_cursor(source)
            .await
            .map_err(|error| handoff_store_error(source, "load_projector_cursor", error))?
            .map(|cursor| cursor.emitted_through_seq())
            .unwrap_or(0);
        if projected_through > state.journal_seq {
            return Err(ChildrenHandoffError::JournalUnusable {
                key: source.clone(),
                detail: format!(
                    "projector cursor {projected_through} is ahead of committed watermark {}",
                    state.journal_seq
                ),
            });
        }
        let outbox_pending = journal
            .authoritative_from(projected_through.saturating_add(1), state.journal_seq)
            .iter()
            .any(|record| {
                matches!(
                    &record.body,
                    JournalBody::Event { .. } | JournalBody::NamedEvent { .. }
                )
            });
        if outbox_pending {
            return Err(ChildrenHandoffError::OutboxNotCaughtUp {
                key: source.clone(),
                projected_through,
                committed_through: state.journal_seq,
            });
        }

        let seq = store
            .append_journal_fenced(
                source,
                &[JournalAppend::phase_completed(
                    state.cursor.iteration,
                    state.cursor.phase,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::HandedOff,
                    },
                )],
                lease,
            )
            .await
            .map_err(|error| handoff_store_error(source, "append_journal", error))?;
        state.journal_seq = seq;
        // Keep the exact wait through the terminal->receipt crash window. It is
        // the durable handoff intent a retry validates; the non-resumable
        // terminal (and the still-held lease) prevent this parked cursor from
        // being advanced.
        revision_for_clear = store
            .commit_fenced(source, &state, revision, lease)
            .await
            .map_err(|error| handoff_store_error(source, "commit terminal", error))?;
    }

    // Publish only after the old cursor is terminal. The lease remains held so
    // even a store whose ending sidecar has not landed cannot give a claimant a
    // window between the two writes.
    store
        .resolve_wake(source, wake_token, resolution_id)
        .await
        .map_err(|error| handoff_store_error(source, "resolve_wake", error))?;

    if state.wait.is_some() {
        state.wait = None;
        store
            .commit_fenced(source, &state, revision_for_clear, lease)
            .await
            .map_err(|error| handoff_store_error(source, "clear retired wait", error))?;
    }

    Ok(PreparedChildrenHandoff {
        source: source.clone(),
        resumes_as: resumes_as.to_string(),
        wake_token: wake_token.to_string(),
        resolution_id: resolution_id.to_string(),
        newly_retired: !already_retired,
    })
}

/// Consume the exact readiness receipt after the checkpoint dispatch owner has
/// taken responsibility for `receipt.resumes_as`.
///
/// Idempotent: zero means a prior completion already consumed it. Consumption
/// deliberately is not part of [`prepare_children_handoff`]; keeping the receipt
/// across the dispatch boundary is what lets a crash retry distinguish
/// "source retired, continuation still owed" from an ordinary ended segment.
pub async fn consume_prepared_children_handoff(
    store: &dyn LoopStateStore,
    receipt: &PreparedChildrenHandoff,
) -> StoreResult<usize> {
    store
        .consume_wake(
            &receipt.source,
            &receipt.wake_token,
            std::slice::from_ref(&receipt.resolution_id),
        )
        .await
}

/// A ground a policy may opt into retiring: a KIND of proof, not one park.
///
/// Exactly the grounds `terminal_for` answers `Some` for, named after the
/// [`ParkReason`] variants they correspond to. The module's *Which grounds are
/// acted on* argues each of them; this type is only the vocabulary a caller
/// names them in.
///
/// # Why this and not [`ParkReason`] itself
///
/// Both retirable `ParkReason` variants carry payloads —
/// `DeadlinePassedWhileParked { deadline_at_ms }` and
/// `EveryChildHasFinished { children }` — because a reason is written from the
/// evidence and therefore names **one park**. A policy names a **kind**: *this
/// pass may retire deadline-passed parks*, decided before any park has been
/// seen and about parks whose payloads cannot be known yet.
///
/// Putting a `ParkReason` in the policy would force a caller to invent a
/// `deadline_at_ms` for a park that does not exist, and the natural membership
/// test — value equality, which is what `contains` means on every set in `std`
/// — would then answer `false` for every real park. That is a filter which
/// compiles, reads as enabled, and silently retires nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetirableGround {
    /// [`ParkReason::DeadlinePassedWhileParked`].
    ///
    /// The sound one. It is decided from the listing alone, reads nothing
    /// outside the run, and its proof is re-taken under the lease from the
    /// run's own `deadline_at_ms` — a field with exactly one production writer,
    /// which only ever runs when nothing is committed under the key.
    DeadlinePassedWhileParked,
    /// [`ParkReason::EveryChildHasFinished`].
    ///
    /// **The address defect is closed.** This ground used to be unsound under
    /// the key layout rather than merely weaker than the deadline one:
    /// `child_state` addressed a child by its **bare** execution id while that
    /// child's refinement pass 1 ran under `{child}-p1`, so pass 0's
    /// non-resumable terminal on the bare key read as *finished* about a child
    /// that was working — a false proof, past a fifteen-minute grace a
    /// refinement pass routinely outlives. The park now carries
    /// [`super::state::ResumeAddress`], `child_state` reads every address a
    /// further pass could run under, and a `Success` child is proved finished
    /// only on a chain it was actually able to read. See `executor.rs`'s
    /// `child_segment_addresses`.
    ///
    /// **The seconds-wide window is closed too, and by a read rather than by an
    /// argument.** The address chain proved a `Success` child finished when
    /// every address a further pass could run under was read and was empty —
    /// which is the wrong claim during the stretch between a segment's terminal
    /// commit and its successor's first commit, because an empty successor then
    /// means *has not started writing* rather than *was never commissioned*.
    /// `child_state` now additionally requires a [`super::store::ChainClosure`]
    /// published for the segment it read the terminal at, and reports
    /// [`ParkReason::ChildChainNotClosed`] when there is none. Absence is never
    /// evidence, so the receipt can only turn a refusal into a proof.
    ///
    /// **It is still the ground to think twice before naming**, and the reasons
    /// are now the ones that remain rather than the two that were fixed: it
    /// reads state **outside the run**, which the deadline ground never does,
    /// and the `-r{n}` resume axis is still unfollowed, where it costs a false
    /// negative. A park committed before the address existed is reported
    /// [`ParkReason::ChildAddressNotCarried`] and never proved, so naming this
    /// ground does not act on the backlog — only on parks committed since.
    ///
    /// It stays a separate opt-in for exactly that reason: *sound* and *safe to
    /// switch on from a live run's critical path* are different claims, and the
    /// filter exists so the second can be answered per ground.
    EveryChildHasFinished,
}

impl RetirableGround {
    /// The ground a reason belongs to, or `None` for a ground never acted on.
    ///
    /// **The one match that decides what is retirable at all.** `terminal_for`
    /// is defined in terms of this function rather than repeating the list, so
    /// the two cannot drift into naming different sets — which is a hazard the
    /// module has already paid for once, in the second bullet of the
    /// exhaustiveness note inside `retire_under_lease`.
    fn of(reason: &ParkReason) -> Option<Self> {
        match reason {
            ParkReason::DeadlinePassedWhileParked { .. } => Some(Self::DeadlinePassedWhileParked),
            ParkReason::EveryChildHasFinished { .. } => Some(Self::EveryChildHasFinished),
            // Every other ground, named by exhaustion rather than by `_`, so a
            // new `ParkReason` has to be given an answer here instead of
            // silently inheriting "never retired" — which is the safe answer but
            // not one to arrive at by default.
            ParkReason::NoChildrenNamed
            | ParkReason::ChildNeverCommitted { .. }
            | ParkReason::ChildUnreadable { .. }
            | ParkReason::ChildUnaddressable { .. }
            | ParkReason::ChildMayStillBeContinued { .. }
            | ParkReason::ChildAddressNotCarried { .. }
            | ParkReason::ChildChainNotClosed { .. }
            | ParkReason::TooManyChildrenToDiagnose { .. }
            | ParkReason::JobHasNotReported { .. }
            | ParkReason::WakeResolvedButNotResumed { .. } => None,
        }
    }

    /// The terminal a park retired on this ground is ended with.
    ///
    /// Matched rather than answered with one expression, so a ground added to
    /// this type has to state its own answer. `terminal_for` carries the whole
    /// argument for why both grounds answer the same kind.
    fn terminal(self) -> TerminalKind {
        match self {
            Self::DeadlinePassedWhileParked | Self::EveryChildHasFinished => {
                TerminalKind::CannotProceed
            },
        }
    }
}

/// Whether a pass may act on what it proves, on which grounds, and under whose
/// identity.
///
/// Both of the acting variant's fields are **inside** it rather than beside it
/// on [`ReconcilerPolicy`], and for one reason applied twice: recovery cannot be
/// enabled without answering the questions that make it safe.
///
/// - The **worker**, because a retirement takes a lease and a lease needs a
///   [`WorkerId`]. A `bool` plus an `Option<WorkerId>` would have made
///   `recover: true, worker: None` expressible, and the only honest thing to do
///   with it at the call site is refuse — a runtime check standing in for a
///   shape the type could have made unrepresentable.
/// - The **grounds**, for exactly the same reason and against exactly the same
///   alternative. A `grounds` field on [`ReconcilerPolicy`] beside a `recovery`
///   one would make *recovery on, grounds unset* expressible, and the two
///   readings of that — "act on everything provable" and "act on nothing" —
///   are the widest and the narrowest possible behaviours of a lease-taking
///   write. A default would be worse still: the whole reason this filter exists
///   is that the two grounds are **not equally sound**, so inheriting a set is
///   inheriting the unsound half without having read it.
///
/// An empty set is expressible and is inert — every proved row is refused
/// [`RetirementRefused::GroundNotOptedIn`]. The shape guarantees the grounds are
/// *named*, not that the naming is useful — the report says which rows it cost,
/// one row at a time, rather than a policy validator saying so once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Diagnose and report. Nothing is claimed, appended or committed.
    ///
    /// [`ReconcilerPolicy::default`]'s choice, and see the module's *What is not
    /// wired up* for why the default is this rather than the other one.
    ReportOnly,
    /// Retire the parks a pass **proves** unwakeable, on the named grounds and
    /// on nothing else — never on a [`ParkVerdict::Stalled`] row, never on
    /// [`ParkReason::NoChildrenNamed`], and never on a retirable ground this
    /// policy did not name.
    RetireProvedParks {
        /// The identity the lease is taken under. See [`ReconcilerWorker`] for
        /// why it is not a bare [`WorkerId`].
        worker: ReconcilerWorker,
        /// The grounds this pass may act on. A proved row whose ground is not
        /// in here is refused [`RetirementRefused::GroundNotOptedIn`] and left
        /// exactly as it was — reported, never written.
        ///
        /// # Why a [`BTreeSet`] over a two-member domain
        ///
        /// The domain is two variants, so every container is cheap and the
        /// choice is about what the type *says* rather than about cost.
        ///
        /// - A `Vec` is the obvious reach and is wrong twice over: it admits
        ///   duplicates, and it orders its members. [`Recovery`] derives
        ///   `PartialEq`, so two policies naming the same grounds in a
        ///   different order would compare unequal — a difference tests and a
        ///   config diff both read, about a value whose whole meaning is set
        ///   membership.
        /// - A `HashSet` is a correct set, and it drags in `Hash` for a domain
        ///   of two while leaving its iteration order unspecified, so a
        ///   `Debug`-printed policy can read differently between two passes.
        ///   That is a poor trade where the thing being printed is the audit of
        ///   a lease-taking write.
        /// - `BTreeSet` needs only `Ord`, which is derivable and total on a
        ///   fieldless enum; it de-duplicates by construction and prints in one
        ///   stable order. Its one allocation is per policy, not per park, so
        ///   the usual objection to a heap set for two elements prices at
        ///   nothing on this path.
        grounds: BTreeSet<RetirableGround>,
    },
}

/// What a pass is allowed to cost, how patient it is, and whether it may act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcilerPolicy {
    /// Applies to [`WaitReason::Children`].
    pub park_grace: Duration,
    /// Applies to [`WaitReason::Job`]. See [`DEFAULT_JOB_PARK_GRACE`] for why the
    /// two are not one number.
    pub job_park_grace: Duration,
    pub scan_limit: usize,
    /// Resume position supplied by the scheduler that owns reconciliation
    /// cadence. The reconciler itself is deliberately short-lived, so keeping
    /// this only on `LoopReconciler` would reset every pass to page one.
    pub scan_after: Option<ScanCursor>,
    /// Whether this pass writes. See [`Recovery`].
    pub recovery: Recovery,
}

impl Default for ReconcilerPolicy {
    fn default() -> Self {
        Self {
            park_grace: DEFAULT_PARK_GRACE,
            job_park_grace: DEFAULT_JOB_PARK_GRACE,
            scan_limit: DEFAULT_SCAN_LIMIT,
            scan_after: None,
            recovery: Recovery::ReportOnly,
        }
    }
}

impl ReconcilerPolicy {
    /// How long this kind of park may sit before it is examined.
    fn grace_ms_for(&self, wait: &WaitReason) -> i64 {
        let grace = match wait {
            WaitReason::Children { .. } => self.park_grace,
            WaitReason::Job { .. } => self.job_park_grace,
        };
        i64::try_from(grace.as_millis()).unwrap_or(i64::MAX)
    }
}

/// How strongly this module is claiming a park is stuck.
///
/// Two levels rather than one, and the distinction is the point of the type: an
/// operator handed a single "stuck" list cannot tell the row that is a proof from
/// the row that is a suspicion, and ends up treating both the same — which means
/// either ignoring the proofs or acting on the suspicions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkVerdict {
    /// **Proved: this run cannot advance again.**
    ///
    /// Two different grounds reach it, and an operator acting on a row has to
    /// read its [`ParkReason`] to know which — they call for opposite next
    /// steps:
    ///
    /// - **Nothing can resolve the token.** Every named child recorded a
    ///   terminal it cannot come back from
    ///   ([`ParkReason::EveryChildHasFinished`]), or the park names no children
    ///   at all ([`ParkReason::NoChildrenNamed`]). This is the ground the
    ///   module's closing assumption is about, and the one where the completer
    ///   is what to go looking for.
    /// - **A resolution would not help.**
    ///   [`ParkReason::DeadlinePassedWhileParked`]. The run is past its own
    ///   deadline, so `store::fs`'s `runnable_at` withholds it whatever the wake
    ///   ledger says — it tests the deadline before the wait — and a claim by
    ///   key commits the canonical timeout terminal as soon as the wait stops standing. This
    ///   ground is decided from the listing alone and reads **nothing** about
    ///   the token or its completer: it proves the run is dead and proves
    ///   nothing at all about whether something was still trying to wake it.
    ///
    /// Spelled as two because the single line this replaces — *nothing that
    /// could resolve this park's token still exists* — is only the first, and an
    /// operator filtering on this verdict to find dead completers would be
    /// handed rows whose completer is alive and well.
    Unwakeable,
    /// **Suspected.** This module could not see far enough to say the run is
    /// dead.
    ///
    /// Deliberately the bucket for reasons with nothing in common except that
    /// none of them is a proof, so it says less on its own than `Unwakeable`
    /// does — read the [`ParkReason`] before acting, because the two ends of the
    /// range mean opposite things:
    ///
    /// - [`ParkReason::JobHasNotReported`], [`ParkReason::ChildNeverCommitted`],
    ///   [`ParkReason::ChildUnreadable`], [`ParkReason::ChildUnaddressable`] and
    ///   [`ParkReason::TooManyChildrenToDiagnose`] all mean *nothing has
    ///   answered this park, and this pass could not establish that nothing
    ///   will*. The thing to go looking for is a completer that never came.
    /// - [`ParkReason::ChildMayStillBeContinued`] is a third thing again: the
    ///   child **did** end, and the ending is one a person may still be offered
    ///   as continue-or-cancel. The thing to go looking for is the unanswered
    ///   prompt on the child, not a completer on the parent.
    /// - [`ParkReason::ChildAddressNotCarried`] is a fourth, and it is a
    ///   statement about **this pass** rather than about the run: the child
    ///   ended `Success` at the one address an old park names, and where its
    ///   work may have continued is not recorded anywhere this pass can read.
    ///   Nothing is established as missing. Acting on this row as though a
    ///   completer were dead is the mistake it exists to prevent.
    /// - [`ParkReason::ChildChainNotClosed`] is a fifth, and it is also a
    ///   statement about **this pass**: the child ended `Success` at the last
    ///   address the park names, every later address was empty, and nothing had
    ///   yet published the receipt that says the segment was the last one. The
    ///   ordinary cause is that the pass looked in the seconds before the
    ///   child's own writer published; the next sweep settles it. A row that
    ///   persists means the child's process died between its terminal and its
    ///   receipt, and the thing to go looking for is that process — not a
    ///   completer.
    /// - [`ParkReason::WakeResolvedButNotResumed`] means the opposite: something
    ///   **did** answer and the wait is satisfied. Whether that is a fault at
    ///   all is not settled by the row — the reason's own doc says what this
    ///   module cannot tell about it — but looking for a dead completer here
    ///   finds a healthy one.
    ///
    /// A `Job` park is always this, however long it has waited, because job
    /// runners live outside this store and their absence is not observable from
    /// here.
    Stalled,
}

/// Why a park was reported.
///
/// Written from the evidence rather than from a severity, so a reader can check
/// the verdict against what was actually seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkReason {
    /// Every named child has recorded a terminal it cannot come back from.
    ///
    /// **Retirable**, and only that: retired under a
    /// [`Recovery::RetireProvedParks`] that names
    /// [`RetirableGround::EveryChildHasFinished`], and refused
    /// [`RetirementRefused::GroundNotOptedIn`] under one that does not.
    ///
    /// *"Recorded a terminal it cannot come back from"* now means **at an
    /// address nothing can follow**, which is the whole of what changed here.
    /// It used to mean *at the child's bare execution id*, which is where a
    /// refinement pass's predecessor ends, and this row was issued about
    /// children that were working. Read
    /// [`RetirableGround::EveryChildHasFinished`]'s own docs before naming that
    /// ground: the address defect is closed, and the reasons to think twice
    /// about it are now different ones rather than none.
    ///
    /// *"Nothing can follow"* is a **read** and not an inference. A child whose
    /// last committed terminal is `Success` earns this row only when a
    /// [`super::store::ChainClosure`] published for that segment says no further
    /// pass was commissioned; without one the child is
    /// [`ParkReason::ChildChainNotClosed`] and the park is not proved.
    EveryChildHasFinished { children: usize },
    /// The park names no children at all.
    ///
    /// [`super::state::WaitReason::children`] cannot construct this and
    /// `driver_worker::check_park` refuses to commit it, so a state carrying it
    /// was written by something that bypassed both — or by a driver older than
    /// either. It is unwakeable by construction: the token is `children:` and
    /// nothing computes that token from its own view of a child set.
    ///
    /// **Never retired, and that is not an oversight — nor a setting.**
    /// [`RetirableGround`] has no variant for it, so no policy can name it and
    /// the refusal is [`RetirementRefused::GroundIsNotActedOn`] rather than
    /// [`RetirementRefused::GroundNotOptedIn`]. It is the one
    /// [`ParkVerdict::Unwakeable`] ground that is itself evidence of a writer
    /// that did not obey the rules every other safety argument here rests on —
    /// that a committed cursor is its journal's cursor, that a watermark is
    /// real, that a placement means what it says. Recovery reports it and leaves
    /// it alone; the module's *Which grounds are acted on* carries the argument.
    NoChildrenNamed,
    /// At least one named child has never committed a state.
    ///
    /// **Not a proof**, deliberately: a child that was spawned and has not yet
    /// reached its first commit is indistinguishable from one that never started.
    /// The grace period makes the first unlikely; it does not make it impossible,
    /// and a verdict is not the place to round an unlikely case away.
    ChildNeverCommitted { child_execution_ids: Vec<String> },
    /// A child's state or journal could not be read.
    ChildUnreadable {
        child_execution_id: String,
        detail: String,
    },
    /// A named child id cannot address an execution at all.
    ChildUnaddressable {
        child_execution_id: String,
        detail: String,
    },
    /// A child recorded an ending a person may still be offered.
    ///
    /// [`TerminalKind::is_resumable`] answers **false** for
    /// [`TerminalKind::MaxIterationsReached`] and
    /// [`TerminalKind::BudgetExhausted`], and both of those kinds are what
    /// `TerminalKind::from` collapses a *pause-carrying* `AgenticOutcome` onto:
    /// `AgenticOutcome::BudgetExhausted { pause_state: Some(..) }` is a
    /// continue-or-cancel prompt, and `phases/resolve.rs` produces exactly that
    /// through `conclude_budget_exhausted` whenever a run is under a
    /// `max_tokens_per_cycle` — which orchestrated children are. So a child
    /// carrying one of these kinds is neither demonstrably finished nor
    /// demonstrably live.
    ///
    /// **Not a proof, therefore never retired**, and the module's *The route
    /// that IS reachable* section carries the argument. The cost of the
    /// conservative reading is a false negative on a child whose `pause_state`
    /// was `None`: a genuinely dead park is reported as a suspicion. The journal
    /// records the kind and not the payload, so the two cannot be told apart
    /// from the store — closing that is `journal.rs`'s to do.
    ChildMayStillBeContinued {
        child_execution_id: String,
        terminal: TerminalKind,
    },
    /// A child ended `Success` and the park does not say where its work may
    /// have continued.
    ///
    /// **This is what a park committed before [`super::state::ResumeAddress`]
    /// existed reports for every child of it that succeeded**, and it is the
    /// row that used to be an `EveryChildHasFinished` proof about a child that
    /// might have been working. A `Success` is the one outcome
    /// `refinement_gaps_if_warranted` will commission another pass from, and
    /// that pass runs under `{child}-p1` — an address a park with no chain does
    /// not name and this module must not invent.
    ///
    /// **Not a proof, therefore never retired.** The whole point of the variant
    /// is that it is a suspicion: it says *this pass did not look*, which is
    /// different from *this pass looked and found nothing*, and only the second
    /// is evidence.
    ///
    /// These rows drain as old parks age out of the store; they do not become
    /// provable, because nothing rewrites a committed park's address. An
    /// operator who wants one settled reads the child's own directory for a
    /// `-p{n}` sibling — which is exactly the read this module declines to
    /// guess at.
    ChildAddressNotCarried {
        child_execution_id: String,
        /// The address this pass did read: the child's bare execution id, the
        /// only one recoverable from a park that names no chain.
        read_at: String,
    },
    /// A child ended `Success` at an address the park named, every later
    /// address was read and was empty, and **no receipt says that segment was
    /// the last one**.
    ///
    /// # Why an empty successor is not evidence on its own
    ///
    /// `executor.rs`'s `execute_agentically_with_refinement` decides whether to
    /// commission a refinement pass **after** the predecessor's
    /// `RunEnded { Success }` is committed, and the successor takes some seconds
    /// — `build_run_setup` and seeding — to commit anything of its own. For that
    /// stretch the successor address is empty because the pass has not started
    /// writing, not because there is no pass, and a reader that could not tell
    /// those apart proved a working child finished. The address chain closed the
    /// long window where a successor was *visibly* live; it could not close this
    /// one, because the evidence a walk needs is not on disk yet.
    ///
    /// So the writer publishes a [`super::store::ChainClosure`] once it has
    /// decided nothing more will run, and this row is what a pass reports when
    /// there is no such receipt. **Not a proof, therefore never retired** — for
    /// the same reason [`ParkReason::ChildAddressNotCarried`] is not: *this pass
    /// did not see the evidence* is different from *the evidence says no*, and
    /// only the second may end a run.
    ///
    /// # It is transient where `ChildAddressNotCarried` is permanent
    ///
    /// An old park can never be proved, because nothing rewrites a committed
    /// park's address. This row usually settles on its own within one pass: the
    /// receipt appears the moment the child's wrapper returns, and the next
    /// sweep proves the park. A row that persists across sweeps means something
    /// else — the child's process died between its terminal commit and its
    /// receipt, or that run never had a store to publish into — and it is
    /// **correct** for those to stay unproved: the successor may yet be
    /// commissioned by a recovery, and this module has no way to know it will
    /// not be.
    ///
    /// **The one permanent population is the pre-existing one**, and an operator
    /// reading a first sweep after this landed should expect it: a child that
    /// ended `Success` under a build with no writer published no receipt and
    /// nothing publishes one retrospectively, so its parent reports this row
    /// forever. That is the same conservative shape
    /// [`ParkReason::ChildAddressNotCarried`] gives the address backlog, and for
    /// the same reason — a proof cannot be back-dated onto evidence that was
    /// never recorded.
    ChildChainNotClosed {
        child_execution_id: String,
        /// The segment whose terminal this pass read, and the address the
        /// missing receipt would have been published at.
        read_at: String,
    },
    /// More children than one pass will load. See [`MAX_CHILDREN_DIAGNOSED`].
    TooManyChildrenToDiagnose { children: usize },
    /// Parked on an external job that has not reported.
    ///
    /// Always [`ParkVerdict::Stalled`]. Whether a runner still exists for this
    /// job id is not a question this store can answer, and answering it anyway
    /// would be this module's one chance to be confidently wrong.
    JobHasNotReported { job_id: String },
    /// The run's own wall-clock deadline passed while it was parked.
    ///
    /// [`ParkVerdict::Unwakeable`], and it is the strongest proof this module
    /// issues — stronger than the children scan, because it does not depend on
    /// reading anything outside the run. A deadline-passed run is withheld by
    /// `list_runnable`, so no scan offers it; and a claim by key runs no phase
    /// either, because `advance_under_lease` answers about the park while the
    /// wait stands and commits the canonical timeout terminal once it does not. So the run
    /// cannot advance again **even if its wake resolves**, and whether anything
    /// would have woken it is not a question worth asking.
    ///
    /// **Stated exactly, because the near-miss is tempting:** the driver does
    /// *not* report a deadline on a parked run — its park guard is checked before
    /// its deadline guard, so `advance_once` on one answers
    /// `Advanced::Parked`. The run is stuck either way; this reason is the only
    /// place the deadline is named.
    ///
    /// **Retirable** under a [`Recovery::RetireProvedParks`] that names
    /// [`RetirableGround::DeadlinePassedWhileParked`], and it is the one ground
    /// whose retirement costs the store nothing extra: clearing the wait does
    /// not make the run visible to `list_runnable`, because that scan tests
    /// `deadline_passed` too.
    ///
    /// It is also the ground with no soundness caveat of its own, which is what
    /// the per-ground filter was added to let a caller act on by itself.
    DeadlinePassedWhileParked { deadline_at_ms: i64 },
    /// The wake resolved and the run was still parked when this pass looked.
    ///
    /// # What this row does NOT establish, said first because the name suggests it
    ///
    /// - **Not that the resolution is old.** A resolution carries no timestamp —
    ///   `wake_resolutions` answers a `Vec<String>` — and
    ///   [`StalledPark::parked_for_ms`] dates the **park commit**, not the
    ///   resolution. A completion that landed one second ago on a legitimately
    ///   long park is indistinguishable here from one that landed twelve hours
    ///   ago. So a twelve-hour `Job` park whose runner reports perfectly
    ///   normally is named by the very next pass, before any worker has napped
    ///   its way back round to claim it — a healthy run reported at the moment it
    ///   becomes healthy.
    /// - **Not that `list_runnable` would offer the run.** The wait is one
    ///   filter of several. `runnable_at` also tests
    ///   [`super::state::LoopState::claimable_by`], which is false for a run
    ///   pinned to a worker that is not the one asking — including a dead one,
    ///   for as long as its pin has left to run. That window is documented by
    ///   name on [`super::store::LoopStateStore::list_runnable`], and a run
    ///   inside it is committed, unleased, satisfied, and still offered to
    ///   nobody.
    ///
    /// # What it does establish
    ///
    /// Something published a resolution for this token and the run had not left
    /// its park. That is worth surfacing on its own — it is the one case where
    /// the store and the scheduler disagree, and answering *no live child* about
    /// it would name the wrong thing entirely — but it is
    /// [`ParkVerdict::Stalled`] and never a proof, precisely because both
    /// paragraphs above are consistent with a run that is about to advance.
    WakeResolvedButNotResumed { resolutions: usize },
}

/// What this pass DID about one finding, as distinct from what it found.
///
/// Separate from [`ParkVerdict`] on purpose. The verdict is a claim about the
/// run and is the same whether or not recovery is enabled; this is a claim about
/// the pass. Folding them into one enum would make a row's meaning depend on the
/// policy that produced it, and a reader comparing two passes could not tell a
/// park that became healthy from one the policy stopped acting on.
#[derive(Debug, Clone, PartialEq)]
pub enum ParkAction {
    /// Reported, and nothing else. Every row under [`Recovery::ReportOnly`], and
    /// every [`ParkVerdict::Stalled`] row under either policy.
    Reported,
    /// A non-resumable terminal was appended and the wait cleared, under the
    /// lease. The run will not be offered a phase again.
    ///
    /// Read *What a retirement asserts, and what it does not* in the module docs
    /// before concluding anything about the **work** from this.
    Retired { terminal: TerminalKind },
    /// Recovery was enabled and this row was not retired. Always says why.
    ///
    /// A row rather than a silence, because the two outcomes an operator must be
    /// able to tell apart are *"this was fixed"* and *"this could not be fixed
    /// and here is what stopped it"*, and a pass that reported the first and
    /// stayed quiet about the second would read as a clean sweep.
    NotRetired(RetirementRefused),
}

/// Why a proved park was not retired.
#[derive(Debug, Clone, PartialEq)]
pub enum RetirementRefused {
    /// This ground is **never** acted on, whatever a policy says. The reason is
    /// the [`ParkReason`] beside it in the same [`StalledPark`]; see the
    /// module's *Which grounds are acted on*.
    ///
    /// Distinct from [`Self::GroundNotOptedIn`], and kept distinct because the
    /// two answer different questions. This one is about **the ground's
    /// nature**: no policy can enable it, `terminal_for` has no terminal for it,
    /// and every future pass refuses it identically. The other is about **this
    /// pass's configuration**, and changes the moment somebody edits a policy.
    /// An operator reading a report has to be able to tell *we can never retire
    /// this* from *we chose not to, here* — only the second is a decision
    /// anybody can revisit.
    GroundIsNotActedOn,
    /// This ground is retirable and [`Recovery::RetireProvedParks`] did not name
    /// it. **Left exactly as it was**: reported, nothing claimed, nothing
    /// written.
    ///
    /// Which ground it was is not repeated here, for the same reason
    /// [`Self::GroundIsNotActedOn`] does not repeat it: it is the
    /// [`ParkReason`] beside it in the same [`StalledPark`], and a copy would be
    /// a second place for the two to disagree.
    GroundNotOptedIn,
    /// Another worker holds a live lease. **Left alone**, which is the whole
    /// point of claiming first: a reconciler that mutated a run another worker
    /// holds is the strand it exists to prevent.
    LeaseHeld { by: WorkerId, until_ms: i64 },
    /// The run is [`Placement::Pinned`] to a worker that is not this one.
    ///
    /// Checked after the claim, because `claim` cannot consult a placement that
    /// lives in the state the claim is taken in order to read — the same order
    /// `advance_under_lease` uses and for the same reason.
    PinnedElsewhere { pinned_to: WorkerId },
    /// The state under the lease is not the state the diagnosis judged.
    ///
    /// The park was left, or replaced, or the run was deleted, between the
    /// listing and the claim. Nothing is written: a proof about a state that has
    /// moved is not a proof about the state in front of us.
    StateMoved { detail: String },
    /// A completion landed before the terminal could be written.
    ///
    /// The third and last wake-ledger read, taken under the lease. Downgrading
    /// here costs one wasted claim; not reading would cost a woken run a
    /// permanent terminal.
    WakeResolvedFirst { resolutions: usize },
    /// The run's own log is not one this module may append to: it does not
    /// parse, its numbering is broken, its cursor is not the cursor it replays
    /// to, or it already records a non-resumable terminal.
    ///
    /// The same family `verify_journal` quarantines on, and refused for the same
    /// reason — appending onto a log a driver would refuse to load makes the
    /// damage permanent and moves it further from its cause.
    JournalUnusable { detail: String },
    /// The store could not answer. Fail closed; nothing is written.
    Store {
        during: &'static str,
        detail: String,
    },
    /// Somebody committed between the load and the commit.
    ///
    /// The compare-and-swap refused, so the terminal record this pass appended
    /// sits **above** the watermark as an orphan — never replayed, never
    /// projected, and swept by the next append. Losing the race costs a wasted
    /// append and nothing else.
    StaleCommit { expected: Revision, found: Revision },
}

/// One parked run this pass is reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct StalledPark {
    pub key: ExecutionKey,
    pub wait: WaitReason,
    pub verdict: ParkVerdict,
    pub reason: ParkReason,
    /// What this pass did about it. See [`ParkAction`].
    pub action: ParkAction,
    /// How long the park has been sitting, when the store could date it.
    ///
    /// `None` means the store could not say, **not** that the park is new — the
    /// pass examined it precisely because an undatable park cannot be shown to be
    /// young. See [`ParkedExecution::parked_since_ms`].
    pub parked_for_ms: Option<i64>,
    /// The last lease this run held. Read [`LastLease::expires_at_ms`]'s own
    /// documentation before concluding anything from it: a released lease carries
    /// no timestamp.
    pub last_lease: Option<LastLease>,
}

/// What one pass found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReconcileReport {
    /// Parked runs seen.
    pub parked: usize,
    /// Where the scheduler should begin the next bounded pass. `None` means
    /// this page reached the end and the next round starts from the beginning.
    pub resume: Option<ScanCursor>,
    /// Parked runs this pass actually judged.
    ///
    /// A park is judged when it is past its grace — or, whatever its age, when
    /// its own deadline has passed, which is decided from the listing and needs
    /// no patience. Everything else is counted in [`Self::parked`] and left
    /// alone, and the gap between the two numbers is what the age gate bought.
    pub examined: usize,
    /// Parks this pass retired.
    ///
    /// Always `0` under [`Recovery::ReportOnly`], and always `0` under a
    /// [`Recovery::RetireProvedParks`] whose [`RetirableGround`] set is empty —
    /// two different policies with one number, which is why the per-row
    /// [`ParkAction`] and not this counter is what says *why*. Counted separately from
    /// `findings.len()` because a retired park is still a finding — the row is
    /// what says a run was ended — and an operator needs to know how much of a
    /// long list is already dealt with.
    pub retired: usize,
    pub findings: Vec<StalledPark>,
    /// **This pass did not see everything.** The listing stopped short, or a run
    /// could not be read, or a diagnosis was abandoned because the store would
    /// not answer.
    ///
    /// A caller must not read an empty `findings` as "nothing is stuck" while
    /// this is set. That is why it travels with the report rather than being
    /// logged and dropped.
    pub incomplete: bool,
}

/// Reads the store, judges parks, and — only under [`Recovery::RetireProvedParks`],
/// only on a proved ground, and only on one that policy named — retires one.
///
/// # Which store methods it uses, split by policy
///
/// **Always, and only these, all reads:** `scan_parked`, `wake_resolutions`,
/// `load`, `read_journal`.
///
/// **Additionally when this reconciler retires a diagnosed park:** `claim`,
/// `load_projector_cursor`, `append_journal`, `commit`, `release` — in that
/// order, per park, and never `renew`, `resolve_wake` or `consume_wake`. The
/// cursor read only rejects impossible ahead-of-watermark authority; debt below
/// the watermark is preserved for the terminal projector. The separate lifecycle API
/// [`prepare_children_handoff`] deliberately uses a terminal-first wake receipt;
/// it is not a diagnosis or an inferred completion.
///
/// The last three are the ones a retirement must never reach and the reasons are
/// not the same:
///
/// - `renew` is unnecessary: a retirement is four store calls with no phase
///   between them, so a lease it could not finish inside is a store that is not
///   answering, and the answer to that is to stop rather than to hold longer.
/// - `resolve_wake` publishes a completion this module did not observe. See the
///   module docs' third refused recovery.
/// - `consume_wake` **removes** a resolution, and `wake/` is the one place a
///   writer without the lease publishes: dropping an id there can lose a real
///   completion. A retired park may therefore leave an unconsumed resolution
///   behind — only on the deadline ground, which does not read the ledger — and
///   that litter is deliberate. It satisfies nothing, because the run it belongs
///   to can no longer take a phase.
///
/// None of this is enforceable by the type, since the trait carries every method,
/// so it is enforced by tests: `a_report_only_pass_writes_nothing` runs a full
/// pass against a store that panics on every mutating call, and
/// `a_retirement_never_touches_the_wake_ledger` keeps the three above unreached
/// on the acting path.
pub struct LoopReconciler<'a> {
    store: &'a dyn LoopStateStore,
    policy: ReconcilerPolicy,
}

impl<'a> LoopReconciler<'a> {
    pub fn new(store: &'a dyn LoopStateStore, policy: ReconcilerPolicy) -> Self {
        Self { store, policy }
    }

    /// One pass, against the wall clock.
    pub async fn reconcile(&self) -> StoreResult<ReconcileReport> {
        self.reconcile_at(Utc::now().timestamp_millis()).await
    }

    /// One pass, against `now_ms`.
    ///
    /// The clock is a parameter for the same reason `advance_under_lease` reads
    /// one clock for a whole cycle: every age decision in a pass must be taken
    /// against one instant, or two parks judged milliseconds apart can disagree
    /// about which side of the grace they are on. It also makes the grace period
    /// testable without sleeping through it.
    pub async fn reconcile_at(&self, now_ms: i64) -> StoreResult<ReconcileReport> {
        // A listing that cannot be produced fails the pass. This is the one place
        // that fails closed rather than degrading: with no listing there is
        // nothing to be incomplete *about*, and an empty report would be a claim
        // that the store holds no stuck parks.
        // A zero-sized window can never advance its cursor and would turn a
        // configuration typo into permanent tail starvation.  Keep the scan
        // bounded, but always let it inspect at least one execution.
        let scan_limit = self.policy.scan_limit.max(1);
        let listing = self
            .store
            .scan_parked(scan_limit, self.policy.scan_after.as_ref())
            .await?;

        let mut report = ReconcileReport {
            parked: listing.parked.len(),
            resume: listing.resume.clone(),
            incomplete: listing.incomplete,
            ..ReconcileReport::default()
        };

        for parked in &listing.parked {
            let parked_for_ms = parked
                .parked_since_ms
                .map(|since| now_ms.saturating_sub(since));

            // Ahead of the age gate, deliberately, and it costs nothing: the
            // deadline came with the listing. A park past its own deadline can
            // never advance again whatever happens to its wake, so waiting out a
            // grace period would make the one run that is already provably dead
            // the last one reported — and for a job park that wait is twelve
            // hours.
            if let Some(deadline_at_ms) = parked.deadline_at_ms.filter(|at| *at <= now_ms) {
                report.examined += 1;
                let reason = ParkReason::DeadlinePassedWhileParked { deadline_at_ms };
                let action = self
                    .act_on(parked, ParkVerdict::Unwakeable, &reason, now_ms)
                    .await;
                push_finding(
                    &mut report,
                    parked,
                    parked_for_ms,
                    ParkVerdict::Unwakeable,
                    reason,
                    action,
                );
                continue;
            }

            // The age gate, and it is the only thing between this pass and a read
            // per park per pass. An UNDATED park is examined: a store that could
            // not date one has not shown it to be young, and reading "unknown" as
            // "new" would hide exactly the parks whose directories are in a state
            // worth looking at.
            if parked_for_ms.is_some_and(|age| age < self.policy.grace_ms_for(&parked.wait)) {
                continue;
            }
            report.examined += 1;

            let diagnosis = match self.diagnose(parked).await {
                Ok(diagnosis) => diagnosis,
                // One park the store will not answer about must not fail the pass
                // — the other findings are real and an operator needs them — but
                // it must not vanish either, so the report stops claiming it saw
                // everything.
                Err(error) => {
                    warn!(
                        execution = %parked.key,
                        %error,
                        "[LOOP-RECONCILER] a parked run could not be diagnosed; this pass is \
                         incomplete"
                    );
                    report.incomplete = true;
                    continue;
                },
            };
            let Some((verdict, reason)) = diagnosis else {
                continue;
            };
            let action = self.act_on(parked, verdict, &reason, now_ms).await;
            push_finding(&mut report, parked, parked_for_ms, verdict, reason, action);
        }

        report.retired = report
            .findings
            .iter()
            .filter(|finding| matches!(finding.action, ParkAction::Retired { .. }))
            .count();
        Ok(report)
    }

    /// Decide what to do about one finding, and do it.
    ///
    /// Three gates before anything is claimed, and they are separate on purpose:
    ///
    /// 1. **The policy writes at all.** [`Recovery::ReportOnly`] returns
    ///    [`ParkAction::Reported`] for every row, so a report-only pass is
    ///    read-only by construction rather than by remembering not to write.
    /// 2. **The verdict and the ground.** Only [`ParkVerdict::Unwakeable`] is
    ///    acted on, and only two of its three grounds — a `Stalled` row is
    ///    reported without even naming a refusal, because "recovery declined" is
    ///    not news about a park nothing claimed to have proved.
    /// 3. **The policy named THIS ground.** A retirable ground the caller did
    ///    not opt into is refused [`RetirementRefused::GroundNotOptedIn`], which
    ///    is a different row from the one the gate above writes: that one says a
    ///    ground can never be retired, this one says a ground was not retired
    ///    *here*. Separate because the two retirable grounds are not equally
    ///    sound — the children ground rests on an address the refinement key
    ///    axis broke — and one switch over both meant the sound half could only
    ///    be enabled by enabling the unsound half beside it.
    async fn act_on(
        &self,
        parked: &ParkedExecution,
        verdict: ParkVerdict,
        reason: &ParkReason,
        now_ms: i64,
    ) -> ParkAction {
        let Recovery::RetireProvedParks { worker, grounds } = &self.policy.recovery else {
            return ParkAction::Reported;
        };
        if verdict != ParkVerdict::Unwakeable {
            return ParkAction::Reported;
        }
        let Some(terminal) = terminal_for(reason) else {
            return ParkAction::NotRetired(RetirementRefused::GroundIsNotActedOn);
        };
        // The filter, and it is deliberately the LAST gate before the claim: a
        // ground this policy declined is still diagnosed, still verdicted and
        // still reported, so switching a ground off costs the findings nothing.
        // The only way to suppress one ground before this existed was to stretch
        // `park_grace` until those parks were never examined, which loses the
        // `Stalled` rows beside them too.
        if !RetirableGround::of(reason).is_some_and(|ground| grounds.contains(&ground)) {
            return ParkAction::NotRetired(RetirementRefused::GroundNotOptedIn);
        }
        self.retire(parked, reason, terminal, worker.as_worker_id(), now_ms)
            .await
    }

    /// Take the lease, retire under it, give it back.
    ///
    /// The bracket is its own function for the same reason
    /// `advance_once`/`advance_under_lease` are two: every refusal below has to
    /// release, and a guard that returned without releasing would hold an
    /// execution for a whole TTL over a condition the next pass would re-decide
    /// in milliseconds.
    async fn retire(
        &self,
        parked: &ParkedExecution,
        reason: &ParkReason,
        terminal: TerminalKind,
        worker: &WorkerId,
        now_ms: i64,
    ) -> ParkAction {
        let lease = match self
            .store
            .claim(&parked.key, worker, RETIREMENT_LEASE_TTL)
            .await
        {
            Ok(lease) => lease,
            // Not a fault and not retried. Something holds this run; whatever it
            // is has a better claim to decide its fate than a reconciler does.
            Err(StoreError::LeaseHeld { by, until_ms }) => {
                return ParkAction::NotRetired(RetirementRefused::LeaseHeld { by, until_ms })
            },
            Err(error) => {
                return ParkAction::NotRetired(RetirementRefused::Store {
                    during: "claim",
                    detail: error.to_string(),
                })
            },
        };

        let action = self
            .retire_under_lease(parked, reason, terminal, worker, &lease, now_ms)
            .await;

        // A release that fails leaves the lease to expire, which is where a dead
        // holder leaves it and is already handled. Logged rather than folded into
        // the action, because it changes nothing a caller would do differently —
        // the same position `advance_once` takes.
        if let Err(error) = self.store.release(lease).await {
            warn!(
                execution = %parked.key,
                worker = %worker,
                %error,
                "[LOOP-RECONCILER] releasing the retirement lease failed; it will expire instead"
            );
        }
        action
    }

    /// Append the terminal and commit the cleared wait, or refuse and write
    /// nothing.
    ///
    /// # The order is append-then-commit, and the failure directions are not
    /// symmetric
    ///
    /// A crash between the two leaves the terminal record **above** the
    /// watermark, where it is an orphaned attempt: never replayed, never
    /// projected, swept by the next append. The park is intact and the next pass
    /// re-diagnoses it. The other order — commit the cleared wait, then append —
    /// would leave a run un-parked with no terminal, which is exactly the
    /// unbounded re-park the module's first refused recovery describes. So the
    /// order is the same one `commit_boundary` uses, for the same reason.
    ///
    /// # Everything that can refuse happens before the append
    ///
    /// Five re-checks, all under the lease, because the diagnosis was taken
    /// before the claim and the world moves in between:
    ///
    /// 1. **The state is still the state judged** — still committed, still
    ///    parked, still parked on the same wait.
    /// 2. **The placement admits this worker.** `claim` cannot check it: the
    ///    placement lives in the state the claim is taken in order to read.
    /// 3. **The proof still holds.** For the deadline ground that is the state's
    ///    own field; for the children ground it is the wake ledger, read a third
    ///    time and read **last**, so a completion landing during the diagnosis
    ///    costs a wasted claim rather than a woken run's permanent terminal.
    /// 4. **The log is one a driver would load.** It parses, its numbering is
    ///    intact, the committed cursor is the cursor it replays to, and it does
    ///    not already end non-resumably. Appending onto a log `verify_journal`
    ///    would quarantine makes the damage permanent and moves it away from its
    ///    cause.
    /// 5. **The durable projector cursor is not ahead of that log.** Debt below
    ///    the watermark is safe and intentionally preserved for the terminal
    ///    owner; a cursor beyond the watermark is corrupt authority and fails
    ///    closed before retirement.
    /// Once the terminal commit lands, the placement-independent terminal
    /// outbox owner discovers the ending marker and drains every accepted event
    /// still above the projector cursor. Retirement therefore preserves the
    /// existing cursor and does not wait for a phase host that a parked run does
    /// not have.
    ///
    /// # What it does NOT do, and why each absence is deliberate
    ///
    /// - **It does not project.** [`super::state::LoopState::for_commit`] needs
    ///   an `AgenticContext`, which is a live run's and which a reconciler has
    ///   no business holding one of. Skipping it is sound **only because of what
    ///   this commit changes**: `wait` and `journal_seq`, neither of which the
    ///   projection reads. It must not grow to touch `placement`,
    ///   `identity.browser_transports` or `work_budget_consumed_ms` — those are
    ///   what the projection exists to police, and the second commit site that
    ///   forgets the fold is the regression `for_commit`'s own docs predict.
    ///   The terminal-outbox lifecycle is what makes this safe for event
    ///   delivery: the ending marker makes any earlier unprojected prefix
    ///   independently discoverable after this commit. Carrying the loaded
    ///   values forward untouched is also the *correct*
    ///   accounting: no work was done, no secret is held, so the budget must not
    ///   move and the pin must be allowed to decay.
    /// - **It does not move the cursor.** `next_cursor` answers `current` for a
    ///   `RunEnded` step, and `replay_each` sets the cursor to the record's own
    ///   `(iteration, phase)`. Writing the record at the committed cursor is
    ///   therefore the one address at which the state and its journal still
    ///   agree afterwards — any other and the next `verify_journal` quarantines
    ///   the run for a divergence this pass created.
    /// - **It does not touch `wake/`.** See the type's own docs.
    async fn retire_under_lease(
        &self,
        parked: &ParkedExecution,
        reason: &ParkReason,
        terminal: TerminalKind,
        worker: &WorkerId,
        lease: &super::store::Lease,
        now_ms: i64,
    ) -> ParkAction {
        let key = &parked.key;

        // ── 1. STILL THE STATE THIS PASS JUDGED ─────────────────────────────
        let committed = match self.store.load(key).await {
            Ok(Some(committed)) => committed,
            Ok(None) => {
                return moved("nothing is committed under this key any more");
            },
            Err(error) => {
                return ParkAction::NotRetired(RetirementRefused::Store {
                    during: "load",
                    detail: error.to_string(),
                })
            },
        };
        let revision = committed.revision;
        let mut state = committed.state;

        let Some(wait) = state.wait.clone() else {
            return moved("the run left its park between the listing and the lease");
        };
        if wait != parked.wait {
            // Compared by value rather than by token: two different child sets
            // can share a token only if they are the same sorted set, but a park
            // that changed from `Job` to `Children` or swapped its ids is a
            // different park and the diagnosis does not carry over.
            return moved("the run re-parked on a different wait");
        }

        // ── 2. PLACEMENT ────────────────────────────────────────────────────
        if !state.claimable_by(worker, now_ms) {
            let pinned_to = match &state.placement {
                Placement::Pinned { worker: owner, .. } => owner.clone(),
                // Not reachable: `claimable_by` is true for every `Portable`
                // state. Named rather than `unreachable!` for the reason
                // `advance_under_lease` names its twin — the cost of being wrong
                // is a panic inside a holder of a lease.
                Placement::Portable => worker.clone(),
            };
            return ParkAction::NotRetired(RetirementRefused::PinnedElsewhere { pinned_to });
        }

        // ── 3. THE PROOF, RE-TAKEN ──────────────────────────────────────────
        //
        // Against the PASS's clock, not a fresh one, which is the same discipline
        // `advance_under_lease` follows and here it is also the conservative
        // direction: a long pass makes `now_ms` stale, and a stale-early clock
        // reads a deadline that has since passed as *not* passed and a pin that
        // has since lapsed as *still live*. Both refuse. A fresh clock would make
        // the second of those act.
        match reason {
            ParkReason::DeadlinePassedWhileParked { .. } => {
                if !state.deadline_passed(now_ms) {
                    return moved(
                        "the deadline this pass proved is not on the state under the lease",
                    );
                }
            },
            ParkReason::EveryChildHasFinished { .. } => {
                // The LAST read before the write. The children are not re-read:
                // they are other executions and this lease does not cover them,
                // so a second scan of them would cost n reads and prove nothing
                // it did not already. Their chain-closure receipts are not
                // re-read either, and that is a stronger statement: a receipt is
                // published once and never retracted, so the only way the
                // diagnosis's reading of one can go stale is a receipt
                // APPEARING, which turns a refusal into a proof and cannot
                // arrive in the direction that matters. The ledger is different
                // — it is the one place a completer publishes for THIS run, and
                // the window between the diagnosis and here is the whole
                // diagnosis.
                match self.store.wake_resolutions(key, &wait.wake_token()).await {
                    Ok(resolutions) if !resolutions.is_empty() => {
                        return ParkAction::NotRetired(RetirementRefused::WakeResolvedFirst {
                            resolutions: resolutions.len(),
                        })
                    },
                    Ok(_) => {},
                    Err(error) => {
                        return ParkAction::NotRetired(RetirementRefused::Store {
                            during: "wake_resolutions",
                            detail: error.to_string(),
                        })
                    },
                }
            },
            // Unreachable while this match and `terminal_for` name the same two
            // grounds. Spelled out rather than `_`, and the guarantee that buys
            // is narrower than it looks, so it is stated exactly:
            //
            // - A **new** `ParkReason` variant fails to compile here *and* in
            //   `RetirableGround::of`, which `terminal_for` is now a projection
            //   of, so it cannot be added without an answer in both.
            // - Moving an **existing** ground into `RetirableGround::of`'s
            //   retirable set and not here still compiles, and lands on this
            //   arm: the row reports `GroundIsNotActedOn` for a ground somebody
            //   just declared retirable. That is the safe direction and it is
            //   visible in the report, but it is a refusal, not a build error.
            //   Do not read the exhaustiveness as covering it.
            //
            // The per-ground filter neither weakens this nor widens it. A ground
            // the policy declined never reaches this function at all — `act_on`
            // answers `GroundNotOptedIn` before the claim — so these arms are
            // reached by exactly the grounds they were before, less the ones
            // nobody opted into.
            ParkReason::NoChildrenNamed
            | ParkReason::ChildNeverCommitted { .. }
            | ParkReason::ChildUnreadable { .. }
            | ParkReason::ChildUnaddressable { .. }
            | ParkReason::ChildMayStillBeContinued { .. }
            | ParkReason::ChildAddressNotCarried { .. }
            | ParkReason::ChildChainNotClosed { .. }
            | ParkReason::TooManyChildrenToDiagnose { .. }
            | ParkReason::JobHasNotReported { .. }
            | ParkReason::WakeResolvedButNotResumed { .. } => {
                return ParkAction::NotRetired(RetirementRefused::GroundIsNotActedOn)
            },
        }

        // ── 4. A LOG A DRIVER WOULD LOAD ────────────────────────────────────
        let records = match self.store.read_journal(key, 0).await {
            Ok(records) => records,
            Err(error) => {
                return ParkAction::NotRetired(RetirementRefused::Store {
                    during: "read_journal",
                    detail: error.to_string(),
                })
            },
        };
        let journal = match Journal::from_records(records) {
            Ok(journal) => journal,
            Err(error) => return unusable(error.to_string()),
        };
        // Bounded by the watermark, exactly as `verify_journal` is: a terminal a
        // dead worker left above it was never vouched for by anything, and a
        // cursor replayed through it is not the committed one.
        let replayed = match replay(journal.authoritative(state.journal_seq)) {
            Ok(replayed) => replayed,
            Err(error) => return unusable(error.to_string()),
        };
        if replayed.iteration != state.cursor.iteration || replayed.phase != state.cursor.phase {
            return unusable(format!(
                "the committed cursor {:?} is not the cursor its own journal replays to \
                 (iteration {}, {:?}) at watermark {}",
                state.cursor, replayed.iteration, replayed.phase, state.journal_seq
            ));
        }
        if let Some(recorded) = replayed.terminal {
            if !recorded.is_resumable() {
                // Already over. Appending would put a record after a
                // non-resumable terminal, which `replay_each` refuses on the
                // next read — the run would be retired once and then be
                // permanently unloadable.
                return unusable(format!(
                    "this run already records the non-resumable terminal {recorded:?}"
                ));
            }
        }

        // ── 5. A PROJECTOR CURSOR THIS TERMINAL OWNER CAN ADVANCE ───────────
        // Owed events are not a refusal: committing the ending marker is what
        // makes them discoverable to the host-free terminal projector. A mark
        // beyond the committed prefix is different; saving another terminal
        // above corrupt cursor authority would make an irreversible retirement
        // whose delivery owner must refuse forever.
        let projector_cursor = match self.store.load_projector_cursor(key).await {
            Ok(cursor) => cursor,
            Err(error) => {
                return ParkAction::NotRetired(RetirementRefused::Store {
                    during: "load_projector_cursor",
                    detail: error.to_string(),
                })
            },
        };
        let projected_through = projector_cursor
            .as_ref()
            .map(ProjectorCursor::emitted_through_seq)
            .unwrap_or(0);
        if projected_through > state.journal_seq {
            return unusable(format!(
                "the projector mark {projected_through} is ahead of the committed journal \
                 watermark {}",
                state.journal_seq
            ));
        }
        if projector_cursor
            .as_ref()
            .and_then(ProjectorCursor::runtime_settled_terminal_seq)
            .is_some_and(|settled| settled > state.journal_seq)
        {
            return unusable(format!(
                "the runtime-settlement mark is ahead of the committed journal watermark {}",
                state.journal_seq
            ));
        }

        // ── APPEND, THEN COMMIT ─────────────────────────────────────────────
        let seq = match self
            .store
            .append_journal_fenced(
                key,
                &[JournalAppend::phase_completed(
                    state.cursor.iteration,
                    state.cursor.phase,
                    RecordedStep::RunEnded { terminal },
                )],
                lease,
            )
            .await
        {
            Ok(seq) => seq,
            Err(error) => {
                return ParkAction::NotRetired(RetirementRefused::Store {
                    during: "append_journal",
                    detail: error.to_string(),
                })
            },
        };
        state.journal_seq = seq;
        state.wait = None;

        match self.store.commit_fenced(key, &state, revision, lease).await {
            Ok(_) => ParkAction::Retired { terminal },
            Err(StoreError::Conflict { expected, found }) => {
                ParkAction::NotRetired(RetirementRefused::StaleCommit { expected, found })
            },
            Err(error) => ParkAction::NotRetired(RetirementRefused::Store {
                during: "commit",
                detail: error.to_string(),
            }),
        }
    }

    /// Judge one park. `None` means it is healthy and nothing is reported.
    async fn diagnose(
        &self,
        parked: &ParkedExecution,
    ) -> StoreResult<Option<(ParkVerdict, ParkReason)>> {
        let token = parked.wait.wake_token();

        // Cheapest question first: is the park already satisfied? A run with an
        // outstanding resolution is not waiting on anything — `list_runnable`
        // offers it — so if it is still sitting here past the grace, the stall is
        // downstream of the wake, and saying "no live child" about it would name
        // the wrong thing entirely.
        let resolutions = self
            .store
            .wake_resolutions(&parked.key, &token)
            .await?
            .len();
        if resolutions > 0 {
            return Ok(Some((
                ParkVerdict::Stalled,
                ParkReason::WakeResolvedButNotResumed { resolutions },
            )));
        }

        let candidate = match &parked.wait {
            WaitReason::Job { job_id } => Some((
                ParkVerdict::Stalled,
                ParkReason::JobHasNotReported {
                    job_id: job_id.clone(),
                },
            )),
            WaitReason::Children {
                child_execution_ids,
                resume,
            } => {
                self.diagnose_children(&parked.key, child_execution_ids, resume)
                    .await?
            },
        };

        let reason = match candidate {
            Some((ParkVerdict::Unwakeable, reason)) => reason,
            healthy_or_suspected => return Ok(healthy_or_suspected),
        };

        // The ledger is read again, AFTER the children were judged, and a proof
        // is issued only if it is still empty. Between the first read and here
        // this pass performed one `load` and one journal read per child, and a
        // completion landing in that window would make the first read's answer
        // stale — so the proof is taken from the read that happens last. It is
        // the same discipline `leave_park` follows when it commits before it
        // consumes: the cheap wrong direction is one extra round, and the
        // expensive wrong direction is a claim that cannot be taken back.
        let late = self
            .store
            .wake_resolutions(&parked.key, &token)
            .await?
            .len();
        if late > 0 {
            return Ok(Some((
                ParkVerdict::Stalled,
                ParkReason::WakeResolvedButNotResumed { resolutions: late },
            )));
        }
        Ok(Some((ParkVerdict::Unwakeable, reason)))
    }

    /// Judge a `Children` park by reading the children.
    ///
    /// # Every child is looked at, and one live child outranks every fault
    ///
    /// The loop below does not return on the first child it cannot judge. It
    /// holds the fault and carries on, so that a live child found later still
    /// wins and the park is left alone — which is what `ChildNeverCommitted`
    /// already did, and what an earlier version did not do for
    /// [`ParkReason::ChildUnreadable`] and [`ParkReason::ChildUnaddressable`]:
    /// those returned immediately, so a park with one unreadable child and one
    /// demonstrably live one was reported `Stalled`. That is a finding about a
    /// healthy park, in the bucket the two-verdict split exists to keep clean.
    ///
    /// A held fault still outranks `Finished` and `NeverCommitted`, and it still
    /// stops the [`ParkReason::EveryChildHasFinished`] proof. Nothing that could
    /// not be read may ever be counted as a child that ended.
    ///
    /// [`ChildState::MayStillBeContinued`] is held on exactly the same terms and
    /// for the same reason — it is a child this pass could not establish is
    /// unable to report — even though the fault is in the vocabulary rather than
    /// in the store. See [`may_still_be_continued`].
    ///
    /// [`ChildState::AddressNotCarried`] is the third member of that family and
    /// the one an old park produces; [`ChildState::ChainNotClosed`] is the
    /// fourth and the one a pass looking into the seconds between a segment's
    /// terminal and its successor's first commit produces. See
    /// [`Self::child_state`].
    async fn diagnose_children(
        &self,
        parent: &ExecutionKey,
        child_execution_ids: &[String],
        resume: &ResumeAddress,
    ) -> StoreResult<Option<(ParkVerdict, ParkReason)>> {
        if child_execution_ids.is_empty() {
            return Ok(Some((ParkVerdict::Unwakeable, ParkReason::NoChildrenNamed)));
        }
        if child_execution_ids.len() > MAX_CHILDREN_DIAGNOSED {
            // Before the loop rather than inside it, so this is a refusal to
            // diagnose the park at all rather than a partial scan: no child is
            // read, and no live child can be found to overrule it.
            return Ok(Some((
                ParkVerdict::Stalled,
                ParkReason::TooManyChildrenToDiagnose {
                    children: child_execution_ids.len(),
                },
            )));
        }

        let mut never_committed = Vec::new();
        // The first child this pass could not judge. See the doc above for why
        // it is held rather than returned.
        let mut unjudged: Option<ParkReason> = None;
        for child_execution_id in child_execution_ids {
            // A child is addressed in its parent's SCOPE, and along the segment
            // chain the park names.
            //
            // THE SCOPE, unchanged. The design calls a child *"another
            // `LoopState` in the same store"*, and a `WaitReason::Children`
            // carries no scope beside its ids, so there is no other scope
            // available to look in. A child placed in a different scope reads as
            // absent here, which downgrades the verdict to
            // `ChildNeverCommitted` rather than producing a false proof, because
            // that is the arm an unfound child takes. Safe direction.
            //
            // THE ID, which is what changed. `StatelessArm::new` does not key a
            // run by its bare execution id: it keys it by
            // `loop_state_execution_id`, which folds a resume generation, a
            // nesting chain and a **refinement pass** into the id. A bare id
            // therefore addresses only a child's pass 0, and a child run through
            // `execute_agentically_with_refinement` commits pass 0's terminal
            // there and goes on working under `{child}-p1`.
            //
            // That axis is now followed rather than written down, because the
            // park carries the chain: `executor.rs`'s `child_segment_addresses`
            // spells every key the child's work may be at, with the one function
            // that spells segment keys, and `ResumeAddress` carries it here. See
            // `Self::child_state` for what the walk proves and what it refuses
            // to prove.
            //
            // THE FALLBACK, and it is deliberately the weak one. A park
            // committed before the address existed names no chain, and the bare
            // id is the only address recoverable from it. `complete: false` says
            // so, and `child_state` then refuses to prove a `Success` child
            // finished at all — because an unstated chain cannot rule out the
            // segment it cannot see. Old parks therefore get a *suspicion* where
            // they used to get a false proof.
            //
            // THE `-r{n}` AXIS IS STILL NOT WALKED, and still cannot be:
            // `iteration_offset` is *prior offset + prior iterations*, so
            // generations are strictly increasing but NOT contiguous — `-r3` can
            // follow the bare id with no `-r1` or `-r2` between — no parent can
            // know at park time which one a child will take, and
            // `LoopStateStore` exposes no way to enumerate keys by prefix. It
            // costs a FALSE NEGATIVE: a resumed child leaves a *resumable*
            // terminal at the key it left, which reads as `Live`, so the park is
            // left alone. That direction hides findings rather than inventing
            // them, which is why closing the `-p` axis alone is what makes the
            // ground sound.
            let (segments, complete) = match resume.segments_for(child_execution_id) {
                Some(segments) => (segments.to_vec(), true),
                None => (vec![child_execution_id.clone()], false),
            };
            let mut chain = Vec::with_capacity(segments.len());
            let mut unaddressable = None;
            for segment in &segments {
                match ExecutionKey::new(parent.principal(), parent.workspace(), segment.clone()) {
                    Ok(key) => chain.push(key),
                    Err(error) => {
                        // Named with the SEGMENT that was refused, not only the
                        // child. A chain is several keys and an operator handed
                        // the bare id would go looking at the one address that
                        // was fine.
                        unaddressable = Some(ParkReason::ChildUnaddressable {
                            child_execution_id: child_execution_id.clone(),
                            detail: format!("segment `{segment}`: {error}"),
                        });
                        break;
                    },
                }
            }
            if let Some(reason) = unaddressable {
                // Held rather than returned, on the same terms as every other
                // fault: a live child found later still wins. One unspellable
                // segment stops the proof for this child and nothing more.
                unjudged.get_or_insert(reason);
                continue;
            }

            match self.child_state(&chain, complete).await {
                // One live child is the whole answer: the park is legitimate and
                // there is nothing to report, however long it has been waiting.
                Ok(ChildState::Live) => {
                    if let Some(unjudged) = unjudged.as_ref() {
                        // The park is healthy, so this is not a finding. But a
                        // child the store would not answer about is a fault
                        // whether or not its parent is stuck, and dropping it in
                        // silence is how it goes unnoticed.
                        warn!(
                            execution = %parent,
                            reason = ?unjudged,
                            "[LOOP-RECONCILER] a child could not be judged; its parent has \
                             another live child, so the park is not a stalled park"
                        );
                    }
                    return Ok(None);
                },
                Ok(ChildState::Finished) => {},
                // Held as a fault for the same reason an unreadable child is:
                // this pass could not establish that the child cannot report,
                // and a proof needs that about EVERY child. Held rather than
                // returned, so a live child found later still wins.
                Ok(ChildState::MayStillBeContinued { terminal }) => {
                    unjudged.get_or_insert(ParkReason::ChildMayStillBeContinued {
                        child_execution_id: child_execution_id.clone(),
                        terminal,
                    });
                },
                // The third fault, held on the same terms — and the one an old
                // park produces. `at` indexes the chain this pass built, so the
                // address is read back out of `segments` rather than re-spelled
                // here: this module does not know how to spell a segment key and
                // must not learn.
                Ok(ChildState::AddressNotCarried { at }) => {
                    unjudged.get_or_insert(ParkReason::ChildAddressNotCarried {
                        child_execution_id: child_execution_id.clone(),
                        read_at: segments[at].clone(),
                    });
                },
                // The fourth fault, held on the same terms — and the one a park
                // judged during the window between a segment's terminal and its
                // successor's first commit produces. Held rather than returned,
                // so a live child found later still wins.
                Ok(ChildState::ChainNotClosed { at }) => {
                    unjudged.get_or_insert(ParkReason::ChildChainNotClosed {
                        child_execution_id: child_execution_id.clone(),
                        read_at: segments[at].clone(),
                    });
                },
                Ok(ChildState::NeverCommitted) => {
                    never_committed.push(child_execution_id.clone());
                },
                // A child that cannot be read is surfaced rather than counted as
                // either finished or live. Counting it as finished would let an
                // unreadable journal manufacture a proof.
                Err(error) => {
                    unjudged.get_or_insert(ParkReason::ChildUnreadable {
                        child_execution_id: child_execution_id.clone(),
                        detail: error.to_string(),
                    });
                },
            }
        }

        if let Some(unjudged) = unjudged {
            return Ok(Some((ParkVerdict::Stalled, unjudged)));
        }
        if !never_committed.is_empty() {
            return Ok(Some((
                ParkVerdict::Stalled,
                ParkReason::ChildNeverCommitted {
                    child_execution_ids: never_committed,
                },
            )));
        }
        Ok(Some((
            ParkVerdict::Unwakeable,
            ParkReason::EveryChildHasFinished {
                children: child_execution_ids.len(),
            },
        )))
    }

    /// Whether one child is still capable of resolving its parent's token.
    ///
    /// # It judges a CHAIN, and that is the whole fix
    ///
    /// This used to take one `ExecutionKey` — a child's bare execution id — and
    /// answer from the records under it. A child run through
    /// `execute_agentically_with_refinement` commits pass 0's
    /// `RunEnded { Success }` under exactly that key and then does its remaining
    /// work under `{child}-p1`, so for the whole of pass 1 the bare key answered
    /// `Finished` about a child that was working, `EveryChildHasFinished`
    /// became a proof about a live delegation, and past the fifteen-minute
    /// [`DEFAULT_PARK_GRACE`] — which a refinement pass routinely outlives —
    /// that proof was retirable.
    ///
    /// It now takes the whole chain the park names, nearest-first, and judges
    /// the child by the **last segment that committed anything**.
    ///
    /// # The five answers, and which evidence earns each
    ///
    /// - **`Live` beats everything.** One live segment anywhere in the chain and
    ///   the child can still report, whatever any other segment recorded.
    ///   Checked during the walk, so a child whose pass 0 ended and whose pass 1
    ///   is running is live rather than finished. **This is the case the whole
    ///   change exists for** — it is the answer the bare-key read could not
    ///   produce, because it never looked at `{child}-p1`.
    /// - **`Finished` when nothing can follow the last committed terminal.** Two
    ///   ways nothing can, and only these two:
    ///   1. **The terminal is not `Success`.** A refinement pass is commissioned
    ///      by `refinement_gaps_if_warranted`, whose last guard is
    ///      `let AgenticOutcome::Success { .. } = outcome else { return None }`.
    ///      So a child that ended `Failed`, `LoopDetected` or `CannotProceed`
    ///      has nothing that could be commissioned after it, at any address —
    ///      which holds under an unstated chain too, and is what keeps an old
    ///      park provable for the children that failed.
    ///   2. **The terminal is `Success`, every address a pass could run under
    ///      was READ and was empty, AND the segment that ended published a
    ///      receipt saying nothing follows it.** The first half needs
    ///      `complete`: a stated chain names those addresses and the walk above
    ///      read all of them. The second half is the subject of the next
    ///      section, and it is not optional — without it this arm is a guess.
    /// - **`AddressNotCarried` when the terminal is `Success` and the chain is
    ///   unstated.** There was nowhere to look, so *nothing there* is not
    ///   evidence — it is the absence of a reading. A one-entry chain that looks
    ///   exhausted is exactly the shape the old false proof had, and refusing to
    ///   prove over it is the whole degradation story for parks committed before
    ///   the address existed.
    /// - **`ChainNotClosed` when the terminal is `Success`, the chain is stated
    ///   and exhausted, and no receipt says so.** The next section.
    /// - **`MayStillBeContinued`** is unchanged and is about the terminal's
    ///   vocabulary rather than its address. See [`may_still_be_continued`].
    ///
    /// # Why an exhausted chain is not by itself a proof
    ///
    /// A `Success` child with an empty successor used to be proved finished, and
    /// the window that made that wrong is the stretch between pass 0's
    /// `RunEnded { Success }` commit and pass 1's `seed_if_absent` commit. In
    /// that window the successor exists as a commissioned, running pass and
    /// holds no state, so a pass looking into it proved a working child
    /// finished. The stretch includes `build_run_setup` and successor-pass
    /// seeding — seconds, not a function call.
    ///
    /// It was once written down here as an *assumption*, on the ground that
    /// seconds against [`DEFAULT_PARK_GRACE`] is not reachable in practice. That
    /// is a probability argument for a write that cannot be taken back, and it
    /// is the wrong shape: unlike the wake-ledger window, re-reading does not
    /// help, because the fact that distinguishes the two cases has not been
    /// written yet by anybody.
    ///
    /// So the writer writes it. `executor.rs`'s
    /// `execute_agentically_with_refinement` publishes a
    /// [`super::store::ChainClosure`] for the segment it finished on, once it
    /// has decided no further pass will be commissioned, and this function reads
    /// it as the **last** thing before answering `Finished`. Three properties
    /// carry the safety:
    ///
    /// - **Presence is the evidence, absence is not.** No receipt means
    ///   `ChainNotClosed`, a suspicion, whatever the reason — a crash before
    ///   publication, a store with no receipts, or a chain genuinely still open.
    ///   A receipt can only ever turn a refusal into a proof.
    /// - **It is published strictly after the terminal it vouches for.** So a
    ///   pass that read the terminal and no receipt is exactly the pass that
    ///   looked too early, and the next sweep proves what this one declined to.
    /// - **It names its own segment.** A receipt found under a key it does not
    ///   name is a copied or renamed directory, and is treated as absent.
    ///
    /// What the receipt replaces is **not** a race: the bare-key read it
    /// eventually supersedes answered `Finished` for the entire life of pass 1,
    /// which is minutes of certainty rather than seconds of exposure.
    async fn child_state(&self, chain: &[ExecutionKey], complete: bool) -> StoreResult<ChildState> {
        // The furthest segment that recorded an ending, and what it recorded.
        // The walk does not stop at the first ending: a chain whose pass 0 ended
        // and whose pass 1 also ended is judged by pass 1, and only a walk to
        // the end of the chain can know which that is.
        let mut last_ended: Option<(usize, TerminalKind)> = None;
        for (index, segment) in chain.iter().enumerate() {
            match self.segment_state(segment).await? {
                // A gap is not the end of the chain. A segment that never
                // committed does not stop the walk, because the question is
                // where the child's work is NOW and a later segment answers it.
                SegmentState::NeverCommitted => continue,
                SegmentState::Live => return Ok(ChildState::Live),
                SegmentState::MayStillBeContinued { terminal } => {
                    return Ok(ChildState::MayStillBeContinued { terminal })
                },
                SegmentState::Ended { terminal } => last_ended = Some((index, terminal)),
            }
        }

        let Some((at, terminal)) = last_ended else {
            // Nothing committed at any address the park named. The child was
            // spawned and has not reached its first commit, or it never started
            // — and `ChildNeverCommitted`'s own docs say why the two are not
            // worth separating from here.
            return Ok(ChildState::NeverCommitted);
        };

        // NOTHING COULD FOLLOW THIS TERMINAL, so where the park's chain ends
        // does not matter. `refinement_gaps_if_warranted`'s last guard is
        // `let AgenticOutcome::Success { .. } = outcome else { return None }`,
        // so a child whose last committed terminal is `Failed`, `LoopDetected`
        // or `CannotProceed` has nothing that can be commissioned after it.
        // This arm is what keeps a park committed before the address existed
        // provable for the children that failed.
        if !matches!(terminal, TerminalKind::Success) {
            return Ok(ChildState::Finished);
        }
        // A `Success` child could have a refinement pass after it, so the answer
        // turns on whether this pass was able to LOOK. A stated chain names
        // every address such a pass could run under and the walk above read all
        // of them; finding nothing there is evidence. An unstated chain names
        // none, so there was nothing to read and there is no evidence — one
        // entry long and looking exhausted is exactly the shape the old false
        // proof had.
        if !complete {
            return Ok(ChildState::AddressNotCarried { at });
        }
        // AND ON WHETHER THE SEGMENT SAID IT WAS THE LAST ONE. Reading every
        // named address and finding the successors empty is *not* the same
        // claim as "nothing followed", and the gap between them is the seconds
        // this function's own assumption section used to concede: a successor
        // that has been commissioned and has not reached `seed_if_absent` is
        // empty at exactly the address a walk looks at. Past that gap the walk
        // was proving a working child finished, and no amount of re-reading the
        // same empty key closes it — the evidence is not there to be read.
        //
        // So the closing writer publishes it. `executor.rs`'s
        // `execute_agentically_with_refinement` records a
        // [`super::store::ChainClosure`] for the segment it finished on once it
        // has decided no further pass will be commissioned, and this is the read
        // of it. It is taken LAST, after the whole walk, for the same reason the
        // wake ledger is re-read last in `diagnose`: the cheap wrong direction
        // is one extra read, and the expensive one is a permanent terminal.
        //
        // Absence is never evidence. A run that crashed before publishing, a
        // store that publishes no receipts, and a chain that is genuinely still
        // open are one answer here — `ChainNotClosed`, a suspicion — because a
        // reader that told them apart would still have to refuse all three.
        let closed_at = &chain[at];
        match self.store.load_chain_closure(closed_at).await? {
            Some(closure) if closure.closes(closed_at) => Ok(ChildState::Finished),
            Some(misfiled) => {
                // A receipt found under a key it does not name is a copied or
                // renamed execution directory, not a closure. Loud because it
                // means an address in this store is not the address it claims,
                // which is worth more than the one proof it costs.
                warn!(
                    execution = %closed_at,
                    receipt_names = %misfiled.segment,
                    "[LOOP-RECONCILER] a chain-closure receipt names a different segment than \
                     the one it was found under; treating it as absent"
                );
                Ok(ChildState::ChainNotClosed { at })
            },
            None => Ok(ChildState::ChainNotClosed { at }),
        }
    }

    /// What ONE committed segment's own records say.
    ///
    /// The body this function holds is what `child_state` used to be, unchanged
    /// in what it reads and what it concludes — the fix is the chain around it,
    /// not the record scan inside it. It is split out so the two questions stay
    /// apart: *what does this key say* is answered from a key's records, and
    /// *what does that mean about the child* is answered from the whole chain,
    /// and folding them back together is how a segment's ending becomes a
    /// child's ending again.
    ///
    /// # Why this scans the authoritative records rather than replaying them
    ///
    /// [`super::journal::replay`] answers a different question. It reconstructs
    /// the cursor, and in doing so it **clears** a resumable terminal the moment
    /// another record follows it, and **refuses** the log outright
    /// (`RecordAfterTerminal`) when a record follows a non-resumable one. Both
    /// behaviours are right for a driver deciding where to resume; both are wrong
    /// here. The question is not *where would this child resume* but *did this
    /// child record an ending it cannot come back from*, and a `[RunEnded,
    /// OwnerTransition]` batch — which the design's own settled-assumptions table
    /// records as a live-path defect nothing on the write side refuses — would
    /// make `replay` answer with an error about a child that had plainly
    /// finished.
    ///
    /// So it scans for the record instead. Records **above** the child's committed
    /// watermark are excluded: those are an orphaned attempt by a worker that
    /// appended and never committed, and a terminal among them was never vouched
    /// for by anything.
    async fn segment_state(&self, segment: &ExecutionKey) -> StoreResult<SegmentState> {
        let Some(committed) = self.store.load(segment).await? else {
            return Ok(SegmentState::NeverCommitted);
        };
        let watermark = committed.state.journal_seq;
        if watermark == 0 {
            // Committed with nothing journaled: a run that has started and has
            // not yet reached a boundary. Live.
            return Ok(SegmentState::Live);
        }
        for record in self.store.read_journal(segment, 0).await? {
            if record.seq > watermark {
                continue;
            }
            let JournalBody::PhaseCompleted {
                step: RecordedStep::RunEnded { terminal },
            } = &record.body
            else {
                continue;
            };
            // A **resumable** terminal is not the end of a child. A pause, a
            // confirmation, a wait: the run is alive and something outside will
            // answer it, and a parent waiting on it is waiting legitimately.
            // Reading `terminal.is_some()` here instead would report every
            // delegating parent whose child sits at a confirmation prompt.
            if terminal.is_resumable() {
                continue;
            }
            // And `is_resumable() == false` is not the end of one either, for
            // the two kinds `TerminalKind::from` collapses a pause-carrying
            // outcome onto. See [`may_still_be_continued`].
            if may_still_be_continued(*terminal) {
                return Ok(SegmentState::MayStillBeContinued {
                    terminal: *terminal,
                });
            }
            return Ok(SegmentState::Ended {
                terminal: *terminal,
            });
        }
        Ok(SegmentState::Live)
    }
}

/// Whether a non-resumable terminal may nonetheless be sitting at a prompt.
///
/// [`TerminalKind`] is a lossy projection of `AgenticOutcome`: two of its
/// variants — `MaxIterationsReached` and `BudgetExhausted` — carry an
/// `Option<AgenticPauseState>`, are **resumable** when that is `Some`, and are
/// mapped by `TerminalKind::from` onto one kind each that
/// [`TerminalKind::is_resumable`] answers `false` for. `phases/resolve.rs`
/// reaches the second of those through `executor::conclude_budget_exhausted`,
/// which builds the pause unconditionally and emits a *"Continue or cancel?"*
/// prompt beside it, so the collapse is live rather than hypothetical.
///
/// The journal records the kind and not the payload, so this cannot be narrowed
/// to *"and the pause record was absent"* from the store. Both spellings
/// therefore answer `true` and stop the proof.
///
/// **Exhaustive on purpose.** A new [`TerminalKind`] fails to compile here until
/// somebody says which side of the line it falls on, which is the only thing
/// that would have caught the collapse the first time.
fn may_still_be_continued(terminal: TerminalKind) -> bool {
    match terminal {
        TerminalKind::MaxIterationsReached | TerminalKind::BudgetExhausted => true,
        // Ends the run outright, with no pause record in any spelling.
        TerminalKind::Success
        | TerminalKind::Failed
        | TerminalKind::LoopDetected
        | TerminalKind::CannotProceed => false,
        // This segment is over, but the terminal explicitly says its execution
        // may be continuing at the checkpoint's successor address. A parent
        // diagnosing the child must therefore fail toward live, never prove the
        // whole child finished from this segment alone.
        TerminalKind::HandedOff => true,
        // Resumable, so `segment_state` answers `Live` before reaching here. Named
        // rather than folded into a `_` so the exhaustiveness above is real.
        TerminalKind::WaitingForUser
        | TerminalKind::WaitingForConfirmation
        | TerminalKind::PausedByUser
        | TerminalKind::WaitingForChildren
        | TerminalKind::Sleeping => false,
    }
}

/// Record one finding, and say so where an operator will see it.
///
/// Logged here rather than left to a caller, because a finding nobody surfaces
/// is the same as no detection at all.
///
/// A free function rather than a method, so that the only `&self` in this module
/// is the one that reaches the store — a helper that took the receiver would be
/// a place a later edit could reach the store from without the cost table above
/// accounting for it.
fn push_finding(
    report: &mut ReconcileReport,
    parked: &ParkedExecution,
    parked_for_ms: Option<i64>,
    verdict: ParkVerdict,
    reason: ParkReason,
    action: ParkAction,
) {
    let finding = StalledPark {
        key: parked.key.clone(),
        wait: parked.wait.clone(),
        verdict,
        reason,
        action,
        parked_for_ms,
        last_lease: parked.last_lease.clone(),
    };
    // WHERE THIS PARK'S OWN WORK WENT, on the row rather than left to be
    // recovered. A `Children` wake is a receipt for the pause-owned continuation,
    // not an instruction to resume this key. The park carries the successor
    // address; `None` is *not stated*, which is a park committed before the field
    // existed and NOT a claim that nothing resumed.
    let resumes_as = match &finding.wait {
        WaitReason::Children { resume, .. } => resume.run_resumes_as.as_deref(),
        WaitReason::Job { .. } => None,
    };
    // Two lines, not one with a conditional message. A retirement is the only
    // row that says a write happened, and it is the row an operator most needs
    // to be able to grep for on its own — a shared message with a field to
    // discriminate is how the write becomes invisible in a page of findings.
    match &finding.action {
        ParkAction::Retired { terminal } => warn!(
            execution = %finding.key,
            verdict = ?finding.verdict,
            reason = ?finding.reason,
            terminal = ?terminal,
            parked_for_ms = ?finding.parked_for_ms,
            resumes_as = ?resumes_as,
            "[LOOP-RECONCILER] a parked run was proved unwakeable and RETIRED: a non-resumable \
             terminal is now in its journal and its wait is cleared. This says the execution key \
             ended, NOT that the work failed"
        ),
        action => warn!(
            execution = %finding.key,
            verdict = ?finding.verdict,
            reason = ?finding.reason,
            action = ?action,
            parked_for_ms = ?finding.parked_for_ms,
            resumes_as = ?resumes_as,
            "[LOOP-RECONCILER] a parked run is not going to wake on its own"
        ),
    }
    report.findings.push(finding);
}

/// The terminal a ground is retired with, or `None` for a ground never acted on.
///
/// # One terminal for both grounds, and it is chosen for its RESUMABILITY
///
/// [`TerminalKind::CannotProceed`] is the only non-resumable kind whose
/// `AgenticOutcome` counterpart has no resumable twin, and resumability is the
/// property that governs here: a resumable terminal is cleared by the next
/// record and leaves the run claimable, and a non-resumable one that *should*
/// have been resumable makes the log unappendable for a run that was still
/// alive.
///
/// The near misses, so the next reader does not re-derive them:
///
/// - `BudgetExhausted` reads better for the deadline ground — a wall-clock
///   deadline **is** a budget — and it is refused because
///   `AgenticOutcome::BudgetExhausted` carries an `Option<AgenticPauseState>`
///   and is a **resumable pause** when that is `Some`, while
///   `TerminalKind::from` maps both spellings to the one non-resumable kind. The
///   journal's answer is the one that governs the log, so writing it would not
///   be *wrong*; it would be a record whose two vocabularies disagree, chosen
///   for legibility, on the one field where being misread is permanent.
///   `MaxIterationsReached` has the same split.
/// - `Failed` attributes the ending to the work. Nothing here read the work.
///
/// So the **cause** is carried by [`StalledPark::reason`] and the `warn!`, not by
/// the kind. A vocabulary with no word for either cause should not be made to
/// imply one.
///
/// # It no longer holds the list, and that is the point
///
/// The set of retirable grounds is [`RetirableGround`], and this function is a
/// projection of it rather than a second copy — `RetirableGround::of` decides
/// membership, this decides the kind. A ground added to one is therefore added
/// to both, which matters more now that a **policy** also names grounds: three
/// places holding the same list would be two places for it to drift.
fn terminal_for(reason: &ParkReason) -> Option<TerminalKind> {
    RetirableGround::of(reason).map(RetirableGround::terminal)
}

/// The state under the lease is not the state the diagnosis judged.
fn moved(detail: &str) -> ParkAction {
    ParkAction::NotRetired(RetirementRefused::StateMoved {
        detail: detail.to_string(),
    })
}

/// The run's own log is not one this module may append to.
fn unusable(detail: String) -> ParkAction {
    ParkAction::NotRetired(RetirementRefused::JournalUnusable { detail })
}

/// What ONE loop-state key's records say.
///
/// Deliberately not [`ChildState`]: a segment is an address and a child is a
/// chain of them, and the whole defect this module carried was the two being one
/// type. A segment that ended is not a child that ended — it is a child that
/// ended *there*, which is only the same thing when nothing can follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentState {
    /// Nothing is committed under this key.
    NeverCommitted,
    /// Running, paused, parked — anything that is not over for good.
    Live,
    /// Recorded a terminal it cannot come back from. **This segment**; see the
    /// type's own docs.
    Ended { terminal: TerminalKind },
    /// Recorded a terminal that is non-resumable **in the journal's vocabulary**
    /// and may still be a live continue-or-cancel prompt in the outcome's. See
    /// [`may_still_be_continued`].
    MayStillBeContinued { terminal: TerminalKind },
}

/// What a child's own records say about whether it can still report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildState {
    /// Nothing is committed at **any** address the park named for this child.
    NeverCommitted,
    /// Running, paused, parked — anything that is not over for good. One live
    /// segment anywhere in the chain is enough.
    Live,
    /// Recorded a terminal it cannot come back from, where nothing can follow
    /// it. See [`LoopReconciler::child_state`] for the two ways nothing can.
    Finished,
    /// The child ended `Success` and the park named nowhere to look for what
    /// may have followed.
    ///
    /// Neither `Live` nor `Finished`, and a fourth answer rather than either of
    /// them for the reason [`ChildState::MayStillBeContinued`] is a third:
    /// `Finished` manufactures the false proof this type exists to prevent, and
    /// `Live` silently drops a park worth looking at.
    ///
    /// **This is what a park committed before
    /// [`super::state::ResumeAddress`] existed resolves to**, for every child of
    /// it that succeeded. Such a park carries no chain, so the only address
    /// recoverable from it is the child's bare execution id — which is where a
    /// refinement pass's *predecessor* ends. The pass reads that terminal,
    /// cannot read what may follow it, and says so instead of proving.
    ///
    /// `at` indexes the chain the caller passed in. The addresses stay with the
    /// caller: this module does not know how to spell a segment key and must not
    /// learn, or `executor.rs`'s spelling gains a second copy that cannot see
    /// the first.
    AddressNotCarried { at: usize },
    /// The child ended `Success` at an address the park named, every later
    /// address was read and was empty, and **nothing published a receipt saying
    /// that segment was the last one**.
    ///
    /// The fourth member of the family `AddressNotCarried` and
    /// `MayStillBeContinued` belong to, and it is the one that covers the
    /// seconds a walk cannot see: a refinement pass that has been commissioned
    /// and has not yet reached its first commit is empty at exactly the address
    /// the walk reads. See [`LoopReconciler::child_state`] for the read, and
    /// [`super::store::ChainClosure`] for what publishes the receipt.
    ///
    /// `at` indexes the chain the caller passed in, on the same terms as
    /// [`ChildState::AddressNotCarried`]: the addresses stay with the caller.
    ChainNotClosed { at: usize },
    /// Recorded a terminal that is non-resumable **in the journal's vocabulary**
    /// and may still be a live continue-or-cancel prompt in the outcome's.
    ///
    /// Neither `Live` nor `Finished`, and it is a separate answer rather than
    /// either of them because the two would be wrong in opposite directions:
    /// `Finished` manufactures the false proof this whole distinction exists to
    /// prevent, and `Live` silently drops a park worth looking at. See
    /// [`may_still_be_continued`] and [`ParkReason::ChildMayStillBeContinued`].
    MayStillBeContinued { terminal: TerminalKind },
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::effects::{
        EffectId, EffectLedger, EffectLedgerEntry, EffectOutcome,
    };
    use crate::magician_v2::execution::agentic::run_loop::journal::{
        JournalAppend, JournalRecord, ProjectorCursor, TerminalKind,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
    use crate::magician_v2::execution::agentic::run_loop::state::{
        ChildSegments, LoopCursor, LoopState, WorkerId,
    };
    use crate::magician_v2::execution::agentic::run_loop::store::contract::{fresh_state, key};
    use crate::magician_v2::execution::agentic::run_loop::store::memory::MemoryLoopStateStore;
    use crate::magician_v2::execution::agentic::run_loop::store::{
        ChainClosure, CommittedLoopState, Lease, ParkedListing, Revision, StoreError,
    };

    const HOUR_MS: i64 = 60 * 60 * 1000;

    fn policy() -> ReconcilerPolicy {
        ReconcilerPolicy {
            scan_limit: 100,
            ..ReconcilerPolicy::default()
        }
    }

    /// The same policy, allowed to act on **both** retirable grounds.
    ///
    /// Deliberately the widest policy the type can express, because it is what
    /// every case below that is *not* about the filter wants: a refusal those
    /// tests assert has to be the one their own fixture earned, not the
    /// blanket one a narrower policy would hand every row.
    fn recovering_policy() -> ReconcilerPolicy {
        recovering_on(BTreeSet::from([
            RetirableGround::DeadlinePassedWhileParked,
            RetirableGround::EveryChildHasFinished,
        ]))
    }

    /// Allowed to act, and only on the sound ground.
    ///
    /// The shape `executor.rs` would name if the decision to re-arm that site
    /// were taken — see `a_policy_naming_one_ground_retires_it_and_refuses_the_other`.
    fn deadline_ground_only() -> ReconcilerPolicy {
        recovering_on(BTreeSet::from([RetirableGround::DeadlinePassedWhileParked]))
    }

    fn recovering_on(grounds: BTreeSet<RetirableGround>) -> ReconcilerPolicy {
        ReconcilerPolicy {
            recovery: Recovery::RetireProvedParks {
                worker: ReconcilerWorker::named("reconciler-under-test")
                    .expect("the fixture id is inside the reconciler namespace"),
                grounds,
            },
            ..policy()
        }
    }

    /// A key some worker other than the reconciler holds or is pinned to.
    fn another_worker() -> WorkerId {
        WorkerId::new("some-other-worker")
    }

    /// The whole log of one execution, whatever its watermark.
    async fn journal_of(store: &dyn LoopStateStore, key: &ExecutionKey) -> Vec<JournalRecord> {
        store.read_journal(key, 0).await.expect("a journal reads")
    }

    /// The committed state of one execution.
    async fn state_of(store: &dyn LoopStateStore, key: &ExecutionKey) -> LoopState {
        store
            .load(key)
            .await
            .expect("a load answers")
            .expect("something is committed")
            .state
    }

    /// What a driver would replay this run's own authoritative log to.
    ///
    /// The same two calls `verify_journal` makes, in the same order, because the
    /// property a retirement has to leave behind is *the committed cursor is
    /// still the cursor its own journal replays to* — a record written at any
    /// other address diverges the two and the next claim quarantines the run
    /// rather than refusing it.
    async fn replayed_by_a_driver(
        store: &dyn LoopStateStore,
        key: &ExecutionKey,
    ) -> super::super::journal::ReplayedCursor {
        let state = state_of(store, key).await;
        let journal =
            Journal::from_records(journal_of(store, key).await).expect("the log is well-formed");
        let replayed = replay(journal.authoritative(state.journal_seq)).expect("and replays");
        assert_eq!(
            (replayed.iteration, replayed.phase),
            (state.cursor.iteration, state.cursor.phase),
            "a state whose journal replays to a different cursor is quarantined on the next \
             claim, not refused — which is a run this module broke rather than retired"
        );
        replayed
    }

    /// The address `executor.rs`'s `wait_for_outcome` writes onto a park.
    ///
    /// Spelled here rather than imported, because `executor.rs` is not a
    /// dependency of this module in either direction and importing it to build a
    /// fixture would invert that. The shape is pinned against the real producer
    /// by `executor.rs`'s own
    /// `a_childs_address_chain_covers_every_pass_that_could_be_working`; what
    /// this owes is that the fixture is not *weaker* than production, which is
    /// the direction that would make every test below pass for the wrong reason.
    ///
    /// One segment per refinement pass the child could take, nearest-first, with
    /// the bare id first — which is where a child's pass 0 commits.
    fn addressed(children: &[&str]) -> ResumeAddress {
        ResumeAddress {
            run_resumes_as: Some("parent-r7".to_string()),
            child_segments: children
                .iter()
                .map(|child| ChildSegments {
                    child_execution_id: (*child).to_string(),
                    segments: vec![(*child).to_string(), format!("{child}-p1")],
                })
                .collect(),
        }
    }

    /// A parent parked on `children`, committed.
    ///
    /// **Journal-free, which a real park is not.** See
    /// [`park_on_after_the_boundary_that_parked_it`] for the shape
    /// `commit_boundary` actually leaves behind, and use that one for anything
    /// that asserts about the journal, the cursor or an append.
    ///
    /// **Addressed, which a real park also is.** The park carries the segment
    /// chain production writes, so the tests below exercise the walk rather than
    /// the fallback. [`park_on_without_an_address`] is the fixture for the old
    /// on-disk shape, and it is deliberately a different function: a default
    /// that quietly omitted the address would make every proof below a proof
    /// about the degraded path.
    async fn park_on(
        store: &MemoryLoopStateStore,
        parent: &ExecutionKey,
        children: &[&str],
    ) -> WaitReason {
        let wait = WaitReason::Children {
            child_execution_ids: children.iter().map(|id| (*id).to_string()).collect(),
            resume: addressed(children),
        };
        let mut state = fresh_state(parent);
        state.wait = Some(wait.clone());
        store
            .commit(parent, &state, Revision::INITIAL)
            .await
            .expect("the parent parks");
        wait
    }

    /// The same park in the shape it had before addresses existed.
    ///
    /// What is on disk for every park committed before this field, and therefore
    /// what `serde(default)` produces on load: no chain for any child, and no
    /// key for this run's own resume.
    async fn park_on_without_an_address(
        store: &MemoryLoopStateStore,
        parent: &ExecutionKey,
        children: &[&str],
    ) -> WaitReason {
        let wait = WaitReason::Children {
            child_execution_ids: children.iter().map(|id| (*id).to_string()).collect(),
            resume: ResumeAddress::unstated(),
        };
        let mut state = fresh_state(parent);
        state.wait = Some(wait.clone());
        store
            .commit(parent, &state, Revision::INITIAL)
            .await
            .expect("the parent parks");
        wait
    }

    /// The park a driver actually commits, journal and cursor included.
    ///
    /// # Every field here is what `commit_boundary` would have written
    ///
    /// - The batch ends with `RunEnded { WaitingForChildren }`, because
    ///   `driver_worker::commit_boundary` puts a run-ending completion record
    ///   **last** in its batch.
    /// - `state.cursor` is that record's own `(iteration, phase)`, because
    ///   `next_cursor` answers `current` for a `RunEnded` step and
    ///   `replay_each` sets the cursor to the record's own address.
    /// - `state.journal_seq` is that record's seq, so the record is
    ///   authoritative rather than an orphan.
    ///
    /// [`park_on`] has none of that: an empty journal, watermark zero, and the
    /// cursor `LoopState::new` starts at. Two things are only reachable on the
    /// shape below, and neither was exercised anywhere before this fixture
    /// existed:
    ///
    /// 1. **`replay_each`'s "a resumable ending is not the end of the log"
    ///    branch**, which is the only reason a retirement's append is legal at
    ///    all. On a journal-free park `replayed.terminal` is `None` and the
    ///    branch is never entered.
    /// 2. **The duplicate journal address** the append then produces — a second
    ///    record at the same `(iteration, phase, ordinal)` — legal only because
    ///    `Journal::check_batch_addresses` is batch-scoped and
    ///    `ProjectorCursor::project` skips non-`Event` bodies.
    async fn park_on_after_the_boundary_that_parked_it(
        store: &dyn LoopStateStore,
        parent: &ExecutionKey,
        children: &[&str],
    ) -> WaitReason {
        let wait = WaitReason::Children {
            child_execution_ids: children.iter().map(|id| (*id).to_string()).collect(),
            resume: addressed(children),
        };
        let seq = store
            .append_journal(
                parent,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::WaitingForChildren,
                    },
                )],
            )
            .await
            .expect("the parking boundary records its ending");
        let mut state = fresh_state(parent);
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Apply,
        };
        state.journal_seq = seq;
        state.wait = Some(wait.clone());
        store
            .commit(parent, &state, Revision::INITIAL)
            .await
            .expect("the parent parks");
        wait
    }

    /// A child that has committed a journaled ending **and closed its chain**.
    ///
    /// The closure is published only for `Success`, which is exactly what
    /// production publishes: `executor.rs`'s
    /// `execute_agentically_with_refinement` records one for the segment it
    /// finished on when the run ends successfully with no further pass
    /// commissioned, and a non-`Success` ending needs no receipt because
    /// `refinement_gaps_if_warranted` can commission nothing after one.
    ///
    /// [`child_ending_as_with_an_open_chain`] is the deliberately different
    /// fixture for a segment that ended and has **not** said it was the last —
    /// the seconds before its own writer publishes, and the shape of every
    /// child whose process died in between. A default that quietly published a
    /// receipt for every ending would make the receipt read as free, which is
    /// the whole thing it is not.
    async fn child_ending_as(
        store: &MemoryLoopStateStore,
        child: &ExecutionKey,
        terminal: TerminalKind,
    ) {
        child_ending_as_with_an_open_chain(store, child, terminal).await;
        if matches!(terminal, TerminalKind::Success) {
            close_the_chain_at(store, child).await;
        }
    }

    /// A child that has committed a journaled ending and published no receipt.
    async fn child_ending_as_with_an_open_chain(
        store: &MemoryLoopStateStore,
        child: &ExecutionKey,
        terminal: TerminalKind,
    ) {
        let seq = store
            .append_journal(
                child,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded { terminal },
                )],
            )
            .await
            .expect("the child records its ending");
        let mut state = fresh_state(child);
        state.journal_seq = seq;
        store
            .commit(child, &state, Revision::INITIAL)
            .await
            .expect("and commits it");
    }

    /// The receipt `execute_agentically_with_refinement` publishes once it has
    /// decided nothing follows the segment it just finished.
    async fn close_the_chain_at(store: &MemoryLoopStateStore, segment: &ExecutionKey) {
        store
            .record_chain_closure(
                segment,
                &ChainClosure::for_segment(segment.execution_id(), Utc::now().timestamp_millis()),
            )
            .await
            .expect("the closing writer publishes its receipt");
    }

    /// A child that has committed and journaled nothing.
    ///
    /// Watermark zero, so judging it stops at `segment_state`'s `watermark == 0`
    /// early exit and never reaches the journal scan. See
    /// [`child_running_past_a_boundary`] for the other live shape — the two
    /// reach different code and neither covers the other.
    async fn child_still_running(store: &MemoryLoopStateStore, child: &ExecutionKey) {
        store
            .commit(child, &fresh_state(child), Revision::INITIAL)
            .await
            .expect("the child is under way");
    }

    /// A child that has committed a journaled, NON-terminal boundary.
    ///
    /// Watermark one, so judging it runs the whole journal scan and falls out of
    /// the bottom of it — the `Ok(SegmentState::Live)` fall-through, which
    /// [`child_still_running`] cannot reach.
    async fn child_running_past_a_boundary(store: &MemoryLoopStateStore, child: &ExecutionKey) {
        let seq = store
            .append_journal(
                child,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the child completes a phase");
        let mut state = fresh_state(child);
        state.journal_seq = seq;
        store
            .commit(child, &state, Revision::INITIAL)
            .await
            .expect("and commits it");
    }

    async fn one_finding(store: &dyn LoopStateStore, now_ms: i64) -> StalledPark {
        one_finding_with(store, policy(), now_ms).await
    }

    async fn one_finding_with(
        store: &dyn LoopStateStore,
        policy: ReconcilerPolicy,
        now_ms: i64,
    ) -> StalledPark {
        let report = LoopReconciler::new(store, policy)
            .reconcile_at(now_ms)
            .await
            .expect("a pass");
        assert_eq!(
            report.findings.len(),
            1,
            "expected exactly one finding, got {:?}",
            report.findings
        );
        report.findings.into_iter().next().expect("one finding")
    }

    async fn children_handoff_contract(store: &dyn LoopStateStore) {
        let parent = key("handoff-parent");
        park_on_after_the_boundary_that_parked_it(store, &parent, &["child-b", "child-a"]).await;
        let children = vec!["child-a".to_string(), "child-b".to_string()];
        let worker = ReconcilerWorker::named("reconciler-handoff-test").expect("valid worker");

        let first = prepare_children_handoff(store, &parent, &children, "parent-r7", &worker)
            .await
            .expect("a ready checkpoint prepares its source handoff");
        assert!(first.newly_retired);
        assert_eq!(first.wake_token, "children:child-a,child-b");
        assert!(
            first.resolution_id.starts_with("delegation-ready:"),
            "the readiness id is a content-bound handoff identity"
        );

        let state = state_of(store, &parent).await;
        assert!(state.wait.is_none(), "the old cursor is no longer parked");
        assert_eq!(
            replayed_by_a_driver(store, &parent).await.terminal,
            Some(TerminalKind::HandedOff),
            "the old cursor is non-resumable before its wake receipt is published"
        );
        assert_eq!(
            store
                .wake_resolutions(&parent, &first.wake_token)
                .await
                .expect("wake ledger reads"),
            vec![first.resolution_id.clone()],
            "readiness remains durable across the checkpoint dispatch boundary"
        );
        assert!(
            !store
                .list_runnable(&another_worker(), 10)
                .await
                .expect("discovery reads")
                .contains(&parent),
            "a resolved wake must not make the handed-off mid-Apply segment runnable"
        );

        let mismatched_retry =
            prepare_children_handoff(store, &parent, &children, "another-successor", &worker)
                .await
                .expect_err("a retry after wait clearing remains bound to the original checkpoint");
        assert!(matches!(
            mismatched_retry,
            ChildrenHandoffError::StateMismatch { .. }
        ));

        let retried = prepare_children_handoff(store, &parent, &children, "parent-r7", &worker)
            .await
            .expect("a crash retry restores the same receipt");
        assert!(!retried.newly_retired);
        assert_eq!(retried.wake_token, first.wake_token);
        assert_eq!(retried.resolution_id, first.resolution_id);
        assert_eq!(
            journal_of(store, &parent).await.len(),
            2,
            "retry must not append a second handoff terminal"
        );

        assert_eq!(
            consume_prepared_children_handoff(store, &retried)
                .await
                .expect("dispatch consumes readiness"),
            1
        );
        assert_eq!(
            consume_prepared_children_handoff(store, &retried)
                .await
                .expect("consumption is idempotent"),
            0
        );
    }

    #[tokio::test]
    async fn memory_store_delegation_handoff_is_crash_retryable_and_never_reenters_the_source() {
        children_handoff_contract(&MemoryLoopStateStore::new()).await;
    }

    #[tokio::test]
    async fn filesystem_store_delegation_handoff_is_crash_retryable_and_never_reenters_the_source()
    {
        let root = tempfile::tempdir().expect("temporary store root");
        let store = super::super::store::fs::FsLoopStateStore::new(root.path());
        children_handoff_contract(&store).await;
    }

    #[tokio::test]
    async fn delegation_handoff_refuses_a_checkpoint_that_names_another_successor() {
        let store = MemoryLoopStateStore::new();
        let parent = key("handoff-mismatch");
        park_on_after_the_boundary_that_parked_it(&store, &parent, &["child-a"]).await;
        let worker = ReconcilerWorker::named("reconciler-handoff-test").expect("valid worker");

        let error = prepare_children_handoff(
            &store,
            &parent,
            &["child-a".to_string()],
            "another-resume-segment",
            &worker,
        )
        .await
        .expect_err("a checkpoint must not retire another continuation's source");
        assert!(matches!(error, ChildrenHandoffError::StateMismatch { .. }));
        assert!(state_of(&store, &parent).await.wait.is_some());
        assert_eq!(journal_of(&store, &parent).await.len(), 1);
        assert!(
            store
                .wake_resolutions(&parent, "children:child-a")
                .await
                .expect("wake ledger reads")
                .is_empty(),
            "a refusal writes neither terminal nor readiness receipt"
        );
    }

    /// The row the design's failure-mode table names, end to end.
    ///
    /// What production change leaves this green: any that stops distinguishing
    /// parks at all and reports everything — which is what
    /// `a_park_with_one_live_child_is_left_alone` exists to fail.
    #[tokio::test]
    async fn a_park_whose_children_have_all_finished_is_proved_unwakeable() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1", "child-2"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        child_ending_as(&store, &key("child-2"), TerminalKind::Failed).await;

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.key, parent);
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.reason,
            ParkReason::EveryChildHasFinished { children: 2 }
        );
        assert!(
            finding.parked_for_ms.is_some_and(|age| age >= HOUR_MS),
            "the finding must carry how long the park sat: {:?}",
            finding.parked_for_ms
        );
    }

    /// **The defect this module carried, as a test.**
    ///
    /// A delegated child's pass 0 commits `RunEnded { Success }` under its bare
    /// execution id and its refinement pass 1 runs under `{child}-p1`. Before the
    /// park carried an address, `child_state` read the bare key, answered
    /// `Finished`, and `EveryChildHasFinished` became a proof about a child that
    /// was working — retirable, past a grace a refinement pass routinely
    /// outlives.
    ///
    /// What production change turns this red: reading only the first segment of
    /// the chain, or building the chain from the ids instead of from the address
    /// the park carries. Both revert to the false proof.
    #[tokio::test]
    async fn a_child_whose_refinement_pass_is_working_is_not_a_finished_child() {
        for pass_one in ["still_running", "past_a_boundary"] {
            let store = MemoryLoopStateStore::new();
            let parent = key("parent");
            park_on(&store, &parent, &["child-1"]).await;
            // Pass 0 SUCCEEDED, at the address a bare-id read stops at — and
            // published no receipt, because the writer that would publish one
            // is still inside pass 1.
            child_ending_as_with_an_open_chain(&store, &key("child-1"), TerminalKind::Success)
                .await;
            // And pass 1 is doing real work one address further along. Both live
            // shapes, because `segment_state` answers them from different
            // branches.
            if pass_one == "still_running" {
                child_still_running(&store, &key("child-1-p1")).await;
            } else {
                child_running_past_a_boundary(&store, &key("child-1-p1")).await;
            }

            let report = LoopReconciler::new(&store, recovering_policy())
                .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
                .await
                .expect("a pass");
            assert!(
                report.findings.is_empty(),
                "the park is healthy — its child is working under `child-1-p1` ({pass_one}); got \
                 {:?}",
                report.findings
            );
            assert_eq!(
                report.retired, 0,
                "and nothing may be written into a run whose child is live ({pass_one})"
            );
            assert!(
                state_of(&store, &parent).await.wait.is_some(),
                "the park itself must survive the pass ({pass_one})"
            );
        }
    }

    /// The chain is judged by its LAST committed segment, not its first.
    ///
    /// Pass 0 succeeded and pass 1 then failed. The child is finished — but
    /// finished *at `child-1-p1`*, and a walk that stopped at the first ending it
    /// found would reach the same verdict for the wrong reason and would keep
    /// reaching it when pass 1 was live instead.
    #[tokio::test]
    async fn a_child_is_judged_by_the_last_segment_that_committed() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        // No receipt on pass 0: a segment a refinement pass followed is not one
        // its writer ever closed the chain at.
        child_ending_as_with_an_open_chain(&store, &key("child-1"), TerminalKind::Success).await;
        child_ending_as(&store, &key("child-1-p1"), TerminalKind::Failed).await;

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.reason,
            ParkReason::EveryChildHasFinished { children: 1 },
            "every address the park named was read and the furthest one that committed ended \
             where nothing can follow it"
        );
    }

    /// **The window the receipt closes, as a test.**
    ///
    /// Pass 0 committed `RunEnded { Success }` and nothing is at `child-1-p1`.
    /// From the store alone that is indistinguishable from a chain that ended —
    /// and it is exactly what a commissioned refinement pass looks like for the
    /// seconds between its predecessor's terminal commit and its own first
    /// commit. So the exhausted chain is not the proof; the receipt is, and
    /// there is none.
    ///
    /// Asserted under the widest recovering policy there is, because the claim
    /// is that no policy retires this row rather than that the default does not.
    ///
    /// What production change turns this red: answering `Finished` from an
    /// exhausted chain alone, which is what this ground did before the receipt
    /// existed and is why it was held at diagnosis-only.
    #[tokio::test]
    async fn a_child_that_succeeded_and_published_no_receipt_is_never_proved() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as_with_an_open_chain(&store, &key("child-1"), TerminalKind::Success).await;

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "an unclosed chain is a suspicion, and it used to be a proof"
        );
        assert_eq!(
            finding.reason,
            ParkReason::ChildChainNotClosed {
                child_execution_id: "child-1".to_string(),
                read_at: "child-1".to_string(),
            },
            "and the row names the segment whose receipt was missing"
        );
        assert_eq!(
            finding.action,
            ParkAction::Reported,
            "a `Stalled` row never reaches a ground at all"
        );
        assert!(
            journal_of(&store, &parent).await.is_empty(),
            "nothing may be written into a park this pass did not prove"
        );
        assert!(
            state_of(&store, &parent).await.wait.is_some(),
            "and the park itself must survive the pass"
        );
    }

    /// The other half of that contract: the receipt is what makes the proof.
    ///
    /// One store, one park, two passes, and the only thing that changes between
    /// them is that the child's own writer published its closure. A change that
    /// made the receipt decorative — read but not required, or required but
    /// never consulted — leaves one of the two halves red.
    ///
    /// It also pins the direction the receipt may move a verdict in: from
    /// suspicion to proof and never the reverse. Nothing retracts a closure, so
    /// the second pass is the only order these two answers can occur in.
    #[tokio::test]
    async fn publishing_the_receipt_turns_the_same_suspicion_into_a_proof() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as_with_an_open_chain(&store, &key("child-1"), TerminalKind::Success).await;
        let now_ms = Utc::now().timestamp_millis() + HOUR_MS;

        let before = one_finding_with(&store, recovering_policy(), now_ms).await;
        assert_eq!(before.verdict, ParkVerdict::Stalled);
        assert_eq!(
            before.action,
            ParkAction::Reported,
            "the first pass must leave the park exactly as it found it"
        );

        close_the_chain_at(&store, &key("child-1")).await;

        let after = one_finding_with(&store, recovering_policy(), now_ms).await;
        assert_eq!(after.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            after.reason,
            ParkReason::EveryChildHasFinished { children: 1 }
        );
        assert_eq!(
            after.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed
            }
        );
        assert!(
            state_of(&store, &parent).await.wait.is_none(),
            "a retired park must leave the listing"
        );
    }

    /// A receipt found under a key it does not name is not a receipt.
    ///
    /// The shape is a copied or renamed execution directory, and the cost of
    /// believing one is a permanent terminal on a run nobody asked about — so
    /// `ChainClosure::closes` is checked at the read and a mismatch is treated
    /// as absence.
    ///
    /// What production change turns this red: reading only `is_some()` on the
    /// loaded receipt, which is the shape the read would naturally have been
    /// written in.
    #[tokio::test]
    async fn a_receipt_naming_another_segment_does_not_close_this_one() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as_with_an_open_chain(&store, &key("child-1"), TerminalKind::Success).await;
        store
            .record_chain_closure(
                &key("child-1"),
                &ChainClosure::for_segment("child-9", 1_700_000_000_000),
            )
            .await
            .expect("the misfiled receipt is written");

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Stalled);
        assert_eq!(
            finding.reason,
            ParkReason::ChildChainNotClosed {
                child_execution_id: "child-1".to_string(),
                read_at: "child-1".to_string(),
            },
            "a receipt for another segment must count as no receipt at all"
        );
    }

    /// The receipt that matters is the one on the segment the child ENDED at.
    ///
    /// Pass 0 succeeded and closed; pass 1 then ran, succeeded, and has not
    /// published. The child is judged by pass 1 — the last segment that
    /// committed — so pass 0's receipt is about a segment the child has already
    /// left and proves nothing about where it is now.
    ///
    /// What production change turns this red: reading the receipt at the head of
    /// the chain instead of at the segment the walk judged, which is the shorter
    /// and more obvious way to write the read.
    #[tokio::test]
    async fn the_receipt_that_matters_is_the_one_on_the_segment_the_child_ended_at() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        child_ending_as_with_an_open_chain(&store, &key("child-1-p1"), TerminalKind::Success).await;

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Stalled);
        assert_eq!(
            finding.reason,
            ParkReason::ChildChainNotClosed {
                child_execution_id: "child-1".to_string(),
                read_at: "child-1-p1".to_string(),
            },
            "the head of the chain closed; the segment the child ended at did not"
        );
    }

    /// A child that could not have been refined needs no receipt.
    ///
    /// `refinement_gaps_if_warranted`'s last guard is
    /// `let AgenticOutcome::Success { .. } = outcome else { return None }`, so a
    /// child whose last committed terminal is `Failed` has nothing that could be
    /// commissioned after it at any address — the closure is a **fact about the
    /// terminal**, not a receipt anyone has to publish. Requiring one anyway
    /// would make the whole ground unprovable for every failed child, which is
    /// most of the population an operator cares about.
    ///
    /// The fixture publishes no receipt for a non-`Success` ending, deliberately
    /// — see [`child_ending_as`] — so this case is the one that would go red if
    /// the read stopped distinguishing the two.
    #[tokio::test]
    async fn a_child_that_could_not_be_refined_is_proved_without_a_receipt() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Failed).await;
        assert!(
            store
                .load_chain_closure(&key("child-1"))
                .await
                .expect("the store answers")
                .is_none(),
            "the fixture must not have published one, or this case proves nothing"
        );

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.reason,
            ParkReason::EveryChildHasFinished { children: 1 }
        );
    }

    /// A park committed before the address existed is reported, never proved.
    ///
    /// The degradation contract for the on-disk backlog. `serde(default)` gives
    /// such a park an **unstated** chain, and unstated is *"this pass had
    /// nowhere to look"* rather than *"there is nowhere else to look"* — so a
    /// `Success` child cannot be proved finished however long the park has sat.
    ///
    /// Asserted under the widest recovering policy there is, because the claim
    /// is that no policy can retire it rather than that the default does not.
    #[tokio::test]
    async fn an_old_park_with_no_address_is_never_proved_about_a_child_that_succeeded() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on_without_an_address(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "an unaddressed park is a suspicion, and it used to be a proof"
        );
        assert_eq!(
            finding.reason,
            ParkReason::ChildAddressNotCarried {
                child_execution_id: "child-1".to_string(),
                read_at: "child-1".to_string(),
            },
            "and the row says which address was read, because it is the only one there was"
        );
        assert_eq!(
            finding.action,
            ParkAction::Reported,
            "a `Stalled` row is never acted on — `act_on` returns before it reaches a ground at \
             all — so this is `Reported` rather than a named refusal"
        );
        assert!(
            journal_of(&store, &parent).await.is_empty(),
            "and nothing was written into the parent"
        );
    }

    /// The other half of that contract: an old park is not made useless.
    ///
    /// A child that ended `Failed` still proves the park, address or no address,
    /// because `refinement_gaps_if_warranted` commissions a pass from `Success`
    /// and from nothing else — so there is no further segment for an unstated
    /// chain to be hiding.
    #[tokio::test]
    async fn an_old_park_is_still_proved_about_a_child_that_could_not_refine() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on_without_an_address(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Failed).await;

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.reason,
            ParkReason::EveryChildHasFinished { children: 1 }
        );
    }

    /// A segment the park names but the store has nothing at is not the end of
    /// the chain.
    ///
    /// `child-1` never committed and `child-1-p1` ended: a walk that stopped at
    /// the first absent segment would answer `ChildNeverCommitted` about a child
    /// that demonstrably finished. Not a shape production writes — it is here
    /// because the walk's "a gap is not the end" rule is one `continue` and
    /// nothing else would fail if it became a `break`.
    #[tokio::test]
    async fn a_gap_in_the_chain_does_not_stop_the_walk() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1-p1"), TerminalKind::Failed).await;

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.reason,
            ParkReason::EveryChildHasFinished { children: 1 }
        );
    }

    /// The negative that stops the case above from passing on "report everything".
    ///
    /// One child of two has finished; the other is live. Run over **both** shapes
    /// a live child comes in, because `segment_state` answers them from different
    /// branches — nothing journaled stops at the `watermark == 0` early exit, and
    /// a journaled non-terminal boundary runs the scan and falls out of the
    /// bottom.
    ///
    /// What production change leaves this green: none of the three it is named
    /// for. Judging the park on the first child it looked at turns it red;
    /// reading "committed nothing" as finished turns the first case red; reading
    /// "scanned the journal and found no terminal" as finished turns the second
    /// case red. Before the second case existed that last flip left this test
    /// green — the property was covered, but by
    /// `a_terminal_a_dead_worker_left_above_the_watermark_does_not_end_a_child`,
    /// which is not what this test's name claims and would have taken the
    /// coverage with it if it were ever deleted.
    #[tokio::test]
    async fn a_park_with_one_live_child_is_left_alone() {
        for journaled in [false, true] {
            let store = MemoryLoopStateStore::new();
            park_on(&store, &key("parent"), &["child-1", "child-2"]).await;
            child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
            if journaled {
                child_running_past_a_boundary(&store, &key("child-2")).await;
            } else {
                child_still_running(&store, &key("child-2")).await;
            }

            let report = LoopReconciler::new(&store, policy())
                .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
                .await
                .expect("a pass");
            assert_eq!(
                report.examined, 1,
                "journaled={journaled}: the park was past its grace"
            );
            assert!(
                report.findings.is_empty(),
                "journaled={journaled}: a park with a live child is a park that is working: {:?}",
                report.findings
            );
        }
    }

    /// A child that cannot be read does not outrank a child that is live.
    ///
    /// The unreadable child is the one the scan reaches **first**, which is the
    /// case an early return got wrong: the park is working — `child-2` can still
    /// report — and reporting it is a finding about a healthy park.
    ///
    /// What production change leaves this green: not the early return this
    /// replaced, which reported on `child-1` before ever reaching `child-2`. A
    /// change that simply dropped unreadable children instead of holding them
    /// does leave it green, and
    /// `an_unreadable_child_still_stops_the_proof_when_no_child_is_live` is what
    /// turns that one red.
    #[tokio::test]
    async fn an_unreadable_child_does_not_outrank_a_live_child() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1", "child-2"]).await;
        child_still_running(&store, &key("child-2")).await;
        let probe = ProbeStore::over(store).with_an_unreadable_child(key("child-1"));

        let report = LoopReconciler::new(&probe, policy())
            .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(report.examined, 1, "the park was past its grace");
        assert!(
            report.findings.is_empty(),
            "one live child is the whole answer, whatever could not be read beside it: {:?}",
            report.findings
        );
    }

    /// An unreadable child still stops the proof when nothing else is live.
    ///
    /// `child-1` cannot be read and `child-2` has ended for good. Holding the
    /// fault must not let the park fall through to `EveryChildHasFinished`: an
    /// unreadable journal must never be counted as an ending, which is the
    /// proof-manufacturing case the early return was there to prevent and which
    /// holding the fault has to keep preventing.
    ///
    /// What production change leaves this green: none. Dropping the held fault
    /// turns it red with `Unwakeable`; counting an unreadable child as finished
    /// does the same.
    #[tokio::test]
    async fn an_unreadable_child_still_stops_the_proof_when_no_child_is_live() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1", "child-2"]).await;
        child_ending_as(&store, &key("child-2"), TerminalKind::Success).await;
        let probe = ProbeStore::over(store).with_an_unreadable_child(key("child-1"));

        let finding = one_finding(&probe, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "a child that could not be read is not a child that finished"
        );
        assert!(
            matches!(
                &finding.reason,
                ParkReason::ChildUnreadable {
                    child_execution_id,
                    ..
                } if child_execution_id == "child-1"
            ),
            "got {:?}",
            finding.reason
        );
    }

    /// A child waiting for a person is a live child, not a finished one.
    ///
    /// This is the assertion that fails if `segment_state` ever reads
    /// `terminal.is_some()` instead of `!terminal.is_resumable()` — which would
    /// report every delegating parent whose child sits at a prompt.
    #[tokio::test]
    async fn a_child_paused_for_a_person_is_still_a_live_child() {
        for terminal in [
            TerminalKind::WaitingForUser,
            TerminalKind::WaitingForConfirmation,
            TerminalKind::PausedByUser,
            TerminalKind::WaitingForChildren,
            TerminalKind::Sleeping,
        ] {
            let store = MemoryLoopStateStore::new();
            park_on(&store, &key("parent"), &["child-1"]).await;
            child_ending_as(&store, &key("child-1"), terminal).await;

            let report = LoopReconciler::new(&store, policy())
                .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
                .await
                .expect("a pass");
            assert_eq!(report.examined, 1, "{terminal:?}: the park was diagnosed");
            assert!(
                report.findings.is_empty(),
                "{terminal:?} is resumable, so the child can still report: {:?}",
                report.findings
            );
        }
    }

    /// The grace period, both ways, in one test.
    ///
    /// The first half fails if the grace is dropped; the second fails if nothing
    /// is ever examined. Either alone would pass on a broken implementation.
    #[tokio::test]
    async fn a_park_is_not_examined_until_it_is_past_its_grace() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        let reconciler = LoopReconciler::new(&store, policy());
        let now_ms = Utc::now().timestamp_millis();

        let fresh = reconciler
            .reconcile_at(now_ms + 60_000)
            .await
            .expect("a pass");
        assert_eq!(fresh.parked, 1, "the park is seen");
        assert_eq!(fresh.examined, 0, "and not yet looked at");
        assert!(fresh.findings.is_empty());

        let aged = reconciler
            .reconcile_at(now_ms + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(aged.examined, 1);
        assert_eq!(aged.findings.len(), 1, "the same park, an hour later");
    }

    /// A job park is a stall and never a proof.
    ///
    /// A production change that let a `Job` park reach `Unwakeable` — by falling
    /// through to the children arm, or by concluding from an empty ledger alone —
    /// turns this red.
    #[tokio::test]
    async fn a_job_park_is_surfaced_as_a_stall_and_never_as_a_proof() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("the parent parks on a job");

        let now_ms = Utc::now().timestamp_millis();
        let reconciler = LoopReconciler::new(&store, policy());

        // An hour in, which is past the CHILDREN grace and nowhere near the job
        // one. A single shared grace would report this — and would report every
        // legitimate eight-hour coding run for seven and three quarter hours.
        let early = reconciler
            .reconcile_at(now_ms + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(early.parked, 1, "the park is seen");
        assert_eq!(
            early.examined, 0,
            "a job park is designed to be long, and fifteen minutes is not evidence"
        );

        let finding = one_finding(&store, now_ms + 13 * HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Stalled);
        assert_eq!(
            finding.reason,
            ParkReason::JobHasNotReported {
                job_id: "coding-1".to_string()
            }
        );
    }

    /// A park past its own deadline is reported without waiting out any grace.
    ///
    /// The run is parked on a job, whose grace is twelve hours, and this pass
    /// runs one second after the deadline. A production change that folded the
    /// deadline check in behind the age gate would leave the one provably dead
    /// run unreported for the longest of any — which is the exact inversion this
    /// ordering exists to prevent.
    #[tokio::test]
    async fn a_park_past_its_own_deadline_is_reported_without_waiting_out_its_grace() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let now_ms = Utc::now().timestamp_millis();
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        state.deadline_at_ms = Some(now_ms + 1_000);
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let inside = LoopReconciler::new(&store, policy())
            .reconcile_at(now_ms)
            .await
            .expect("a pass");
        assert!(
            inside.findings.is_empty(),
            "a deadline that has not passed is not a passed deadline: {:?}",
            inside.findings
        );

        let finding = one_finding(&store, now_ms + 1_001).await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Unwakeable,
            "a deadline-passed run cannot advance even if its wake resolves"
        );
        assert_eq!(
            finding.reason,
            ParkReason::DeadlinePassedWhileParked {
                deadline_at_ms: now_ms + 1_000
            }
        );
    }

    /// A child that never committed is evidence, not proof.
    #[tokio::test]
    async fn a_child_that_never_committed_does_not_prove_the_parent_is_unwakeable() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1", "child-2"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        // child-2 was named and never wrote anything.

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "an absent child and a finished child are not the same evidence"
        );
        assert_eq!(
            finding.reason,
            ParkReason::ChildNeverCommitted {
                child_execution_ids: vec!["child-2".to_string()]
            }
        );
    }

    /// A satisfied park that nobody took is surfaced as its own thing.
    #[tokio::test]
    async fn a_wake_nobody_acted_on_is_reported_rather_than_read_as_a_healthy_park() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let wait = park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        store
            .resolve_wake(&parent, &wait.wake_token(), "child-1-done")
            .await
            .expect("the completer reports");

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "a resolved park is not a park with nothing to wake it"
        );
        assert_eq!(
            finding.reason,
            ParkReason::WakeResolvedButNotResumed { resolutions: 1 }
        );
    }

    /// A completion that lands **during** the diagnosis must not be raced.
    ///
    /// The probe answers the ledger as empty once and publishes a real resolution
    /// afterwards, which is exactly the window between the cheap first read and
    /// the proof — a vocabulary the plain memory store cannot express. Reverting
    /// `diagnose` to a single ledger read turns this red with `Unwakeable`, which
    /// is the wrong answer about a run that is about to wake on its own.
    #[tokio::test]
    async fn a_completion_landing_during_the_diagnosis_downgrades_the_proof() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        let probe = ProbeStore::over(store).resolving_during_the_diagnosis();

        let finding = one_finding(&probe, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Stalled,
            "a proof taken before the last read is a proof about a stale ledger"
        );
        assert_eq!(
            finding.reason,
            ParkReason::WakeResolvedButNotResumed { resolutions: 1 }
        );
    }

    /// A report-only pass never writes.
    ///
    /// The point the whole module started from: a detector that could itself
    /// strand a run is worse than no detector. The probe panics on every mutating
    /// method, so any production change that commits, appends, claims, resolves
    /// or consumes **on this policy** fails here rather than in an incident.
    ///
    /// Narrowed from *the reconciler writes nothing* when retirement landed, and
    /// the narrowing is stated rather than left to the name: writing is now
    /// reachable, and what this pins is that [`Recovery::ReportOnly`] does not
    /// reach it. The acting path is pinned by the retirement tests below, which
    /// use the same probe with the four retirement writes delegated and the wake
    /// ledger still fatal.
    #[tokio::test]
    async fn a_report_only_pass_writes_nothing() {
        let store = MemoryLoopStateStore::new();
        let unwakeable = key("all-finished");
        park_on(&store, &unwakeable, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let resolved = key("resolved");
        let wait = park_on(&store, &resolved, &["child-9"]).await;
        child_ending_as(&store, &key("child-9"), TerminalKind::Success).await;
        store
            .resolve_wake(&resolved, &wait.wake_token(), "child-9-done")
            .await
            .expect("the completer reports");

        let jobbed = key("jobbed");
        let mut state = fresh_state(&jobbed);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&jobbed, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let probe = ProbeStore::over(store);
        // Thirteen hours, so the job park is past its own grace too — the pass
        // has to reach every arm for the absence of writes to mean anything.
        let report = LoopReconciler::new(&probe, policy())
            .reconcile_at(Utc::now().timestamp_millis() + 13 * HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(
            report.findings.len(),
            3,
            "the pass has to reach every finding kind, or it proves nothing about writing: {:?}",
            report.findings
        );
        assert_eq!(report.retired, 0);
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.action == ParkAction::Reported),
            "a report-only pass must say so in the report as well as by not writing — a caller \
             reading `action` is entitled to the same answer the probe enforces: {:?}",
            report.findings
        );
    }

    /// A pass that stopped short says so, and an empty finding list from one is
    /// not a clean bill of health.
    #[tokio::test]
    async fn a_pass_that_did_not_see_the_whole_store_says_so() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent-a"), &["child-a"]).await;
        park_on(&store, &key("parent-b"), &["child-b"]).await;
        child_still_running(&store, &key("child-a")).await;
        child_still_running(&store, &key("child-b")).await;

        let report = LoopReconciler::new(
            &store,
            ReconcilerPolicy {
                scan_limit: 1,
                ..ReconcilerPolicy::default()
            },
        )
        .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
        .await
        .expect("a pass");
        assert_eq!(report.parked, 1, "only one park fits in the limit");
        assert!(
            report.incomplete,
            "a pass that stopped at its limit must not read as a full sweep"
        );
    }

    /// A park naming more children than one pass will load is not proved.
    ///
    /// The ceiling exists so one malformed state cannot make a pass unbounded.
    /// Reporting `Unwakeable` from a partial scan would be a proof about the
    /// children that were read dressed as one about the children that were named
    /// — and this is the assertion an off-by-one in the comparison trips.
    #[tokio::test]
    async fn a_park_naming_more_children_than_a_pass_will_load_is_not_proved() {
        let store = MemoryLoopStateStore::new();
        let many: Vec<String> = (0..=MAX_CHILDREN_DIAGNOSED)
            .map(|index| format!("child-{index}"))
            .collect();
        let parent = key("parent");
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Children {
            child_execution_ids: many.clone(),
            // Addressed, though the ceiling is checked before any child is read
            // — so this fixture would behave the same unaddressed. Written the
            // production way anyway: a fixture that differs from production
            // where it does not have to is a fixture that stops matching it.
            resume: addressed(&many.iter().map(String::as_str).collect::<Vec<_>>()),
        });
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");
        // Not one of them has committed anything, so a pass that ignored the
        // ceiling would answer `ChildNeverCommitted` — a different, and here
        // wrong, reason.

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Stalled);
        assert_eq!(
            finding.reason,
            ParkReason::TooManyChildrenToDiagnose {
                children: many.len()
            }
        );
    }

    /// A park naming no children at all cannot be woken by anything.
    #[tokio::test]
    async fn a_park_naming_no_children_is_unwakeable_by_construction() {
        let store = MemoryLoopStateStore::new();
        // `WaitReason::children` refuses to build this and `check_park` refuses
        // to commit it, so the fixture assembles the variant directly — which is
        // how a state carrying it would ever reach disk, and therefore the shape
        // a reconciler has to be able to read.
        park_on(&store, &key("parent"), &[]).await;

        let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(finding.reason, ParkReason::NoChildrenNamed);
    }

    /// Records above a child's watermark are an orphaned attempt and prove
    /// nothing about whether it finished.
    ///
    /// A production change that scanned the whole log instead of the
    /// authoritative prefix would call this child finished and report its parent
    /// unwakeable — on the strength of a record no commit ever vouched for.
    #[tokio::test]
    async fn a_terminal_a_dead_worker_left_above_the_watermark_does_not_end_a_child() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1"]).await;

        let child = key("child-1");
        // The child commits an ordinary phase completion...
        let seq = store
            .append_journal(
                &child,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        let mut state = fresh_state(&child);
        state.journal_seq = seq;
        store
            .commit(&child, &state, Revision::INITIAL)
            .await
            .expect("commit");
        // ...and then a worker appends a terminal and dies before committing it.
        store
            .append_journal(
                &child,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Success,
                    },
                )],
            )
            .await
            .expect("the orphaned attempt");

        let report = LoopReconciler::new(&store, policy())
            .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(report.examined, 1, "the park was diagnosed");
        assert!(
            report.findings.is_empty(),
            "an uncommitted terminal is not an ending: {:?}",
            report.findings
        );
    }

    /// A store that cannot list fails the pass rather than reporting an all-clear.
    #[tokio::test]
    async fn a_store_that_cannot_list_fails_the_pass_rather_than_reporting_nothing() {
        let probe = ProbeStore::over(MemoryLoopStateStore::new()).with_an_unavailable_listing();
        let error = LoopReconciler::new(&probe, policy())
            .reconcile_at(Utc::now().timestamp_millis())
            .await
            .expect_err("a store that cannot answer must not produce an empty all-clear");
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "got {error}"
        );
    }

    // ========================================================================
    // Recovery
    // ========================================================================

    /// The gap this module was built to close: a proved park is ended, and ended
    /// in a way a driver will refuse rather than one it would try to run.
    ///
    /// Four separate claims, because three of them pass on implementations the
    /// fourth catches and vice versa:
    ///
    /// - the finding says a retirement happened,
    /// - the terminal is **non-resumable**, which is what makes the next claim
    ///   answer `RunAlreadyEnded` instead of re-entering the phase that parked,
    ///   and what the module's *pick the right resumability* constraint is about,
    /// - the wait is cleared, which is what takes the row out of `list_parked`,
    /// - and the committed cursor is **still** the cursor its own journal replays
    ///   to. That last one is the assertion a record written at any other address
    ///   fails, and its failure mode is a quarantine on the next claim — a run
    ///   this module broke rather than retired.
    ///
    /// What production change leaves this green: none of the four. Writing a
    /// resumable terminal leaves the second red. Clearing the wait without
    /// appending leaves the second red for a different reason — there is no
    /// terminal at all. Appending at a fresh iteration or the next phase leaves
    /// the fourth red.
    #[tokio::test]
    async fn a_park_proved_by_its_children_is_retired_with_a_terminal_a_driver_will_refuse() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1", "child-2"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        child_ending_as(&store, &key("child-2"), TerminalKind::Failed).await;

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            finding.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed
            }
        );

        let replayed = replayed_by_a_driver(&store, &parent).await;
        let terminal = replayed.terminal.expect("the run now records an ending");
        assert!(
            !terminal.is_resumable(),
            "a resumable terminal is cleared by the next record and leaves the run claimable, so \
             the park would come straight back: {terminal:?}"
        );
        assert!(
            state_of(&store, &parent).await.wait.is_none(),
            "a retired park must leave the listing, or the next pass re-diagnoses it forever"
        );
    }

    /// The same retirement, onto the journal a real park actually has.
    ///
    /// # What this catches that the test above cannot
    ///
    /// Every other retirement fixture parks with an EMPTY journal at watermark
    /// zero, and a real park never looks like that: `commit_boundary` appends
    /// `RunEnded { WaitingForChildren }` as the last record of its batch and
    /// leaves the cursor on that record's own address. Two things are only
    /// reachable on that shape, and a `park_on` fixture reaches neither:
    ///
    /// - **`retire_under_lease`'s `replayed.terminal` is `Some(..)` here**, so
    ///   the append is legal *because the prior terminal is resumable* rather
    ///   than because there is no prior terminal to diverge from. That branch is
    ///   `replay_each`'s "a resumable ending is not the end of the log", and
    ///   `a_run_whose_journal_already_ends_for_good_is_not_retired_again` only
    ///   ever reaches its refusing half.
    /// - **The append lands on a DUPLICATE journal address** —
    ///   `(iteration, phase, ordinal)` identical to the park record's. Legal
    ///   only because `Journal::check_batch_addresses` is batch-scoped and
    ///   `ProjectorCursor::project` skips non-`Event` bodies, and asserted here
    ///   so that a later log-scoped uniqueness check fails a test instead of a
    ///   production retirement.
    ///
    /// What production change leaves this green: not moving the append off the
    /// committed cursor — `replayed_by_a_driver` fails, because the state and
    /// its own journal would then disagree and the next claim would quarantine
    /// the run rather than answering `RunAlreadyEnded`. Not writing before the
    /// resumable terminal is read either: the whole point of the shape is that
    /// there IS one to read.
    #[tokio::test]
    async fn a_park_committed_on_its_own_run_ended_record_is_retired_at_that_address() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on_after_the_boundary_that_parked_it(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let before = journal_of(&store, &parent).await;
        assert_eq!(before.len(), 1, "the park is one record");
        assert!(
            matches!(
                before[0].body,
                JournalBody::PhaseCompleted {
                    step: RecordedStep::RunEnded {
                        terminal: TerminalKind::WaitingForChildren
                    }
                }
            ),
            "the fixture must leave a RESUMABLE terminal, or it is testing the refusal instead: \
             {:?}",
            before[0].body
        );

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(
            finding.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed
            },
            "a park sitting on a resumable terminal is retirable; refusing it here would leave \
             every real park unretirable while every fixture retired fine"
        );

        let after = journal_of(&store, &parent).await;
        assert_eq!(after.len(), 2, "the retirement appends exactly one record");
        assert_eq!(
            (
                after[1].iteration,
                after[1].phase,
                after[1].ordinal,
                after[1].body.clone()
            ),
            (
                after[0].iteration,
                after[0].phase,
                after[0].ordinal,
                JournalBody::PhaseCompleted {
                    step: RecordedStep::RunEnded {
                        terminal: TerminalKind::CannotProceed
                    }
                }
            ),
            "the terminal goes at the committed cursor, which is the park record's own address \
             — so the two records collide, and that collision is legal only while \
             `check_batch_addresses` is batch-scoped"
        );

        // Replays, and replays to the committed cursor. This is the assertion
        // that fails if the append moves to a fresh iteration or the next phase.
        let replayed = replayed_by_a_driver(&store, &parent).await;
        assert_eq!(
            replayed.terminal,
            Some(TerminalKind::CannotProceed),
            "the resumable park terminal is cleared by the record that follows it, and the \
             retirement's is what the log now ends on"
        );
        assert!(
            state_of(&store, &parent).await.wait.is_none(),
            "a retired park must leave the listing"
        );
    }

    /// The deadline ground, and the one property that is only true of it.
    ///
    /// Every retired run is withheld from `list_runnable` by the ending marker
    /// its own retiring commit publishes — that is
    /// `a_run_the_children_ground_retires_is_withheld_by_the_scan`, and there
    /// the marker is the *only* thing doing the withholding, which is why that
    /// case has to assert the wait was cleared first. A deadline-passed run is
    /// withheld twice over: the scan tests `deadline_passed` as well, and would
    /// go on withholding this key with no marker published at all. Asserting
    /// both sides in one place is what stops the two from being confused —
    /// reading this case as evidence about the marker would be reading a pass
    /// that a reverted marker leaves green.
    #[tokio::test]
    async fn a_park_past_its_deadline_is_retired_and_is_still_withheld_from_the_scan() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let now_ms = Utc::now().timestamp_millis();
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        // In the real past, not merely before the pass's clock: `list_runnable`
        // reads its own `Utc::now()` and would offer a run whose deadline had
        // only passed relative to a parameter.
        state.deadline_at_ms = Some(now_ms - 1_000);
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let finding = one_finding_with(&store, recovering_policy(), now_ms).await;
        assert_eq!(
            finding.reason,
            ParkReason::DeadlinePassedWhileParked {
                deadline_at_ms: now_ms - 1_000
            }
        );
        assert_eq!(
            finding.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed
            }
        );

        let runnable = store
            .list_runnable(&another_worker(), 100)
            .await
            .expect("a scan");
        assert!(
            !runnable.contains(&parent),
            "a deadline-passed run is withheld by the scan whatever its wait says: {runnable:?}"
        );
    }

    /// A parked run has no phase host to drain prior outbox debt. Retirement
    /// publishes an exact ending marker without moving the projector cursor, so
    /// the placement-independent terminal owner can discover and drain the
    /// complete committed prefix instead of deadlocking the two lifecycles.
    #[tokio::test]
    async fn retirement_exposes_prior_event_debt_to_the_terminal_projector() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent-with-owed-event");
        let now_ms = Utc::now().timestamp_millis();
        let seq = store
            .append_journal(
                &parent,
                &[JournalAppend::named_event(
                    1,
                    Phase::first(),
                    "agentic.iteration_started",
                    "agent-under-test",
                    None,
                    None,
                    serde_json::json!({"execution_id": parent.execution_id()}),
                )
                .expect("the fixture event fits one journal record")],
            )
            .await
            .expect("the event is appended");
        let mut state = fresh_state(&parent);
        state.journal_seq = seq;
        state.wait = Some(WaitReason::Job {
            job_id: "coding-owed-event".to_string(),
        });
        state.deadline_at_ms = Some(now_ms - 1_000);
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("the parent parks with an authoritative event");

        let finding = one_finding_with(&store, deadline_ground_only(), now_ms).await;
        assert_eq!(
            finding.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed,
            }
        );
        assert!(
            state_of(&store, &parent).await.wait.is_none(),
            "the expired park is retired without waiting for a nonexistent phase host"
        );
        assert_eq!(
            journal_of(&store, &parent).await.len(),
            2,
            "retirement appends a terminal after the still-authoritative event"
        );
        let debt = store
            .scan_terminal_outbox_debt(std::num::NonZeroUsize::new(8).expect("non-zero page"), None)
            .await
            .expect("scan terminal debt");
        assert_eq!(
            debt.keys,
            vec![parent],
            "the unchanged projector cursor leaves the full terminal prefix discoverable"
        );
    }

    #[tokio::test]
    async fn retirement_refuses_a_runtime_settlement_mark_beyond_the_committed_prefix() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent-with-impossible-runtime-mark");
        let now_ms = Utc::now().timestamp_millis();
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-impossible-runtime-mark".to_string(),
        });
        state.deadline_at_ms = Some(now_ms - 1_000);
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit expired park");
        let mut cursor = ProjectorCursor::new();
        cursor.mark_runtime_terminal_settled(1);
        store
            .save_projector_cursor(&parent, &cursor)
            .await
            .expect("publish impossible runtime mark fixture");

        let finding = one_finding_with(&store, deadline_ground_only(), now_ms).await;
        assert!(matches!(
            finding.action,
            ParkAction::NotRetired(RetirementRefused::JournalUnusable { .. })
        ));
        assert!(state_of(&store, &parent).await.wait.is_some());
        assert!(journal_of(&store, &parent).await.is_empty());
    }

    /// The split the per-ground filter exists for, in one pass each.
    ///
    /// A policy naming **only** [`RetirableGround::DeadlinePassedWhileParked`]
    /// retires a deadline-ground park and refuses an every-child-finished one —
    /// and refuses it with [`RetirementRefused::GroundNotOptedIn`], not with
    /// [`RetirementRefused::GroundIsNotActedOn`]. The two refusals are the whole
    /// reason a new variant was added rather than the existing one reused: this
    /// ground *is* retirable, and what stopped it was this pass's configuration.
    /// A report that spelled it `GroundIsNotActedOn` would tell an operator the
    /// opposite of the truth — that no policy could ever retire it.
    ///
    /// Two stores rather than two parks in one, because the assertion on the
    /// refused side is that **nothing was written**, and one store holding a
    /// park that was retired beside a park that was not makes "nothing was
    /// written" a claim about the wrong key if the two are ever confused.
    ///
    /// What production change leaves this green: none. Dropping the filter turns
    /// the second half red on the action; dropping the terminal from the
    /// deadline ground turns the first half red; folding the new refusal back
    /// into `GroundIsNotActedOn` turns the second half red on the variant.
    #[tokio::test]
    async fn a_policy_naming_one_ground_retires_it_and_refuses_the_other() {
        let now_ms = Utc::now().timestamp_millis();

        // ── the ground this policy named ────────────────────────────────────
        let deadline_store = MemoryLoopStateStore::new();
        let past_its_deadline = key("past-its-deadline");
        let mut state = fresh_state(&past_its_deadline);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        state.deadline_at_ms = Some(now_ms - 1_000);
        deadline_store
            .commit(&past_its_deadline, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let retired = one_finding_with(&deadline_store, deadline_ground_only(), now_ms).await;
        assert_eq!(
            retired.reason,
            ParkReason::DeadlinePassedWhileParked {
                deadline_at_ms: now_ms - 1_000
            }
        );
        assert_eq!(
            retired.action,
            ParkAction::Retired {
                terminal: TerminalKind::CannotProceed
            },
            "the ground the policy named must still be acted on"
        );
        assert!(
            state_of(&deadline_store, &past_its_deadline)
                .await
                .wait
                .is_none(),
            "a retirement clears the wait it retired"
        );

        // ── the ground it did not ───────────────────────────────────────────
        let children_store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&children_store, &parent, &["child-1"]).await;
        child_ending_as(&children_store, &key("child-1"), TerminalKind::Success).await;

        let refused =
            one_finding_with(&children_store, deadline_ground_only(), now_ms + HOUR_MS).await;
        assert_eq!(refused.verdict, ParkVerdict::Unwakeable);
        assert_eq!(
            refused.reason,
            ParkReason::EveryChildHasFinished { children: 1 },
            "the filter must not change what a pass DIAGNOSES, only what it acts on"
        );
        assert_eq!(
            refused.action,
            ParkAction::NotRetired(RetirementRefused::GroundNotOptedIn)
        );
        assert!(
            state_of(&children_store, &parent).await.wait.is_some(),
            "a ground the policy did not name must leave the state exactly as it was"
        );
        assert!(
            journal_of(&children_store, &parent).await.is_empty(),
            "and must not append a terminal to a log it declined to end"
        );
    }

    /// A retired park is gone from the next pass, which is the population fix.
    ///
    /// The module's cost section says the store's steady state is *"the same
    /// `scan_limit` parks, again"* and that retiring is what moves the window.
    /// This is that sentence as an assertion: the second pass does not merely
    /// stop *reporting* the park, it stops *seeing* it — `parked` is zero, not
    /// `findings`.
    #[tokio::test]
    async fn a_retired_park_is_gone_from_the_next_pass() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let reconciler = LoopReconciler::new(&store, recovering_policy());
        let now_ms = Utc::now().timestamp_millis() + HOUR_MS;

        let first = reconciler.reconcile_at(now_ms).await.expect("a pass");
        assert_eq!(first.parked, 1);
        assert_eq!(first.retired, 1);

        let second = reconciler
            .reconcile_at(now_ms)
            .await
            .expect("a second pass");
        assert_eq!(
            second.parked, 0,
            "the retired park must leave the listing, not merely stop being reported"
        );
        assert_eq!(second.retired, 0);
        assert!(second.findings.is_empty());
    }

    /// Retirement is terminal for **discovery**, not only for the parked listing.
    ///
    /// # This assertion used to run the other way, and the tripwire that flipped it
    ///
    /// It read *"a retired run is offered by every scan, claimed, and answered
    /// `RunAlreadyEnded`"*, with a message saying that if the store ever learned
    /// to withhold an ended run the cost was gone and this case should be
    /// deleted rather than weakened. The store learned: both `commit`
    /// implementations now derive `store::non_resumable_terminal` over the
    /// prefix the committed watermark vouches for and publish it, and both scans
    /// skip a key whose `ended.seq == state.journal_seq`. So the case is kept
    /// and inverted rather than deleted — the property is still worth a test,
    /// only the answer changed.
    ///
    /// # Why the cleared wait is asserted BEFORE the scan
    ///
    /// `list_runnable` withholds a *parked* run as well, so "not offered" on its
    /// own is an answer this case would produce even if the retirement had done
    /// nothing whatsoever. `wait.is_none()` is what makes the last assertion
    /// mean *the published terminal withheld it* rather than *the park is still
    /// there*. The retirement lease is released by `retire`, so it is not the
    /// lease either, and nothing here sets a deadline — which is what separates
    /// this case from the deadline ground's, where `deadline_passed` withholds
    /// the key on its own and proves nothing about the marker.
    ///
    /// # What production change would leave this green
    ///
    /// None that matters. Dropping the ending marker from either store, or
    /// widening `RetirableGround::terminal` onto a **resumable** kind — for which
    /// `non_resumable_terminal` publishes nothing at all — puts the key back
    /// into the scan and turns the last assertion red. A retirement that stopped
    /// happening turns `retired` red first, and one that stopped clearing the
    /// wait turns the middle assertion red before the scan could pass vacuously.
    ///
    /// The filesystem-specific crash boundary is covered in `store::fs`: its
    /// prepublished marker retains both the current and proposed revision, so a
    /// failed successor snapshot cannot hide this terminal binding.
    #[tokio::test]
    async fn a_run_the_children_ground_retires_is_withheld_by_the_scan() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        assert!(
            !store
                .list_runnable(&another_worker(), 100)
                .await
                .expect("a scan")
                .contains(&parent),
            "while it is parked, the scan withholds it"
        );

        let report = LoopReconciler::new(&store, recovering_policy())
            .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(
            report.retired, 1,
            "the children ground has to actually retire, or everything below passes for the \
             wrong reason"
        );

        assert!(
            state_of(&store, &parent).await.wait.is_none(),
            "the retirement clears the wait, so the park is no longer what withholds this key"
        );
        assert!(
            !store
                .list_runnable(&another_worker(), 100)
                .await
                .expect("a scan")
                .contains(&parent),
            "the retiring commit publishes an ending at the seq it just committed, and both \
             scans withhold exactly that — so an un-parked retired run is still never offered"
        );
    }

    /// The ground that is proved and still never acted on.
    ///
    /// `NoChildrenNamed` is unwakeable by construction and is **evidence of a
    /// writer that bypassed two refusals**, so the state's other fields — its
    /// cursor, its watermark, its placement — are not ones to write a permanent
    /// terminal on the strength of. The row is still reported, and it says
    /// exactly why it was not acted on.
    ///
    /// What production change leaves this green: none. Adding the ground to
    /// `RetirableGround` — and so to `terminal_for`, which projects it — turns
    /// it red on the action, because `recovering_policy` names every ground the
    /// type has; dropping the row entirely turns it red on the finding count.
    #[tokio::test]
    async fn a_park_naming_no_children_is_reported_and_never_retired() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &[]).await;

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Unwakeable);
        assert_eq!(finding.reason, ParkReason::NoChildrenNamed);
        assert_eq!(
            finding.action,
            ParkAction::NotRetired(RetirementRefused::GroundIsNotActedOn)
        );
        assert!(
            state_of(&store, &parent).await.wait.is_some(),
            "a ground that is not acted on must leave the state exactly as it was"
        );
        assert!(journal_of(&store, &parent).await.is_empty());
    }

    /// A suspicion is never acted on, however old it is.
    ///
    /// Thirteen hours past a job park's own twelve-hour grace, which is the
    /// oldest a row in this store gets. `Stalled` is a claim about what this
    /// module could not see, and time does not turn one into a proof.
    #[tokio::test]
    async fn a_stalled_park_is_never_retired_however_old() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Job {
            job_id: "coding-1".to_string(),
        });
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + 13 * HOUR_MS,
        )
        .await;
        assert_eq!(finding.verdict, ParkVerdict::Stalled);
        assert_eq!(
            finding.action,
            ParkAction::Reported,
            "a `Stalled` row is reported, not refused: 'recovery declined' is not news about a \
             park nothing claimed to have proved"
        );
        assert!(state_of(&store, &parent).await.wait.is_some());
        assert!(journal_of(&store, &parent).await.is_empty());
    }

    /// A run another worker holds is left exactly as it was.
    ///
    /// The constraint the whole recovery is built around: *a reconciler that
    /// mutates a run another worker holds is the strand it is trying to prevent*.
    /// The claim is what enforces it, and this is the assertion that the claim is
    /// taken **before** anything is decided rather than after.
    #[tokio::test]
    async fn a_run_another_worker_holds_is_left_exactly_as_it_was() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        // The lease lives in the store, not in this binding, so the returned
        // value is dropped straight away — holding it would only invite a reader
        // to think the binding is what keeps the claim alive.
        store
            .claim(&parent, &another_worker(), Duration::from_secs(600))
            .await
            .expect("another worker takes the lease");

        let finding = one_finding_with(
            &store,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Unwakeable,
            "the diagnosis is unchanged; only the action is refused"
        );
        assert!(
            matches!(
                &finding.action,
                ParkAction::NotRetired(RetirementRefused::LeaseHeld { by, .. })
                    if by == &another_worker()
            ),
            "got {:?}",
            finding.action
        );
        assert!(state_of(&store, &parent).await.wait.is_some());
        assert!(journal_of(&store, &parent).await.is_empty());
    }

    /// A pinned run is not retired by a worker that is not its pin.
    ///
    /// `claim` cannot check this — the placement lives in the state the claim is
    /// taken in order to read — so the check sits after the claim, exactly where
    /// `advance_under_lease` puts its twin. A run pinned to a live worker is the
    /// nearest thing this store has to *somebody else is on it*.
    #[tokio::test]
    async fn a_run_pinned_to_another_worker_is_not_retired() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let now_ms = Utc::now().timestamp_millis();
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Children {
            child_execution_ids: vec!["child-1".to_string()],
            resume: addressed(&["child-1"]),
        });
        state.placement = Placement::Pinned {
            worker: another_worker(),
            pinned_until_ms: now_ms + 24 * HOUR_MS,
        };
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let finding = one_finding_with(&store, recovering_policy(), now_ms + HOUR_MS).await;
        assert_eq!(
            finding.action,
            ParkAction::NotRetired(RetirementRefused::PinnedElsewhere {
                pinned_to: another_worker()
            })
        );
        assert!(state_of(&store, &parent).await.wait.is_some());
        assert!(journal_of(&store, &parent).await.is_empty());
    }

    /// A completion that lands between the proof and the write stops it.
    ///
    /// The diagnosis takes two ledger reads and issues a proof; the retirement
    /// takes a third, under the lease, immediately before the append. The probe
    /// answers empty for the first two and publishes before the third — a
    /// vocabulary neither the memory store nor a fake without a read counter can
    /// express.
    ///
    /// What production change leaves this green: none. Dropping the third read
    /// turns it red with `Retired`, which is a permanent terminal on a run that
    /// was about to wake.
    #[tokio::test]
    async fn a_completion_landing_before_the_terminal_is_written_stops_the_retirement() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        let probe = ProbeStore::over(store)
            .allowing_the_retirement_writes()
            .resolving_just_before_the_retirement();

        let finding = one_finding_with(
            &probe,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert_eq!(
            finding.verdict,
            ParkVerdict::Unwakeable,
            "the two diagnosis reads both answered empty, so the proof stands"
        );
        assert_eq!(
            finding.action,
            ParkAction::NotRetired(RetirementRefused::WakeResolvedFirst { resolutions: 1 })
        );
        assert!(state_of(&probe, &parent).await.wait.is_some());
        assert!(journal_of(&probe, &parent).await.is_empty());
    }

    /// A retirement that loses its commit leaves an orphan, never a cleared park.
    ///
    /// The append happens before the commit, so a lost compare-and-swap leaves
    /// the terminal **above** the watermark: never replayed, never projected,
    /// swept by the next append. The park is intact and the next pass tries
    /// again.
    ///
    /// The other order is what this is really asserting against. Committing the
    /// cleared wait first and then appending would, on the same failure, leave a
    /// run un-parked with **no** terminal — claimable, and re-entering the phase
    /// that parked it. That is the unbounded re-park the module refuses by name,
    /// produced by an ordering rather than by a decision.
    #[tokio::test]
    async fn a_retirement_that_loses_its_commit_leaves_an_orphan_rather_than_a_cleared_park() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        let probe = ProbeStore::over(store)
            .allowing_the_retirement_writes()
            .losing_every_commit();

        let finding = one_finding_with(
            &probe,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert!(
            matches!(
                finding.action,
                ParkAction::NotRetired(RetirementRefused::StaleCommit { .. })
            ),
            "got {:?}",
            finding.action
        );

        let state = state_of(&probe, &parent).await;
        assert!(
            state.wait.is_some(),
            "the park must survive a lost commit; a cleared wait with no terminal is a run that \
             re-enters the phase that parked it"
        );
        assert_eq!(
            state.journal_seq, 0,
            "the watermark must not move for a commit that was refused"
        );
        assert_eq!(
            journal_of(&probe, &parent).await.len(),
            1,
            "the appended terminal is on disk as an ORPHAN — above the watermark, so no replay \
             sees it and the next append sweeps it"
        );
    }

    /// A run whose journal already ends for good is not ended a second time.
    ///
    /// Appending after a non-resumable terminal is what `replay_each` refuses on
    /// the next read, so a second retirement would make the run permanently
    /// unloadable — the failure the module's constraint about resumability names.
    /// The guard is the same one `verify_journal` applies, run here before the
    /// append rather than at the next claim.
    #[tokio::test]
    async fn a_run_whose_journal_already_ends_for_good_is_not_retired_again() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        let seq = store
            .append_journal(
                &parent,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::first(),
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Failed,
                    },
                )],
            )
            .await
            .expect("append");
        let mut state = fresh_state(&parent);
        // Parked AND already over: not a shape a correct driver publishes, which
        // is why it is assembled here — a retirement that ran twice would produce
        // it, and the second run is what has to refuse.
        state.wait = Some(WaitReason::Children {
            child_execution_ids: vec!["child-1".to_string()],
            resume: addressed(&["child-1"]),
        });
        state.journal_seq = seq;
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let probe = ProbeStore::over(store).allowing_the_retirement_writes();
        let finding = one_finding_with(
            &probe,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert!(
            matches!(
                finding.action,
                ParkAction::NotRetired(RetirementRefused::JournalUnusable { .. })
            ),
            "got {:?}",
            finding.action
        );
        assert_eq!(
            journal_of(&probe, &parent).await.len(),
            1,
            "nothing may be appended after a terminal that ends the log"
        );
    }

    /// A state whose journal does not replay to its cursor is not written to.
    ///
    /// This is the shape `verify_journal` quarantines — the snapshot is a cache
    /// of the journal and a cache that disagrees with its source is not
    /// repairable by preferring either. A retirement that appended anyway would
    /// add a record to a log an operator is about to be asked to reconcile, and
    /// would do it at an address derived from the cursor that is already wrong.
    #[tokio::test]
    async fn a_state_whose_journal_does_not_replay_to_its_cursor_is_not_retired() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        // One committed `Continued` at the first phase moves the replayed cursor
        // to the second, while the state below keeps the default cursor.
        let seq = store
            .append_journal(
                &parent,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::first(),
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        let mut state = fresh_state(&parent);
        state.wait = Some(WaitReason::Children {
            child_execution_ids: vec!["child-1".to_string()],
            resume: addressed(&["child-1"]),
        });
        state.journal_seq = seq;
        store
            .commit(&parent, &state, Revision::INITIAL)
            .await
            .expect("commit");
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;

        let probe = ProbeStore::over(store).allowing_the_retirement_writes();
        let finding = one_finding_with(
            &probe,
            recovering_policy(),
            Utc::now().timestamp_millis() + HOUR_MS,
        )
        .await;
        assert!(
            matches!(
                finding.action,
                ParkAction::NotRetired(RetirementRefused::JournalUnusable { .. })
            ),
            "got {:?}",
            finding.action
        );
        assert_eq!(journal_of(&probe, &parent).await.len(), 1);
    }

    /// A child at a continue-or-cancel prompt is not a child that finished.
    ///
    /// # What this replaces, and why the thing it replaces was worse than nothing
    ///
    /// There was a source scan here asserting that `run_loop/phases/` constructs
    /// neither `AgenticOutcome::MaxIterationsReached` nor
    /// `AgenticOutcome::BudgetExhausted`, on the theory that no journal could
    /// then hold either non-resumable kind. **It was green and the claim was
    /// false.** `phases/resolve.rs`'s token-budget check calls
    /// `executor::conclude_budget_exhausted`, which builds
    /// `BudgetExhausted { pause_state: Some(..) }` unconditionally — so the
    /// literal the scan looked for was one function call outside the six files
    /// it read, and the gate was a string search standing where a property
    /// should have been.
    ///
    /// So the property is asserted about **this** module instead, where it is
    /// this module's to keep: whatever any phase does, a child whose journal
    /// records one of those two kinds must not be counted as finished and must
    /// not be allowed to prove a park unwakeable.
    ///
    /// What production change leaves this green: none that matters. Reading
    /// `!is_resumable()` as `Finished` again turns both cases into an
    /// `Unwakeable` / `EveryChildHasFinished` row and fails the first assertion.
    /// Answering `Live` instead — which also stops the proof — drops the finding
    /// entirely and fails the second.
    #[tokio::test]
    async fn a_child_at_a_continue_or_cancel_prompt_cannot_prove_a_park_unwakeable() {
        for terminal in [
            TerminalKind::BudgetExhausted,
            TerminalKind::MaxIterationsReached,
        ] {
            let store = MemoryLoopStateStore::new();
            let parent = key("parent");
            park_on(&store, &parent, &["child-1"]).await;
            child_ending_as(&store, &key("child-1"), terminal).await;

            // The acting policy, so the assertion covers the retirement and not
            // only the verdict: a proof this module still issued would be
            // written into the parent's journal permanently.
            let finding = one_finding_with(
                &store,
                recovering_policy(),
                Utc::now().timestamp_millis() + HOUR_MS,
            )
            .await;
            assert_eq!(
                finding.verdict,
                ParkVerdict::Stalled,
                "{terminal:?} is what `TerminalKind::from` collapses a pause-carrying outcome \
                 onto, so a child holding one may still be answerable by a person: {:?}",
                finding.reason
            );
            assert_eq!(
                finding.reason,
                ParkReason::ChildMayStillBeContinued {
                    child_execution_id: "child-1".to_string(),
                    terminal,
                },
                "the row has to name the child and the kind, or an operator cannot tell it from \
                 a park with no completer at all"
            );
            // `Reported` rather than a refusal, and the difference is `act_on`'s:
            // a refusal is what a PROVED park on a declined ground gets, and a
            // `Stalled` row never reaches that test. Asserting the refusal here
            // would be asserting that this row was proved.
            assert_eq!(
                finding.action,
                ParkAction::Reported,
                "a suspicion is never retired"
            );
            assert!(
                journal_of(&store, &parent).await.is_empty(),
                "{terminal:?}: nothing may be appended to the parent's log"
            );
            assert!(
                state_of(&store, &parent).await.wait.is_some(),
                "{terminal:?}: the park has to stand — the child may yet answer it"
            );
        }
    }

    /// One live child still outranks a child at a prompt.
    ///
    /// The held-fault property, checked for the new answer specifically. An
    /// implementation that returned on the first `MayStillBeContinued` instead
    /// of holding it would report a healthy park — a finding in the bucket the
    /// two-verdict split exists to keep clean.
    #[tokio::test]
    async fn a_child_at_a_prompt_beside_a_live_child_is_not_a_finding() {
        let store = MemoryLoopStateStore::new();
        park_on(&store, &key("parent"), &["child-1", "child-2"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::BudgetExhausted).await;
        child_running_past_a_boundary(&store, &key("child-2")).await;

        let report = LoopReconciler::new(&store, recovering_policy())
            .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(report.examined, 1, "the park was diagnosed");
        assert!(
            report.findings.is_empty(),
            "child-2 is live, so the park is legitimate: {:?}",
            report.findings
        );
    }

    /// The kinds that DO end a child for good still prove a park.
    ///
    /// The other half of the pair: without it, `may_still_be_continued`
    /// answering `true` for everything would leave the two tests above green and
    /// this module would never prove anything again.
    #[tokio::test]
    async fn the_endings_that_carry_no_prompt_still_finish_a_child() {
        for terminal in [
            TerminalKind::Success,
            TerminalKind::Failed,
            TerminalKind::LoopDetected,
            TerminalKind::CannotProceed,
        ] {
            let store = MemoryLoopStateStore::new();
            park_on(&store, &key("parent"), &["child-1"]).await;
            child_ending_as(&store, &key("child-1"), terminal).await;

            let finding = one_finding(&store, Utc::now().timestamp_millis() + HOUR_MS).await;
            assert_eq!(
                (finding.verdict, finding.reason),
                (
                    ParkVerdict::Unwakeable,
                    ParkReason::EveryChildHasFinished { children: 1 }
                ),
                "{terminal:?} ends a child in both vocabularies and must still prove the park"
            );
        }
    }

    /// A retirement uses four store writes and never the wake ledger.
    ///
    /// `resolve_wake`, `consume_wake` and `renew` are fatal on every probe, so a
    /// pass that retires without tripping one is the assertion. Stated as its own
    /// test because the property is easy to lose to a plausible-looking tidy-up:
    /// consuming the resolutions of a park being retired reads as housekeeping,
    /// and `wake/` is the one place a writer without the lease publishes.
    #[tokio::test]
    async fn a_retirement_never_touches_the_wake_ledger() {
        let store = MemoryLoopStateStore::new();
        let parent = key("parent");
        park_on(&store, &parent, &["child-1"]).await;
        child_ending_as(&store, &key("child-1"), TerminalKind::Success).await;
        let probe = ProbeStore::over(store).allowing_the_retirement_writes();

        let report = LoopReconciler::new(&probe, recovering_policy())
            .reconcile_at(Utc::now().timestamp_millis() + HOUR_MS)
            .await
            .expect("a pass");
        assert_eq!(
            report.retired, 1,
            "the pass has to reach a retirement, or it proves nothing about what one touches"
        );
    }

    /// Lease exclusion is fence-based, and reconciler instances remain distinct.
    ///
    /// The first half pins the store rule that a same-id claim cannot manufacture
    /// a fresh fence from a live lease; the holder must present the current lease
    /// to `renew`. The second pins both operational safeguards around reconciler
    /// identity: the namespace check and a fresh per-instance nonce.
    #[tokio::test]
    async fn a_same_id_claim_is_refused_and_reconciler_instances_are_unique() {
        let store = MemoryLoopStateStore::new();
        let contested = key("contested");
        let phase_worker = WorkerId::new("loop-worker-1");
        let held = store
            .claim(&contested, &phase_worker, Duration::from_secs(300))
            .await
            .expect("a phase worker takes the key");

        let error = store
            .claim(&contested, &phase_worker, RETIREMENT_LEASE_TTL)
            .await
            .expect_err("a same-id claim is still a new claim and must be refused");
        assert!(
            matches!(error, StoreError::LeaseHeld { by, .. } if by == phase_worker),
            "the original holder must retain the live lease"
        );
        store
            .release(held)
            .await
            .expect("the refused claim must not move the holder's fence");

        assert_eq!(
            ReconcilerWorker::named("loop-worker-1"),
            Err(NotAReconcilerId {
                offered: "loop-worker-1".to_string()
            }),
            "an id outside the reconciler namespace must not be constructible"
        );
        assert!(
            ReconcilerWorker::named(RECONCILER_WORKER_PREFIX).is_err(),
            "the bare prefix names nobody"
        );
        let ok = ReconcilerWorker::named("reconciler-7").expect("the documented shape");
        assert_eq!(ok.as_worker_id(), &WorkerId::new("reconciler-7"));
        let first = ReconcilerWorker::for_this_process();
        let second = ReconcilerWorker::for_this_process();
        assert_ne!(
            first, second,
            "two reconciler instances in one process need distinct audit and lease identities"
        );
        let process_prefix = format!("{RECONCILER_WORKER_PREFIX}{}-", std::process::id());
        for worker in [&first, &second] {
            assert!(
                worker.as_worker_id().as_str().starts_with(&process_prefix),
                "the generated id keeps its reconciler namespace and process attribution"
            );
        }
    }

    // ========================================================================
    // The probe
    // ========================================================================

    /// One store wrapper for every reconciler test that needs more than the
    /// memory store can say.
    ///
    /// # Which mutating methods panic depends on the probe, and three never stop
    ///
    /// `resolve_wake`, `consume_wake` and `renew` panic **always**, whatever the
    /// probe is configured for, and that is the assertion rather than test
    /// hygiene: the first two touch `wake/`, which is the one place a writer
    /// without the lease publishes and where dropping an id loses a real
    /// completion; the third would let a retirement hold a lease it could not
    /// finish inside. No path in this module may reach any of them.
    ///
    /// The four a **retirement** legitimately uses — `claim`, `append_journal`,
    /// `commit`, `release` — panic unless [`Self::allowing_the_retirement_writes`]
    /// was called. So the same wrapper states two different things depending on
    /// how it is built: *this pass writes nothing at all*, and *this pass writes
    /// only the four*.
    ///
    /// One wrapper rather than three, because `#[async_trait]` rewrites the whole
    /// `impl` block before `macro_rules!` inside it would expand — so shared
    /// method bodies cannot be factored into a macro, and three wrappers would be
    /// three copies of the same refusals.
    struct ProbeStore {
        inner: MemoryLoopStateStore,
        /// Answer the wake ledger as empty for this many reads, then publish a
        /// real resolution and answer with it.
        ///
        /// A count rather than a flag because the two windows a test needs to
        /// open are at different reads: `Some(1)` lands a completion between the
        /// cheap first read and the proof (the diagnosis downgrades), `Some(2)`
        /// lands it between the proof and the retirement's own third read under
        /// the lease (the retirement refuses). A flag could only ever express the
        /// first.
        resolve_after_reads: Option<usize>,
        /// Refuse the listing outright.
        listing_unavailable: bool,
        /// Refuse to `load` this one execution, so a diagnosis meets a child the
        /// store will not answer about.
        unreadable_child: Option<ExecutionKey>,
        /// Answer every `commit` with a conflict, as though another holder got
        /// there first.
        commit_conflicts: bool,
        /// Whether `claim` / `append_journal` / `commit` / `release` delegate
        /// instead of panicking.
        retirement_writes_allowed: bool,
        wake_reads: Mutex<usize>,
    }

    impl ProbeStore {
        fn over(inner: MemoryLoopStateStore) -> Self {
            Self {
                inner,
                resolve_after_reads: None,
                listing_unavailable: false,
                unreadable_child: None,
                commit_conflicts: false,
                retirement_writes_allowed: false,
                wake_reads: Mutex::new(0),
            }
        }

        fn with_an_unreadable_child(mut self, child: ExecutionKey) -> Self {
            self.unreadable_child = Some(child);
            self
        }

        fn resolving_during_the_diagnosis(mut self) -> Self {
            self.resolve_after_reads = Some(1);
            self
        }

        /// Empty for the diagnosis's two reads; resolved for the retirement's.
        fn resolving_just_before_the_retirement(mut self) -> Self {
            self.resolve_after_reads = Some(2);
            self
        }

        fn with_an_unavailable_listing(mut self) -> Self {
            self.listing_unavailable = true;
            self
        }

        fn allowing_the_retirement_writes(mut self) -> Self {
            self.retirement_writes_allowed = true;
            self
        }

        fn losing_every_commit(mut self) -> Self {
            self.commit_conflicts = true;
            self
        }
    }

    #[async_trait]
    impl LoopStateStore for ProbeStore {
        async fn load(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
            // An error rather than `Ok(None)`, because the two mean opposite
            // things to a diagnosis: absent is "this child never committed",
            // unreadable is "this store cannot say".
            if self.unreadable_child.as_ref() == Some(key) {
                return Err(StoreError::Unavailable {
                    detail: "this execution cannot be read".to_string(),
                });
            }
            self.inner.load(key).await
        }

        async fn read_journal(
            &self,
            key: &ExecutionKey,
            from_seq: u64,
        ) -> StoreResult<Vec<JournalRecord>> {
            self.inner.read_journal(key, from_seq).await
        }

        async fn load_effects(&self, key: &ExecutionKey) -> StoreResult<EffectLedger> {
            self.inner.load_effects(key).await
        }

        async fn load_projector_cursor(
            &self,
            key: &ExecutionKey,
        ) -> StoreResult<Option<ProjectorCursor>> {
            self.inner.load_projector_cursor(key).await
        }

        // Forwarded rather than left to the trait default. The default answers
        // `Ok(None)`, which is the fail-closed reading — but a probe that
        // silently withheld every receipt its substrate holds would make each
        // children-ground case below pass for the wrong reason on its way to
        // failing for the right one, and would hide a regression in the read
        // itself behind a fixture that never had anything to read.
        async fn load_chain_closure(
            &self,
            key: &ExecutionKey,
        ) -> StoreResult<Option<ChainClosure>> {
            self.inner.load_chain_closure(key).await
        }

        async fn list_runnable(
            &self,
            worker: &WorkerId,
            limit: usize,
        ) -> StoreResult<Vec<ExecutionKey>> {
            self.inner.list_runnable(worker, limit).await
        }

        async fn list_parked(&self, limit: usize) -> StoreResult<ParkedListing> {
            if self.listing_unavailable {
                return Err(StoreError::Unavailable {
                    detail: "the substrate is gone".to_string(),
                });
            }
            self.inner.list_parked(limit).await
        }

        async fn wake_resolutions(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
        ) -> StoreResult<Vec<String>> {
            if let Some(quiet_reads) = self.resolve_after_reads {
                let still_quiet = {
                    let mut reads = self.wake_reads.lock().expect("the counter is not poisoned");
                    *reads += 1;
                    *reads <= quiet_reads
                };
                if still_quiet {
                    return Ok(Vec::new());
                }
                // Published through the real store, so the later reads answer
                // from the substrate rather than from a hard-coded reply — a fake
                // that simply returned a string would pass even if the reconciler
                // had stopped asking the store at all.
                //
                // Idempotent on `(token, resolution_id)`, so publishing again on
                // every subsequent read leaves exactly one resolution outstanding
                // rather than accumulating one per read.
                self.inner
                    .resolve_wake(key, wake_token, "landed-mid-diagnosis")
                    .await?;
            }
            self.inner.wake_resolutions(key, wake_token).await
        }

        async fn commit(
            &self,
            key: &ExecutionKey,
            state: &LoopState,
            expected: Revision,
        ) -> StoreResult<Revision> {
            assert!(
                self.retirement_writes_allowed,
                "the reconciler committed a state"
            );
            if self.commit_conflicts {
                return Err(StoreError::Conflict {
                    expected,
                    // A revision beyond the one the caller loaded, which is what
                    // a real conflict looks like.
                    found: expected.next(),
                });
            }
            self.inner.commit(key, state, expected).await
        }

        async fn append_journal(
            &self,
            key: &ExecutionKey,
            appends: &[JournalAppend],
        ) -> StoreResult<u64> {
            assert!(
                self.retirement_writes_allowed,
                "the reconciler appended to a journal"
            );
            self.inner.append_journal(key, appends).await
        }

        async fn record_effect_intent(
            &self,
            _key: &ExecutionKey,
            _entry: &EffectLedgerEntry,
        ) -> StoreResult<()> {
            panic!("the reconciler recorded an effect intent");
        }

        async fn record_effect_outcome(
            &self,
            _key: &ExecutionKey,
            _effect_id: &EffectId,
            _outcome: EffectOutcome,
        ) -> StoreResult<()> {
            panic!("the reconciler recorded an effect outcome");
        }

        async fn claim(
            &self,
            key: &ExecutionKey,
            worker: &WorkerId,
            ttl: Duration,
        ) -> StoreResult<Lease> {
            assert!(
                self.retirement_writes_allowed,
                "the reconciler took a lease"
            );
            self.inner.claim(key, worker, ttl).await
        }

        /// Fatal on every probe. A retirement is four store calls with no phase
        /// between them; one that needed to extend its lease is a store that has
        /// stopped answering, and the answer to that is to stop.
        async fn renew(&self, _lease: &Lease, _ttl: Duration) -> StoreResult<Lease> {
            panic!("the reconciler renewed a lease");
        }

        async fn release(&self, lease: Lease) -> StoreResult<()> {
            assert!(
                self.retirement_writes_allowed,
                "the reconciler released a lease"
            );
            self.inner.release(lease).await
        }

        /// Fatal on every probe. See [`ProbeStore`]'s own docs: publishing a
        /// completion this module did not observe is the recovery it refuses by
        /// name.
        async fn resolve_wake(
            &self,
            _key: &ExecutionKey,
            _wake_token: &str,
            _resolution_id: &str,
        ) -> StoreResult<()> {
            panic!("the reconciler resolved a wake token");
        }

        /// Fatal on every probe. `wake/` is where a writer without the lease
        /// publishes; dropping an id there can lose a real completion.
        async fn consume_wake(
            &self,
            _key: &ExecutionKey,
            _wake_token: &str,
            _resolution_ids: &[String],
        ) -> StoreResult<usize> {
            panic!("the reconciler consumed a wake resolution");
        }
    }
}
