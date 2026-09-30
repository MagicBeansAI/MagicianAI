//! The loop around [`advance_once`]: discover, claim, advance, release, move on.
//!
//! # What was missing, stated as the gap it closes
//!
//! [`advance_once`] advances **one phase of one execution** and returns. That is
//! deliberate — it is what makes a worker short-lived — but it means the
//! stateless arm has no way to *find* work. Until this module,
//! `MAGICIAN_EXECUTION_DRIVER=stateless` reached `advance_once` through exactly
//! one caller: `executor.rs`'s `StatelessArm`, which drives the run its own
//! process is already running, under a key it minted itself. Nothing anywhere
//! called [`LoopStateStore::list_runnable`] outside its own contract suite.
//!
//! This module is the generic scheduler half: given a store and exact hosts it
//! is already authorized to lend, it sweeps the runnable set and advances what
//! it can. It is not the production authority composer. Restart recovery first
//! validates an Artifact binding, sealed child binding, or durable PlanGraph and
//! then re-enters the ordinary execution lifecycle under the same id.
//!
//! # `list_runnable` is a FILTER, not a queue, and a runner that forgets it spins
//!
//! The most important property to hold in view while reading the rest of this
//! file. It was verified against both implementations (`store/fs.rs::runnable_at`
//! and the filter inside `store/memory.rs::scan_runnable`) rather than assumed.
//! Both stores write `list_runnable` as a call into `scan_runnable`, so the
//! filter has one home and the paged and unpaged answers cannot drift.
//!
//! Both answer one question about the **committed state** — *is this execution
//! claimable, unleased, unparked, inside its deadline and past its retry time?*
//! — and, since the ending marker landed, one about the **journal** as well.
//!
//! ## The ending marker, because it retired the paragraph that used to be here
//!
//! An earlier draft of this file said *"neither can see that a run has ended …
//! an execution that finished successfully an hour ago is offered by every scan
//! forever"*, and that was true of the stores it was written against. It is not
//! true of the stores in this tree. `store/memory.rs::commit` and
//! `store/fs.rs::commit` both run `store::non_resumable_terminal` over the
//! journal prefix the *committed watermark* vouches for and publish the result
//! beside the state — `slot.ended` in memory, `ended.json` on disk — and both
//! scans withhold any key whose revision-bound ending satisfies `ended.seq ==
//! state.journal_seq`. A finished run is therefore **not** offered on that path,
//! and `HoldReason::RunEnded` is reachable only inside the sweep that ends the
//! run: the next scan has already withheld the key.
//!
//! The comparison, not merely the presence of a marker, is what makes that safe
//! to rely on. A `RunEnded` record sitting *above* the watermark is an orphan
//! left by a worker that appended and died — the run has not ended — and neither
//! store publishes an ending for it.
//!
//! ### The filesystem marker is revision-bound
//!
//! `store/memory.rs` assigns `slot.ended` under the state mutex. The filesystem
//! store prepublishes current and proposed revision bindings before snapshot
//! CAS, so either snapshot selects its exact ending across failure or process
//! loss. Claim-time repair remains for legacy snapshots and operator-removed
//! markers. An unreadable or structurally incomplete revision marker is the
//! fail-closed direction and is quarantined by the scan.
//!
//! ## What discovery STILL cannot see, which is what the holdoff set is for
//!
//! The marker names exactly one population: a **non-resumable** terminal the
//! watermark vouches for. Every other permanent-or-sticky answer is still
//! invisible to both scans, and each is still offered forever:
//!
//! - a run stopped at a **resumable** terminal that committed no wait — a pause,
//!   a confirmation. `TerminalKind::is_resumable` is true so no marker is
//!   published, and `WaitReason` has no variant for a human so the state carries
//!   nothing to withhold it by either. `advance_under_lease` lets such a
//!   terminal fall through and RE-RUNS the phase that ended, which is the resume
//!   mechanism and is also a paid `Decide`. This is the population the widening
//!   below is argued and tested against;
//! - a run that is quarantined, past its iteration ceiling, or holding an
//!   indeterminate effect. None of the three is a journal terminal and none is a
//!   state field either scan reads;
//! - a run whose committed cursor sits **inside** an iteration, which no store
//!   reads at all;
//! - a key **this fleet cannot host**, which is not a property of the store's
//!   answer in the first place and is most of what a narrow fleet is offered.
//!
//! A runner that trusted the scan would re-claim those at full speed and —
//! because they occupy the scan's `limit` — would starve the live work sitting
//! behind them.
//!
//! So this runner keeps a **holdoff set**: a key that answered something no
//! further claim can change is not offered to `advance_once` again while this
//! worker lives. For the finished-run half it is now belt to the store's braces,
//! and it is kept rather than deleted because it is what a runner meets a
//! *regressed or decorated* store with. For every bullet above it is still the
//! only thing between a narrow fleet and a scan-rate spin. The set is bounded
//! ([`RunnerConfig::max_held_keys`]) and reports its evictions, because an
//! unbounded memo inside a process whose whole point is that it can be
//! cold-started would be its own defect.
//!
//! ## A holdoff set alone fixes only HALF of that, and the other half is silent
//!
//! Not re-claiming a held key stops the hot re-claim. It does **not** stop the
//! starvation, and an earlier version of this paragraph claimed it did.
//!
//! [`LoopStateStore::list_runnable`] takes a `limit` and no offset and no
//! cursor. Both implementations return the FIRST `limit` claimable keys in a
//! fixed order and then stop — `store/memory.rs` walks a `BTreeMap` in key order
//! and breaks at `limit`; `store/fs.rs` walks `read_dir` order and returns
//! `Walk::Stop` at `limit`. A key this runner is holding is still claimable,
//! unleased, unparked and inside every timer, so it goes on occupying its slot
//! in that prefix forever, and the holdoff set is process-local: nothing the
//! runner remembers moves where the scan stops.
//!
//! With a fixed scan limit, `RunnerConfig::batch` **paused** runs sitting at the
//! head of the walk are enough to make every later sweep report `offered =
//! batch, skipped_held = batch, attempted = 0` for the rest of the process's
//! life, while live executions behind them are never offered at all.
//!
//! Paused rather than finished, and the substitution is not cosmetic: the
//! finished ones are withheld by both stores now, so an argument built on them
//! would be an argument for a mitigation against a hazard that no longer exists.
//! A pause is the population that still does this, because nothing in either
//! store can see one — see the bullets above.
//!
//! ## Two answers, and they cover two different prefixes
//!
//! **The first page asks for `batch` keys plus however many this runner is
//! currently holding.** That is the smallest limit at which a prefix made
//! entirely of *keys this runner has already refused* still leaves room for
//! `batch` unheld ones, and it is bounded by construction: the holdoff set never
//! exceeds [`RunnerConfig::max_held_keys`], so the first page never asks for
//! more than `batch + max_held_keys`. [`SweepReport::scan_limit`] reports the
//! number actually used, because a limit that has grown is the visible symptom
//! of a store offering work nobody can do.
//!
//! That covers only the keys this process has personally met and remembered. It
//! does nothing about a prefix of refusals this worker has never seen — a fresh
//! process, or any caller that constructs a runner for only one sweep. For
//! those, the widened first page is still `limit` keys with no offset, and the
//! head of the walk still owns every slot.
//!
//! **So the sweep pages.** [`LoopStateStore::scan_runnable`] answers one page
//! plus a [`ScanCursor`] naming where the next one resumes, and
//! [`WorkerRunner::sweep`] feeds that cursor back until it has the batch it
//! wants, the walk ends, the sweep stops for its own reasons, or
//! [`RunnerConfig::max_scan_pages`] fires. [`SweepReport::scan_end`] says which
//! of the four happened, and [`ScanEnd::PageCap`] is the one that means *there
//! is store behind this sweep that nothing looked at*.
//!
//! ### The two are complementary, and compounding them was the trap
//!
//! Every page after the first asks for `batch` and **not** `batch + held`. The
//! widening exists to get past a prefix of held keys; the cursor has already
//! carried the walk past that prefix, so widening a later page asks the store to
//! walk a second prefix-sized window for keys there is no reason to think are
//! held. Compounded, the two ceilings multiply — `max_scan_pages × (batch +
//! max_held_keys)` is sixteen thousand keys per sweep on the default config —
//! which is the "two mitigations each assuming it is the only one" failure
//! wearing the shape of the fix. Held keys sitting *later* in the walk still
//! land on later pages and still cost those pages a slot; that makes paging
//! less effective per page, not unbounded.
//!
//! ### What paging costs, per sweep, stated rather than implied
//!
//! - **Round trips.** One `scan_runnable` per page. A sweep whose first page
//!   fills the batch takes exactly one, which is what the overwhelmingly common
//!   sweep does — so the common case is unchanged from the single-call version.
//! - **Store-side walk.** `store/fs.rs` re-reads each *level's* directory
//!   entries per page and filters them against the cursor, so the directory
//!   listings are paid per page; the per-execution `runnable_at` read is paid
//!   once per execution across the whole pass, because the cursor does not
//!   re-visit what it has passed.
//! - **Runner-side reads.** One `store.load` per offered key that is not already
//!   held ([`SweepReport::entry_probes`]). This is the number that grows with
//!   `max_scan_pages`: the worst case moves from `batch` to
//!   `batch × max_scan_pages` — sixteen to sixty-four on the defaults — and it
//!   is paid **only** on a sweep that could not fill its batch, which is the
//!   condition paging exists for. A one-sweep caller with a narrow fleet pays it
//!   every time; see [`RunnerConfig::max_scan_pages`] for the two ways to remove
//!   it, neither of which is in this file. A store whose **offerable** set is no
//!   larger than `batch` pays none of it: that walk ends inside page one, which
//!   answers `resume: None`, and the sweep stops having made one call.
//!
//! It is preferred to the alternative because a scan that costs more is
//! recoverable and a live run that is never offered is not.
//!
//! ### The cursor is SWEEP-LOCAL and is never stored
//!
//! A [`ScanCursor`] is a position in one store's walk and is only meaningful to
//! the store that produced it. [`WorkerRunner::sweep`] takes `store` as an
//! argument, so the same runner can be handed a different store on the next
//! sweep — a field on `self` holding a cursor would be a position fed back to a
//! store that never produced it. It lives in a local, and every sweep starts at
//! `after: None`.
//!
//! ### A store that cannot page FAILS THE SWEEP, and that is deliberate
//!
//! [`LoopStateStore::scan_runnable`] has a default that refuses. Both real
//! stores implement it; a *decorator* that forwards `list_runnable` and forgets
//! this one inherits that refusal, and this runner treats it as
//! [`SweepStop::StoreUnavailable`] like any other scan that could not answer.
//! Falling back to the unpaged listing was considered and rejected: it would
//! reinstate exactly the starvation above, silently, on any store that grew a
//! wrapper — a mitigation presented as a fix, which is the failure this file has
//! already shipped twice. Failing on the first sweep is loud and is caught by
//! whoever added the wrapper.
//!
//! One consequence worth writing down: with this sweep on the paged entry point,
//! [`LoopStateStore::list_runnable`] has **no production caller left** in the
//! tree — every remaining call is a test or a decorator forwarding one. The
//! filter it is famous for has not become dead code, because both real stores
//! write `list_runnable` as a call into `scan_runnable` and the filter lives in
//! the latter. It is the unpaged *entry point* that is now unused, not the
//! judgement behind it.
//!
//! # A run that stopped mid-iteration is refused before it is claimed
//!
//! Five of the six phases read a value the **previous phase of the same
//! iteration** produced. The Resolve recovery path can journal back to a fresh
//! observation, and Prepare durably captures Epilogue's iteration baselines;
//! `state::in_memory_inputs_of` names the full list and
//! `state::ForeignPickup` is the decision it serves. A missing or misbound
//! Epilogue checkpoint remains a cold-entry refusal.
//!
//! `executor.rs`'s `run_phase` already refuses that entry, so this is not the
//! only thing between a cold host and a silently re-decided turn. What the check
//! here changes is **where the refusal lands**. `run_phase` refuses from inside
//! the claim: the lease is taken, the fence has moved, and the driver charges
//! the run an attempt against [`super::state::LoopState::phase_attempts`] — so a
//! handful of sweeps quarantines a healthy execution for a defect belonging to
//! the pickup rather than to the run. The cost of moving it out is one `load`
//! per offered key ([`SweepReport::entry_probes`]).
//!
//! ## The park is a TWO-step transition, and gating only the first step is
//! gating nothing
//!
//! The pre-claim check is asked only of an **unparked** run, because a parked
//! key's pickup is a park exit, which runs no phase; refusing it on the cursor
//! would withhold the one transition the wake index exists to produce.
//!
//! That carve-out on its own is worth nothing, and an earlier version of this
//! file shipped it that way. `driver_worker::leave_park` clears `wait`, commits,
//! and **does not move the cursor** — a parent that parked on children out of
//! `Apply` wakes with `wait: None` and a cursor still at `Apply` — and
//! [`Advanced::LeftPark`] is a [`Disposition::KeepGoing`], so the *same turn*
//! re-claimed and entered that phase. Straight into `run_phase`'s
//! `carry_missing`, from inside the claim, with the lease taken, the fence moved
//! and `commit_failed_attempt` charging `phase_attempts`: exactly the cost the
//! pre-claim check exists to avoid, reached by the one path it did not cover.
//!
//! So the question is re-asked **after** the park exit, before the turn's next
//! claim — see `WorkerRunner::after_park`. The exit itself still happens: the
//! wake is consumed and the durable transition lands. What does not happen is
//! this runner walking on into a phase whose inputs died with another holder.
//!
//! ## What the refusal does NOT distinguish, stated because it dominates the
//! count
//!
//! It reads the committed cursor and nothing else, and a cursor sitting inside
//! an iteration is **three** populations wearing one shape:
//!
//! - a run abandoned mid-iteration — the case the check is named for;
//! - a run being advanced right now by a live holder whose pin has lapsed (see
//!   the wiring hazard below), which is mid-iteration because it is healthy;
//! - a run that **STOPPED AT A RESUMABLE TERMINAL** — a pause, a confirmation.
//!   `driver_worker::next_cursor` answers `RecordedStep::RunEnded =>
//!   Ok(current)`, so *any* terminal leaves the cursor at the phase that ended
//!   the run — `Resolve` or `Apply` for a real one — and a resumable one
//!   publishes no ending marker, so the scan goes on offering the key.
//!
//! A run that ended **non-resumably** used to be a fourth, and was the majority
//! of the count. It is not offered at all now: the stores' ending marker
//! withholds it before this check is reached (see the ending-marker section
//! above). What that means for the counter is that
//! [`SweepReport::mid_iteration_refusals`] is *smaller* than it was and is still
//! not a census of stalled work — a paused run and a healthy pinned one both sit
//! in it, and telling any of the three apart needs the journal, an
//! `O(records)` read per offered key per sweep, which the runner does not pay.
//!
//! One consequence follows and is worth stating where a reader of the logs will
//! meet it: with the marker in place, **neither** [`HoldReason::AlreadyEnded`]
//! nor a mid-iteration refusal *of a finished run* is reachable through the
//! sweep path against either shipped store, and `HoldReason::RunEnded` is
//! reachable only within the sweep that ends the run. Read the reverse as a
//! signal rather than as noise: [`HoldReason::AlreadyEnded`] arriving from a
//! sweep means the key reached a claim that a store filter should have caught,
//! pointing to a decorated/regressed store or damaged operator state.
//!
//! # The runner is DRIVEN by a holder, it does not CONSTRUCT a host
//!
//! This is the shape the seam had to be, and the shape it was not until
//! 2026-08-27. It asked a `HostProvider` for a `Box<dyn WorkerHost>` — which is
//! `Box<dyn WorkerHost + 'static>`, while the only live host in the tree,
//! `executor.rs`'s `InProcessWorkerHost<'a>`, borrows another stack frame. **No
//! value of that type satisfies the bound under any lifetime**, so the seam was
//! unsatisfiable by construction rather than merely unimplemented, and no amount
//! of committed state would have changed it. Checked with a standalone `rustc`
//! on a five-line model of the two types before it was deleted, not reasoned
//! about.
//!
//! [`HostFleet`] is what replaced it: `host_for` hands back a **borrowed** host
//! — `&mut dyn WorkerHost`, the shape [`WorkerRunner::take_turn`] already needed
//! internally — so a caller that is already holding a live host can lend it for
//! one turn and take it back. The runner composes nothing and owns nothing.
//!
//! # Why the host is asked for BEFORE the claim
//!
//! [`HostFleet::host_for`] runs first and a key with no host is never claimed.
//! Two reasons, and the second is the one that matters:
//!
//! - A claim taken and immediately released is still a write, a fence increment
//!   and a window in which no other worker may take the run.
//! - A holder that has no host for a key then touches that key at all. A fleet
//!   may be narrow, so most offered keys can be refused; a refusal that costs a
//!   lease would then be paid on nearly every key of nearly every sweep.
//!
//! # The hazard to read before lending a host for a key this process did not start
//!
//! **A runner with a wide fleet could take a live in-flight run away from
//! `executor.rs`'s `StatelessArm` and fail it — CLOSED 2026-08-28 (v0.6.1292).**
//! The trace is kept rather than deleted, because what closed it is a change in
//! another file and a reader arriving here has to be able to tell that the fix
//! landed rather than that the hazard was forgotten:
//!
//! 1. `StatelessArm::seed_if_absent` commits every run it seeds as
//!    `Placement::Pinned` to `inproc-<pid>-<execution_id>`, until
//!    `now + `[`PIN_TTL_MS`](super::state::PIN_TTL_MS) — fifteen minutes.
//! 2. [`LoopState::for_commit`](super::state::LoopState::for_commit) used to
//!    **renew that pin only while the run held a user-typed ephemeral secret**.
//!    An ordinary run never holds one, so nothing renewed the pin it got at
//!    creation and it lapsed fifteen minutes in — while the run was still going.
//! 3. A lapsed pin is claimable by everybody:
//!    [`claimable_by`](super::state::LoopState::claimable_by) answers true and
//!    `list_runnable` offers the key to any worker.
//! 4. A runner that then claims it takes the lease. `StatelessArm`'s next
//!    `advance_once` gets [`Advanced::LeaseHeld`], which that arm turns into an
//!    `Err` — *"another runtime is advancing this execution"* — and the live run
//!    fails.
//!
//! Step 2 is the step that changed, and it is the one the whole chain rested on.
//! `for_commit` now renews the pin on **every** commit, so a live run is bound to
//! its driver for as long as it keeps committing and step 3 is never reached. The
//! fix named here as owed — *"a pin renewed on every commit or a lease-aware
//! `list_runnable`"* — was taken in its first form. See
//! `docs/archive/plans/2026-08-28-stateless-loop-open-questions-design.md` §1.
//!
//! **What survives it, stated no wider than it is true.** The pin is renewed by a
//! COMMIT, so the bound is the gap between one driver's commits and not the length
//! of its run: a phase that ran longer than [`PIN_TTL_MS`](super::state::PIN_TTL_MS)
//! without reaching a commit still lapses under a live holder, and step 3 follows
//! unchanged — `claimable_by` answers true and `list_runnable` offers the key.
//!
//! **Step 4 does NOT follow, and what replaces it is worse to diagnose.** That
//! residual window is mid-phase by construction: a phase past the TTL without a
//! commit means the holder is inside `advance_under_lease`, not between two
//! `advance_once` calls, so the holder is not sitting at a claim waiting to be told
//! [`Advanced::LeaseHeld`]. The second claimant runs the phase and commits. The
//! live holder then reaches its own commit and fails the store's compare-and-swap
//! against a revision that moved underneath it —
//! [`StoreError::Conflict`](super::store::StoreError::Conflict), *"somebody else
//! committed in between"* — while both workers' records have been appended to one
//! journal, which is exactly the damage `JournalError::SeqRewind` exists to name.
//! A refusal the caller can classify becomes a torn journal it cannot.
//!
//! That is a sizing question the constant's own doc argues, not a fleet question —
//! which is the point, because it means widening a fleet is no longer gated on it.
//!
//! So the **precondition on widening a fleet** that used to live here is
//! discharged. Production did not close restart recovery by teaching this
//! cursor scanner to mint `ActionExecutors`; doing that would make the store an
//! authority source it is not. The orchestrator instead enumerates canonical
//! runtime rows and reconstructs only an Artifact-bound root, an
//! integrity-sealed delegated child, or a durable PlanGraph root before
//! re-entering its exact execution id. Missing composition is failed closed.
//! This runner stays available to a scheduler that already has equally proven
//! hosts, while terminal outbox recovery uses the host-free lifecycle projector.
//!
//! ## The second wiring hazard: a re-claim of a PAUSED run is not cheap
//!
//! A pause is a **resumable terminal that commits no wait**, and a re-claim of
//! one costs a paid phase rather than a refusal. Traced rather than supposed:
//!
//! 1. `PhaseReport::ends_run` inherits `wait: None` from `continued()`, and
//!    `commit_boundary` writes `state.wait = report.wait`. Only a *park* sets a
//!    [`WaitReason`](super::state::WaitReason) — and that enum has exactly two
//!    variants, `Job` and `Children`, so a run that stopped for a human commits
//!    no wait at all and `list_runnable` has nothing to withhold it by.
//! 2. `advance_under_lease` lets a resumable terminal fall through on purpose,
//!    and with `state.wait` empty it skips `leave_park` and **re-runs the phase
//!    that ended** — which is the resume mechanism, and is also a fresh
//!    `Decide` (a paid LLM call) or `Apply`, a new journal record, a new
//!    revision and more `work_budget_consumed_ms`.
//! 3. Neither committed backstop bounds that. `commit_boundary` resets
//!    `phase_attempts` on every successful commit, and `RecordedStep::RunEnded`
//!    leaves the cursor where it is, so nothing walks toward the iteration
//!    ceiling. The only bound is a committed `deadline_at_ms`, which a run this
//!    process did not seed may not carry.
//!
//! A fixed re-check interval on that answer is therefore a *paid poll of a
//! human*: a run left waiting overnight would burn one phase per interval and
//! could exhaust its own work budget while nobody was at the keyboard. So the
//! classifier does not give every resumable terminal the same hold — see
//! [`Resumption`] for the split and for the one signal available to a runner
//! that costs nothing to be wrong about.
//!
//! # Fail closed
//!
//! *"Store unavailable → refuse to advance rather than run unrecorded work."*
//! Implemented at two grains, because the two failures are not one failure:
//!
//! - [`StoreError::Unavailable`] — the substrate cannot be reached. **The sweep
//!   stops.** Not "skip this key and try the next", which would run the whole
//!   batch against a store that has just said it can record nothing.
//! - Every other [`StoreError`] reaching a boundary refusal names a defect in
//!   **one** run's records — a watermark past its own log, an unparseable effect
//!   id — and is held against that key alone. Calling those an outage would
//!   stall every other execution on one bad file.
//!
//! ## What that rule does NOT cover, stated because an unstated gap reads as
//! coverage
//!
//! It covers every error a store *hands to this runner*. It cannot cover an
//! error a store swallows on the way. `store/fs.rs::scan_runnable` maps a
//! per-execution read failure to a `warn!` and carries on, so a tree whose
//! execution records are all unreadable — a permissions change, a half-mounted
//! volume, a store root pointed at the wrong place — answers a page of no keys.
//! Only a failure reading the scope, principal, workspace or executions
//! *directories* propagates.
//!
//! **Paging neither closes that nor widens it**, and the reason is worth being
//! exact about rather than assuming either way. A page fills at `limit` OFFERED
//! keys, so a walk that offers nothing runs to the end of the tree inside one
//! page and answers `resume: None` — [`ScanEnd::WalkedTheWholeStore`], one scan,
//! exactly what the unpaged call did. Only a tree large enough to trip the
//! store's own `MAX_EXECUTIONS_SCANNED` names a resume point, and then
//! [`RunnerConfig::max_scan_pages`] bounds how far this sweep follows it.
//!
//! A page of **zero keys with a `resume`** is therefore a real and expected
//! shape, not a contradiction: it means the walk stopped short having offered
//! nothing. The sweep pages on through it, which is the whole point — an empty
//! page is the strongest possible evidence that the batch is not going to be
//! filled from where it is looking.
//!
//! From here that is indistinguishable from an empty queue: `offered = 0`,
//! `stopped = None`, [`SweepReport::did_work`] false, the idle backoff, and
//! neither [`SweepStop::StoreUnavailable`] nor
//! [`RunnerConfig::max_consecutive_store_failures`] ever fires. The swallow is
//! deliberate where it lives — one unreadable execution must not hide every
//! other one — and the missing half is a *count* of skipped executions on the
//! listing, which is a change in `store/`. Until that exists, a deployment
//! cannot tell "there is nothing to do" from "the substrate is broken" by
//! reading this runner's reports alone.
//!
//! # Bounded
//!
//! Four ceilings, each preventing a different starvation:
//!
//! - [`RunnerConfig::batch`] — how many keys one page of the scan asks for, and
//!   how many keys the sweep has to be able to work before it stops paging.
//! - [`RunnerConfig::max_scan_pages`] — how deep into the walk one sweep will
//!   go looking for that batch. The one ceiling here whose firing is itself a
//!   coverage hole, which is why [`SweepReport::scan_end`] names it.
//! - [`RunnerConfig::max_claims_per_execution`] — a long run yields to the rest
//!   of the batch instead of holding the sweep for its whole life.
//! - [`RunnerConfig::max_claims_per_sweep`] — the sweep returns to its caller,
//!   which is what makes a short-lived worker possible at all.
//!
//! The unit is **claims**, not committed phases, and the difference is not
//! cosmetic: [`Advanced::LeftPark`] and [`Advanced::Committed`] both continue the
//! loop and only one of them ran a phase, so a budget counted in phases would be
//! unbounded on the other.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[cfg(test)]
use super::driver_worker::TerminalSteerPolicy;
use super::driver_worker::{advance_once, Advanced, Refusal, WorkerConfig, WorkerHost};
use super::journal::TerminalKind;
use super::reconciler::{LoopReconciler, ReconcilerPolicy};
use super::state::{LoopStateRefusal, WorkerId};
use super::store::{ExecutionKey, LoopStateStore, Revision, RunnableScan, ScanCursor, StoreError};
use crate::magician_v2::execution::agentic::types::AgenticOutcome;

// ============================================================================
// The seam a holder supplies
// ============================================================================

/// The hosts a caller is willing to LEND this runner, keyed by the run each one
/// is already set up for.
///
/// # A lent host, not a composed one, and the difference is a type fact
///
/// This trait replaced a `HostProvider` whose ready answer was
/// `Box<dyn WorkerHost>`. A bare `Box<dyn Trait>` is `Box<dyn Trait + 'static>`,
/// and the only live implementation of [`WorkerHost`] in the tree —
/// `executor.rs`'s `InProcessWorkerHost<'a>` — borrows another stack frame. **No
/// value of that type satisfies the bound under any lifetime.** The seam could
/// therefore never have carried a live host, whatever else was built, and the
/// deployments reading its `NotComposable` refusal were reading a type error
/// dressed as a policy. Established with a standalone `rustc` on a model of the
/// two types before the seam was deleted, rather than reasoned about.
///
/// So `host_for` hands back `&mut dyn WorkerHost`: the shape
/// [`WorkerRunner::take_turn`] already used internally and did not expose. The
/// borrow lasts exactly one turn, which is also how long a host is safe to lend
/// — [`advance_once`] takes and gives back the lease inside one call, so the
/// fleet is whole again at every claim boundary.
///
/// # What an implementor owes, and it is not "compose whatever is asked for"
///
/// A fleet answers for the runs **its holder has already set up**, and it should
/// answer [`HostForKey::NotComposable`] for everything else rather than build
/// one. A host assembled without `executor.rs`'s `build_run_setup` runs phases
/// under a context with no `owner_definition`, no resolved tool scope and no
/// policy fingerprint — an authority WIDENING wearing the shape of a pickup,
/// which is worse than refusing.
///
/// # The contract on the two refusals, because they are held for different times
///
/// - [`HostForKey::NotComposable`] means *not while this fleet lives*: this
///   holder does not have that run set up and asking again will not change it.
///   The runner holds the key for its own lifetime.
/// - [`HostForKey::Unavailable`] means *not right now*: a dependency was
///   momentarily unreachable, or a host this fleet does hold is busy. The runner
///   retries after [`RunnerConfig::host_unavailable_holdoff`].
///
/// An implementation answering `NotComposable` for a transient failure strands a
/// run until the worker restarts; one answering `Unavailable` for a structural
/// failure re-asks forever. Neither is detectable from here, which is why the
/// distinction is an obligation on the implementor rather than something
/// enforced.
pub trait HostFleet: Send {
    /// The host this fleet will lend for `key`, or why it will not.
    fn host_for(&mut self, key: &ExecutionKey) -> HostForKey<'_>;

    /// A run this fleet lent a host for has ENDED, and this is its outcome.
    ///
    /// # Why the runner hands this back instead of dropping it
    ///
    /// [`Advanced::RunEnded`] carries a `Box<AgenticOutcome>` — the run's actual
    /// answer — and every other caller of [`advance_once`] in the tree returns it
    /// to somebody. A runner that dropped it would make a picked-up run's
    /// completion unobservable to the one process that could deliver it, and that
    /// process's next claim would get [`Advanced::RunAlreadyEnded`], which
    /// `executor.rs`'s `StatelessArm` turns into a hard error. So the outcome
    /// travels to the fleet, which is the only participant that knows who is owed
    /// it.
    ///
    /// Called **after** the borrow [`Self::host_for`] handed out has ended, so an
    /// implementation may take `&mut self` freely.
    ///
    /// The default drops the outcome, which is right for a fleet nobody is
    /// waiting on — a discovery sweep, a test fixture — and wrong for one that
    /// lent a live run's host. Overriding it is how a holder collects its answer.
    fn run_ended(
        &mut self,
        key: &ExecutionKey,
        outcome: Box<AgenticOutcome>,
        terminal: TerminalKind,
    ) {
        let _ = (key, outcome, terminal);
    }
}

/// What a [`HostFleet`] answered.
///
/// The lifetime is written out rather than elided because it is the point of the
/// type: `Ready` carries a reference INTO the fleet, so the fleet cannot be
/// touched again until the turn that borrowed it is over.
pub enum HostForKey<'h> {
    /// A host, lent for one turn. The runner claims and advances.
    Ready(&'h mut dyn WorkerHost),
    /// This fleet does not hold that run, and will not.
    NotComposable { detail: String },
    /// A transient failure. Try again later.
    Unavailable { detail: String },
}

// ============================================================================
// Configuration
// ============================================================================

/// What a runner needs that neither the store nor the execution knows.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// How many keys one page of the scan asks for — and the number of keys a
    /// sweep must be able to **take a turn on** before it stops paging.
    ///
    /// Two jobs, and they are the same number on purpose: the batch a sweep
    /// wants and the width of one look at the store are the same quantity seen
    /// from the two ends. The first page asks for this plus the holdoff set (see
    /// the module docs); every later page asks for exactly this.
    ///
    /// The stop condition is [`SweepReport::attempted`], not
    /// [`SweepReport::offered`]. A page made entirely of keys this fleet cannot
    /// host, or of keys refused before the claim, has offered `batch` keys and
    /// produced no work — counting those would make the paging loop stop on
    /// precisely the input it exists for.
    pub batch: usize,
    /// Pages of the runnable walk one sweep may ask for.
    ///
    /// # Why there is a ceiling at all
    ///
    /// A loop that pages until it fills its batch walks the **whole store** when
    /// every key is refused — which is the ordinary state of a narrow fleet, not
    /// an exotic one. That is a directory walk and one `store.load` per offered
    /// key, on a live run's critical path.
    ///
    /// # What it costs when it fires, which is the part worth reading
    ///
    /// A sweep that gives up early is better than one that walks a million
    /// directories, but only if it says so. When this ceiling stops the paging
    /// with the batch unmet and the walk unfinished, the sweep reports
    /// [`ScanEnd::PageCap`] and logs it: **everything behind the last page was
    /// not looked at by this sweep**, and if the same prefix refuses every sweep
    /// then the same tail is unreachable every sweep. That is a coverage hole
    /// rather than merely less work taken, and it is the same shape of hole
    /// `ParkedListing::incomplete` names on the reconciler's side.
    ///
    /// The recovery is a *later* sweep: a key refused before the claim is put in
    /// the holdoff set, so the next sweep's first page is widened past it and
    /// starts further in. A one-sweep caller has no later sweep and gets no such
    /// recovery, which is why the flag is on the report rather than only in a
    /// log.
    ///
    /// # The bill, on the defaults
    ///
    /// Up to `batch × max_scan_pages` keys offered — sixty-four — against
    /// sixteen for the single unpaged call this replaced, and one `store.load`
    /// for each of them that is not already held. Paid only by a sweep that
    /// could not fill its batch from earlier pages.
    ///
    /// **And a store whose OFFERABLE set is no larger than `batch` pays none of
    /// it**, which is most deployments. Such a walk runs to its end inside the
    /// first page and answers `resume: None`, so the loop stops on
    /// [`ScanEnd::WalkedTheWholeStore`] having made exactly one call — the same
    /// bill, key for key, as the unpaged version. The multiplier needs *both* a
    /// store with more offerable keys than `batch` and a sweep that cannot use
    /// the ones it is shown.
    ///
    /// A short-lived caller with a narrow fleet pays that cost on every fresh
    /// runner. Three things remove or constrain it; the first is local to this
    /// file:
    ///
    /// - **Ask [`HostFleet::host_for`] BEFORE the pre-claim `load`.** That is a
    ///   reorder inside [`WorkerRunner::sweep`], four lines, and it makes an
    ///   unhostable key free rather than a read — the whole amplification, gone,
    ///   for the narrow fleet that is the only shape in the tree. It is not done
    ///   here because it moves what two counters mean:
    ///   [`SweepReport::mid_iteration_refusals`] would stop counting keys this
    ///   fleet could never host (arguably better, certainly different) and
    ///   [`SweepReport::hosts_not_composable`] would start counting keys the
    ///   cursor check subtracts today, which its own docs currently promise it
    ///   does not. Anyone taking it owns both docs and the fixtures that assert
    ///   those two counters.
    /// - **A fleet that can name the keys it holds hosts for**, so the runner
    ///   asks the store for those directly instead of scanning at all. That one
    ///   really is not in this file.
    /// - **Set this to `1` at the call site** for exactly the old bill — with
    ///   the standing coverage flag the next section describes.
    ///
    /// ## …and `1` STANDS THE COVERAGE FLAG UP PERMANENTLY, which is the price
    ///
    /// Recorded here rather than left for whoever sets it. With `max_scan_pages:
    /// 1`, against any store holding more **offerable** keys than
    /// [`Self::batch`] and a fleet that cannot fill the batch — which is a
    /// narrow fleet's ordinary state, not an exotic one — the classifier reaches
    /// `scan_pages (1) >= max_pages (1)` with the batch unmet on **every**
    /// sweep. So [`ScanEnd::PageCap`] is the standing answer, the `warn!` beside
    /// it fires every sweep, and [`RunReport::scans_incomplete`] is up every
    /// time. A flag that is always up is a flag an operator learns to ignore.
    ///
    /// That is not the flag lying: with one page the tail genuinely is
    /// unexamined, exactly as it was under the unpaged call this replaced — the
    /// difference is that it is now *said*. But a caller choosing `1` is
    /// choosing a permanent warning, and should either accept it deliberately or
    /// take one of the two fixes above instead.
    ///
    /// **Zero is read as one.** At least one page is always taken: a runner that
    /// looks at nothing is not a runner, and this field is a ceiling on the
    /// *extra* looks rather than a switch for the first one. Zero therefore
    /// inherits the paragraph above rather than escaping it.
    pub max_scan_pages: usize,
    /// Claims one execution may take before the runner moves to the next key.
    ///
    /// # GAP: this yields at a claim count, not at an iteration boundary
    ///
    /// A turn cut here leaves the committed cursor **inside** an iteration, and
    /// the next sweep asks [`HostFleet::host_for`] again — a fleet is free to
    /// hand back the same host or a fresh one, and the runner cannot tell — so
    /// the values the yielded iteration had produced may be gone with the host
    /// that produced them. The cursor check above then refuses the key for this
    /// worker's life.
    ///
    /// The refusal is not the defect; it is the defect becoming visible. Without
    /// it the next sweep would claim, enter the phase, and be refused from
    /// inside `run_phase` — charging an attempt against the run's quarantine
    /// counter every sweep until it was quarantined.
    ///
    /// So this ceiling and the iteration boundary want to be the same line, and
    /// today they are not. Two ways out: a fleet that keeps the same host across
    /// turns — which [`HostFleet`] makes expressible, since the host is lent
    /// rather than composed — or
    /// a yield that only takes effect at `Phase::next_in_iteration() == None`,
    /// which is not in this file. Until the second lands, a value low enough to
    /// cut an average iteration in half strands whatever it cuts on any fleet
    /// that does not keep its hosts — [`SweepReport::mid_iteration_refusals`] is
    /// where that shows up.
    pub max_claims_per_execution: usize,
    /// Claims one sweep may take in total.
    pub max_claims_per_sweep: usize,
    /// How long a key whose phase failed is held off.
    ///
    /// The attempt counter is committed, so repeated failures reach
    /// `Quarantine::PhaseAttemptsExhausted` on their own. This only stops one
    /// sweep burning every remaining attempt.
    pub failure_holdoff: Duration,
    /// How long a key is held off after an answer somebody else is about to
    /// change — a lease taken between the scan and the claim, a stale commit, a
    /// park that resolved and re-armed.
    pub transient_holdoff: Duration,
    /// How long a key is held off when its host was momentarily unavailable.
    pub host_unavailable_holdoff: Duration,
    /// How long a key whose committed cursor sits **inside** an iteration is
    /// held off.
    ///
    /// # Timed, and the permanent hold it replaced was a claim about the future
    ///
    /// The first cut held this key for the worker's lifetime, arguing that "no
    /// further claim changes the answer". That is false, and it is false in the
    /// same way [`Advanced::NothingToAdvance`]'s answer would be: the cursor is
    /// a *snapshot*, not a structural fact. A run offered mid-iteration may be
    /// mid-iteration because it is **healthy** — a live `StatelessArm` run whose
    /// pin has lapsed is exactly that, see the wiring hazard in the module docs
    /// — and when its holder later stops at an iteration boundary the run
    /// genuinely needs the foreign pickup a permanent hold had already refused
    /// forever, silently, with `expire_holdoffs` never dropping the entry.
    ///
    /// Deliberately long, because the population it holds is dominated by runs
    /// that ENDED (a terminal leaves the cursor where it was — again, the module
    /// docs). Re-checking those is pure read volume, and the check costs one
    /// `load` per interval with no claim, no lease and no attempt charged, so
    /// the interval buys correctness at a price paid in reads rather than in
    /// anything a run can feel.
    pub mid_iteration_holdoff: Duration,
    /// How long a run that ended at a resumable terminal **the store can
    /// witness** is held off.
    ///
    /// Applies to exactly the terminals in
    /// [`Resumption::WhenTheStoreOffersItAgain`]. For those the store is the
    /// thing that knows: a park commits a [`WaitReason`](super::state::WaitReason)
    /// and `list_runnable` withholds the key until a resolution lands, so this
    /// interval bounds only how *late* a resolved wake is picked up, not how
    /// often a run is disturbed.
    ///
    /// What it must not be is `UntilThisWorkerRestarts`: a park whose wake
    /// resolves would then be offered by the store and refused by this runner
    /// forever, which defeats the wake index the store built for exactly that
    /// transition.
    ///
    /// It is emphatically **not** the interval for a run waiting on a human —
    /// see [`Resumption`], and the second wiring hazard in the module docs for
    /// what a re-check costs there.
    pub resumable_terminal_holdoff: Duration,
    /// Keys the holdoff set may remember before it evicts.
    ///
    /// Bounded because the set is process-local memory in a worker whose whole
    /// point is that it can be cold-started. Eviction is not free — see
    /// [`SweepReport::holdoff_evictions`].
    pub max_held_keys: usize,
    /// How long [`WorkerRunner::run`] waits after a sweep that advanced nothing.
    pub idle_backoff: Duration,
    /// How long [`WorkerRunner::run`] waits after a sweep the store stopped.
    pub store_backoff: Duration,
    /// Consecutive store-unavailable sweeps before the runner gives up.
    ///
    /// Giving up is the honest end for a worker: an operator restarts it when
    /// the substrate is back, and a process spinning against a dead store
    /// forever is indistinguishable from one that is working.
    pub max_consecutive_store_failures: usize,
    /// How often [`WorkerRunner::run`] runs a [`LoopReconciler`] pass, if ever.
    ///
    /// # Why the reconciler's home is here and not a task of its own
    ///
    /// `reconciler.rs`'s own *What is not wired up* names this loop as the
    /// scheduler a pass belongs on, for one reason that is not convenience: a
    /// reconciler and a runner disagreeing about a key is the expensive failure
    /// — a retirement takes a lease on a run a sweep may be about to claim — and
    /// two processes cannot be made to interleave, while two calls in one loop
    /// are ordered by construction. A pass runs BETWEEN sweeps, never inside
    /// one, so no key is ever held by both at once **from this process**.
    ///
    /// # The cadence, and what it is measured against
    ///
    /// Deliberately much longer than a sweep and comfortably longer than
    /// [`super::reconciler::DEFAULT_PARK_GRACE`], so a park is diagnosed once or
    /// twice before an operator sees it rather than on every sweep. A pass is
    /// `O(executions)` in directory reads — see the reconciler's cost table —
    /// which is why it is not simply run every time.
    ///
    /// `None` disables it. That is the right answer for a short-lived pass whose
    /// whole life is shorter than one cadence, and it is why this is an `Option`
    /// rather than a very large `Duration`: *never* and *not yet* are different
    /// claims and a caller should be able to say which it means.
    pub reconcile_every: Option<Duration>,
    /// What a reconciler pass is allowed to cost and whether it may act.
    ///
    /// [`ReconcilerPolicy::default`] carries [`super::reconciler::Recovery::ReportOnly`],
    /// so leaving this alone gives a **read-only** pass. A caller that wants
    /// proved parks retired has to name two things — the worker the lease is
    /// taken under, and the `RetirableGround`s it may act on — which together
    /// are the whole shape of `Recovery::RetireProvedParks`. Neither has a
    /// default, and for the same reason: an id would let a lease-taking write be
    /// enabled by copying a struct literal nobody read, and a ground set would
    /// let the unsound half of a pair be inherited without being read either.
    pub reconciler: ReconcilerPolicy,
    /// What [`advance_once`] is told about leases and phase attempts.
    pub worker: WorkerConfig,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            batch: 16,
            // Four rather than one, and rather than "until the walk ends".
            //
            // One is the old behaviour with a new name and fixes nothing. No
            // ceiling at all makes a sweep against a narrow fleet an O(store)
            // walk with an O(store) read behind it. Four bounds the worst case
            // at sixty-four keys offered and sixty-four `load`s — a four-fold
            // worst case on a path documented for sixteen — while letting a
            // sweep see four `batch`-deep windows past a prefix it has never
            // met. It is a starting point chosen against the one live caller's
            // bill, not a measured optimum, and `SweepReport::scan_end` is what
            // says whether it is enough in a given deployment.
            max_scan_pages: 4,
            max_claims_per_execution: 32,
            max_claims_per_sweep: 128,
            failure_holdoff: Duration::from_secs(30),
            transient_holdoff: Duration::from_secs(5),
            host_unavailable_holdoff: Duration::from_secs(30),
            mid_iteration_holdoff: Duration::from_secs(300),
            resumable_terminal_holdoff: Duration::from_secs(60),
            max_held_keys: 4_096,
            // Thirty minutes against a fifteen-minute park grace: a park is
            // examined on its first pass after the grace and roughly twice an
            // hour after that, which is the "once or twice before an operator
            // sees it" the reconciler's docs ask for. Long enough that the
            // `O(executions)` walk is not a per-sweep cost.
            reconcile_every: Some(Duration::from_secs(30 * 60)),
            reconciler: ReconcilerPolicy::default(),
            idle_backoff: Duration::from_secs(2),
            store_backoff: Duration::from_secs(15),
            max_consecutive_store_failures: 5,
            worker: WorkerConfig::default(),
        }
    }
}

// ============================================================================
// What one key's turn produced
// ============================================================================

/// How long a key is withheld from this runner.
///
/// Named variants rather than an `Option<i64>` whose `None` would have to mean
/// *forever*. The direction of an absent value is exactly the confusion this
/// subsystem has already paid for once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Until this wall-clock instant.
    Until(i64),
    /// For as long as this worker runs. Nothing a further claim could do would
    /// change the answer; only an operator, another process, or a restart will.
    UntilThisWorkerRestarts,
    /// Until the run's committed state is not the revision it ended at.
    ///
    /// The one hold that is neither a time nor forever, and the only honest
    /// answer for a run that stopped for a human. Re-claiming such a run
    /// **re-executes the phase that paused** — see the second wiring hazard in
    /// the module docs — so a re-check interval is a paid poll, and a permanent
    /// hold would strand a run this worker could have picked up the moment it
    /// was answered.
    ///
    /// A revision is the cheapest thing that cannot be wrong in the dangerous
    /// direction: a resume has to commit before the run makes any progress, so
    /// an unchanged revision means the phase would re-run and produce exactly
    /// what it produced last time. Testing it costs one `load` — no lease, no
    /// fence, no write — against a phase call that costs an LLM.
    UntilCommittedStateChanges { revision: Revision },
}

/// How a resumable terminal gets picked up again, which is not one answer.
///
/// A pause, a confirmation, a park and a sleep all end the *invocation* and
/// leave the execution alive. They do not agree about **who says it is over**,
/// and reading them as one thing is what put a paid `Decide` on a sixty-second
/// timer behind a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resumption {
    /// The store witnesses it. The run committed a
    /// [`WaitReason`](super::state::WaitReason), so `list_runnable` withholds
    /// the key until a wake resolution lands and the key being offered at all
    /// is itself the signal. A timed hold here only bounds pickup latency.
    ///
    /// [`TerminalKind::Sleeping`] commits its wake time to
    /// `LoopState::runnable_at_ms` in the same fenced transaction as RunEnded,
    /// so the store withholds it until the exact deadline. The bounded hold is
    /// only the live runner's handoff latency after that durable gate opens.
    WhenTheStoreOffersItAgain,
    /// Only a person answers it, and nothing in the committed state says when
    /// they did. Held until the committed revision moves; see
    /// [`Hold::UntilCommittedStateChanges`].
    WhenTheCommittedStateChanges,
}

/// Which of the two a terminal is.
///
/// Exhaustive over [`TerminalKind`] rather than over `is_resumable`, so a
/// terminal added later has to be placed rather than inheriting whichever answer
/// sits at the bottom. The non-resumable half is named too, and answers the
/// question that is not asked of it, because "this terminal is not resumable" is
/// a claim worth failing the build over if it stops being true.
const fn resumption_of(terminal: TerminalKind) -> Resumption {
    match terminal {
        TerminalKind::WaitingForChildren | TerminalKind::Sleeping => {
            Resumption::WhenTheStoreOffersItAgain
        },
        TerminalKind::WaitingForUser
        | TerminalKind::WaitingForConfirmation
        | TerminalKind::PausedByUser => Resumption::WhenTheCommittedStateChanges,
        // Not resumable at all. `classify` reaches this function only behind
        // `terminal.is_resumable()`, so these are unreachable there — but a
        // terminal that stopped being permanent would arrive here silently, and
        // the safe answer for an unknown one is the one that costs nothing.
        TerminalKind::Success
        | TerminalKind::Failed
        | TerminalKind::MaxIterationsReached
        | TerminalKind::LoopDetected
        | TerminalKind::BudgetExhausted
        | TerminalKind::CannotProceed
        | TerminalKind::HandedOff => Resumption::WhenTheCommittedStateChanges,
    }
}

/// Why a key stopped being worked. One variant per non-advancing answer, so a
/// report is legible without re-deriving it from an [`Advanced`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// A phase ended the run on this claim, for good.
    RunEnded,
    /// A phase parked or slept the run. The execution is alive and the store
    /// is what knows when it may go again, so the hold is timed and never
    /// permanent. See [`Resumption::WhenTheStoreOffersItAgain`].
    RunSuspended,
    /// A phase ended the run waiting for a **person** — a pause, a
    /// confirmation. The execution is alive, nothing committed says when the
    /// answer arrives, and a re-claim would re-execute the phase that paused,
    /// so the hold lasts until the committed state moves. See
    /// [`Hold::UntilCommittedStateChanges`].
    AwaitingAnOutsideAnswer,
    /// The run's journal already recorded a terminal it cannot come back from.
    AlreadyEnded,
    /// The committed cursor is past the run's iteration ceiling. Only the
    /// executor's max-iterations ceremony can conclude it.
    IterationCeiling,
    /// Held for an operator. Nothing was deleted.
    Quarantined,
    /// An effect with no result and no licence to fire.
    EffectIndeterminate,
    /// A re-derived dispatch did not reproduce what the run committed.
    ExecutionFailed,
    /// The phase returned an error; the attempt is committed.
    PhaseFailed,
    /// Another worker holds the lease.
    LeaseHeld,
    /// Pinned to a worker that is not this one.
    PinnedElsewhere,
    /// Parked with nothing outstanding to resolve it.
    Parked,
    /// A committed retry delay has not elapsed.
    NotYetRunnable,
    /// Nothing is committed under the key the scan offered.
    NothingToAdvance,
    /// Somebody else committed, or holds, this run.
    StaleCommit,
    /// The commit projection refused.
    ProjectionRefused,
    /// A park on children with no live child.
    ParkWithNoLiveChild,
    /// A phase claimed a boundary exit from the epilogue.
    ExitFromTheEpilogue,
    /// A store error naming a defect in one run's records rather than an outage.
    StoreRecordRefused,
    /// The committed cursor sits **inside** an iteration, and the values that
    /// phase reads were produced in the memory of a holder this runner is not.
    ///
    /// **Timed** ([`RunnerConfig::mid_iteration_holdoff`]), not permanent. The
    /// first cut held it for the worker's lifetime on the argument that "no
    /// further claim changes the answer" — which is a claim about the future
    /// that the observation cannot support. A cursor left mid-iteration moves
    /// whenever *some* holder advances it, and another holder advancing it is
    /// the ordinary case, not the exotic one: the module's own wiring hazard
    /// describes a LIVE run offered by `list_runnable` because its pin lapsed,
    /// and that run is mid-iteration precisely because it is healthy. Held
    /// permanently, it would be refused by this worker for the rest of the
    /// process's life — including at the moment it finally stopped at an
    /// iteration boundary and needed exactly this pickup. It is the identical
    /// argument [`Self::NothingToAdvance`] is already timed for.
    ///
    /// Not a store defect and not a host defect, which is why it is neither
    /// [`Self::StoreRecordRefused`] nor [`Self::HostNotComposable`]: the records
    /// are intact and a perfectly composable host would still be wrong to enter
    /// here. See `state::ForeignPickup`.
    ///
    /// It is also **not** a statement that the run is stalled. A terminal leaves
    /// the cursor where it was, so a finished run reaches this refusal rather
    /// than [`Self::AlreadyEnded`] — see [`SweepReport::mid_iteration_refusals`].
    MidIterationPickup,
    /// No host could be composed for this run, and none will be.
    HostNotComposable,
    /// The host was momentarily unavailable.
    HostUnavailable,
}

/// What the runner does about one answer from [`advance_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Claim this key again.
    KeepGoing,
    /// Stop working this key and hold it off.
    Release { hold: Hold, reason: HoldReason },
    /// The substrate could not answer. Stop the sweep, and record nothing about
    /// the key — nothing was learned about it.
    FailClosed {
        during: &'static str,
        detail: String,
    },
}

/// What a park exit left behind, and whether the turn may go on.
///
/// A park exit is a durable transition that runs **no phase** and moves **no
/// cursor**: `driver_worker::leave_park` clears `wait`, commits, and returns.
/// So the cursor the exit leaves is the cursor the run parked at, and for a
/// parent that parked out of `Apply` that is a phase whose inputs died with the
/// holder that produced them. Continuing the turn there is the exact entry the
/// pre-claim check in [`WorkerRunner::sweep`] exists to prevent, reached by the
/// one path that check deliberately carves out.
///
/// Not folded into `classify`, which is pure and has no store: answering this
/// costs a `load`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AfterPark {
    /// The cursor the exit left is one this runner may enter.
    Enter,
    /// It is not. Hold the key and end the turn.
    Refuse { hold: Hold, reason: HoldReason },
    /// The substrate could not answer the re-read. Same rule as everywhere else
    /// in this file: the sweep stops rather than claiming against a store that
    /// has just said it can record nothing.
    StoreUnavailable {
        during: &'static str,
        detail: String,
    },
}

/// Why one key's turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndOfTurn {
    /// Held off; see the reason.
    Held { hold: Hold, reason: HoldReason },
    /// The key hit [`RunnerConfig::max_claims_per_execution`]. Still live work:
    /// no holdoff, and the next sweep resumes it.
    YieldedOnClaimBudget,
    /// The sweep hit [`RunnerConfig::max_claims_per_sweep`]. Also live work.
    YieldedOnSweepBudget,
    /// The caller cancelled mid-turn.
    Cancelled,
    /// The sweep was stopped by the store while this key was being worked.
    StoreStopped,
}

/// A run that reached a terminal during a turn, on its way back to the fleet.
///
/// Deliberately not a field on [`KeyOutcome`]: that type derives `PartialEq, Eq`
/// and is compared whole in a dozen tests, and `AgenticOutcome` derives neither.
/// A carrier the runner moves through rather than a report field it stores.
pub struct EndedRun {
    /// The run's answer. Boxed for the same reason
    /// [`Advanced::RunEnded`] boxes it.
    pub outcome: Box<AgenticOutcome>,
    /// Which terminal it was, as the journal recorded it — the half a caller can
    /// act on without matching twenty outcome variants.
    pub terminal: TerminalKind,
}

/// What one key's turn did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOutcome {
    pub key: ExecutionKey,
    /// Calls to [`advance_once`].
    pub claims: usize,
    /// Of those, the ones that committed a phase.
    pub phases_committed: usize,
    pub end: EndOfTurn,
}

/// Why a sweep stopped before it ran out of keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepStop {
    /// Fail closed. The substrate could not answer.
    StoreUnavailable {
        during: &'static str,
        detail: String,
    },
    /// [`RunnerConfig::max_claims_per_sweep`].
    ///
    /// Set at three places and they are one condition: before the next key's
    /// turn, before a further claim of the key already being worked, and — when
    /// the budget was spent on the last key of a page — before the sweep would
    /// have asked the store for a page it could not use. The third is what stops
    /// a spent budget costing one extra `scan_runnable`, and it is deliberately
    /// **not** set when the sweep also filled its batch or also finished the
    /// walk, because [`ScanEnd::BatchFilled`] and
    /// [`ScanEnd::WalkedTheWholeStore`] are stronger statements about coverage
    /// than the [`ScanEnd::SweepEnded`] this produces.
    ClaimBudget,
    /// The caller cancelled.
    Cancelled,
}

/// Why a sweep stopped asking the store for more of the runnable walk.
///
/// Four answers rather than a `bool`, because the two that look alike from a
/// counter — a sweep that stopped because it had what it wanted and a sweep that
/// stopped because it was not allowed to look further — call for opposite
/// actions, and the second is the only one that leaves store nobody examined.
///
/// `None` on [`SweepReport::scan_end`] means **some page** of the scan could not
/// be answered — the first one or a later one — so nothing is known about the
/// walk *from that page onward*, not even whether there is any more of it.
///
/// It is deliberately NOT read as "nothing was scanned", and an earlier version
/// of this paragraph said it was. The failure is classified first, ahead of
/// [`SweepReport::stopped`], because no [`ScanEnd`] variant is an honest name
/// for a walk that refused to be read. The pages *before* the failing one still
/// happened: their keys are in [`SweepReport::offered`], their turns are in
/// [`SweepReport::outcomes`], and a report reading `scan_end: None, scan_pages:
/// 3, offered: 32` is describing a sweep that worked two pages and was refused
/// the third. Read `scan_pages` and `offered` for what was covered;
/// `scan_end: None` says only that the tail is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEnd {
    /// The last page's [`RunnableScan::resume`] was `None`: the walk reached the
    /// end of the store. Nothing is behind this sweep.
    WalkedTheWholeStore,
    /// The sweep had [`RunnerConfig::batch`] keys it could take a turn on and
    /// stopped asking. The walk had more, and the next sweep starts again from
    /// the beginning — which is correct, because the head of the walk is where
    /// the newest committed work sorts as often as not.
    BatchFilled,
    /// The sweep itself ended while the page was being worked — a claim budget,
    /// a cancellation, a store refusal that stopped it closed — so no further
    /// page was asked for.
    ///
    /// It says nothing about whether the walk had more: the sweep stopped for a
    /// reason of its own before that question was reached. [`SweepReport::stopped`]
    /// is the field that says which reason.
    SweepEnded,
    /// [`RunnerConfig::max_scan_pages`] fired with the batch unmet **and** the
    /// walk unfinished.
    ///
    /// The one variant that is a coverage hole rather than a stopping point:
    /// everything past the last page was not looked at, and if the prefix
    /// refuses every sweep the same way, it is not looked at by any sweep
    /// either. A later sweep recovers by widening its first page past the keys
    /// this one put in the holdoff set; a caller that takes one sweep and exits
    /// has no later sweep and no recovery.
    PageCap,
}

/// What one sweep did. The runner's whole observable surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// The `limit` the **first** page of this sweep's scan asked for.
    ///
    /// `batch` plus the size of the holdoff set — see the module docs. Reported
    /// rather than assumed to be `batch`, because a limit that has grown is the
    /// visible symptom of a store offering work that no claim can change, and
    /// the number an operator needs when a scan starts costing more.
    ///
    /// Later pages ask for `batch` and are deliberately **not** widened, so this
    /// is not `offered / scan_pages` and must not be read as an average.
    pub scan_limit: usize,
    /// `scan_runnable` calls this sweep made, including a failed one.
    ///
    /// One is the common answer and the one the design is tuned for. More than
    /// one means the earlier pages did not yield [`RunnerConfig::batch`] keys
    /// this runner could work, which is either a narrow fleet or the starvation
    /// paging exists for; [`Self::scan_end`] is what tells them apart.
    pub scan_pages: usize,
    /// Why the sweep stopped paging, or `None` if a page of the scan — the
    /// first or a later one — could not be answered. See [`ScanEnd`]: `None`
    /// says the tail is unknown, not that nothing was scanned, and
    /// [`Self::scan_pages`] and [`Self::offered`] say what was.
    pub scan_end: Option<ScanEnd>,
    /// Keys the scan offered, summed across every page.
    ///
    /// Not the size of the runnable set: what sits past the last page is in no
    /// counter on this report, and [`Self::scan_end`] is the only thing that
    /// says whether anything does.
    pub offered: usize,
    /// Of those, keys skipped because this runner is holding them off.
    pub skipped_held: usize,
    /// Of those skips, ones that cost a `load` to decide.
    ///
    /// A [`Hold::UntilCommittedStateChanges`] cannot be answered from memory.
    /// Counted because it is the one holdoff that reads the store on every
    /// sweep, and an operator watching read volume should not have to infer it.
    pub holdoff_probes: usize,
    /// Cursor reads taken to decide whether this runner may enter a phase.
    ///
    /// One `load` per offered key that survives the holdoff set, **plus one per
    /// park exit** — the park is a two-step transition and the question has to
    /// be asked on both steps, see the module docs. The price of refusing a
    /// mid-iteration pickup *before* it costs a claim, a fence and an attempt
    /// against `phase_attempts`. Reported rather than absorbed, because it is
    /// read volume an operator watching the store will see and would otherwise
    /// have to attribute to something else. Distinct from
    /// [`Self::holdoff_probes`], which reads for a different question.
    pub entry_probes: usize,
    /// Keys refused because their committed cursor is not at an iteration
    /// boundary.
    ///
    /// Its own counter rather than a filter over [`Self::outcomes`], because it
    /// is the one refusal that says something about the **shape of the durable
    /// work** rather than about this runner.
    ///
    /// # What it counts, which is broader than "stalled work"
    ///
    /// An earlier version of this doc read *"a store full of these is a store
    /// full of runs that stopped mid-iteration and can only be finished by the
    /// process that started them"*, and an operator acting on that would be
    /// acting on the wrong population. The check reads the cursor and nothing
    /// else, and three different things sit inside an iteration:
    ///
    /// - a run abandoned mid-iteration — the case above, and the one this
    ///   counter is named for;
    /// - a run a live holder is advancing right now, offered because its pin
    ///   lapsed;
    /// - a run that stopped at a **resumable** terminal — a pause, a
    ///   confirmation. `driver_worker::next_cursor` leaves the cursor at the
    ///   phase that returned the terminal, so such a run's cursor is `Resolve`
    ///   or `Apply`, and nothing withholds the key: `TerminalKind::is_resumable`
    ///   means no ending marker is published and `WaitReason` has no variant for
    ///   a human, so no committed field says the run is waiting.
    ///
    /// # What this counter LOST, said because the number moved
    ///
    /// A run that ended **non-resumably** was a fourth member of that list and
    /// was the majority of it. Both stores now publish an ending beside the
    /// state and withhold such a key from every scan, so it is never offered and
    /// is counted here no more. An operator comparing this number against one
    /// recorded before that change is comparing two different populations.
    ///
    /// [`HoldReason::AlreadyEnded`] is not reachable through the sweep path
    /// against either shipped store — the reason is now the store's marker
    /// rather than this check getting there first.
    ///
    /// Separating what remains needs the journal, which is an `O(records)` read
    /// per offered key per sweep. The runner does not pay it, so this counter is
    /// a cursor fact and is documented as one.
    pub mid_iteration_refusals: usize,
    /// Keys the fleet holds no host for, and will not.
    ///
    /// Structural and permanent for as long as this fleet lives. It is every key
    /// **the fleet was asked about**, which is not every key the scan offered:
    /// [`Self::mid_iteration_refusals`] is subtracted first — that check runs
    /// before [`HostFleet::host_for`] and, per its own docs, catches most
    /// finished and most stalled runs. `hosts_not_composable` alone is not a
    /// census.
    ///
    /// # THE COUNTERS DO NOT SUM TO `offered`, and an earlier version of this
    /// # doc said they did
    ///
    /// It gave `offered = skipped_held + mid_iteration_refusals +
    /// hosts_not_composable + hosts_unavailable + attempted` as an identity to
    /// reconcile against. That equation does not hold, and an operator applying
    /// it after any store-record refusal concludes that keys went missing. Two
    /// populations sit inside `offered` and outside every one of those terms:
    ///
    /// - **Keys released by a per-key store defect at the pre-claim `load`.**
    ///   `classify_store_error` answers `Disposition::Release` for
    ///   `Corrupt`, `WatermarkAhead`, `WakeLedgerFull`, `Journal`, `Effect`,
    ///   `Key`, `LeaseHeld`, `Conflict` and `LeaseLost`, and the sweep hands
    ///   those to `record_refusal` and moves on. No counter above moves.
    /// - **Keys the sweep never reached**, when it stopped early:
    ///   [`SweepStop::Cancelled`], [`SweepStop::ClaimBudget`], or a
    ///   [`SweepStop::StoreUnavailable`] raised by that same pre-claim `load`.
    ///   [`Self::stopped`] is what says an early stop happened at all.
    ///
    /// The first population IS in [`Self::outcomes`] — one entry per key whose
    /// turn was taken or refused, in scan order, carrying
    /// [`HoldReason::StoreRecordRefused`]. The second is in nothing, by
    /// definition. So the closest thing to an identity this report supports is
    ///
    /// ```text
    /// offered = skipped_held + outcomes.len() + unrepresented
    /// ```
    ///
    /// where `unrepresented` covers the keys an early `break` never reached plus
    /// the single key whose pre-claim `load` failed the sweep closed. A holdoff
    /// skip is deliberately not an outcome — `still_held` decides before any
    /// turn exists — which is why `skipped_held` stays a separate term.
    ///
    /// `offered` is itself summed over pages, and the keys past the last page
    /// are in **neither** side of that identity: they were never offered. A
    /// reconciliation over this report is a statement about what the sweep saw,
    /// and [`Self::scan_end`] is what says whether that was the whole store.
    /// [`Self::stopped`] being `None` is what makes `unrepresented` zero, and
    /// only then do the counters above partition the rest. Reconcile against
    /// `outcomes`; read the counters as rates.
    ///
    /// A **large** value here is the normal reading, not an alarm. A fleet is a
    /// holder's own hosts, so a process running one execution refuses every
    /// other key the store offers, on every sweep. What is worth an alarm is
    /// this being large while [`Self::attempted`] is zero on a process that
    /// believes it is hosting something.
    ///
    /// Separate from [`Self::hosts_unavailable`] because the two call for
    /// opposite actions and one counter covering both told an operator neither.
    pub hosts_not_composable: usize,
    /// Keys whose host was momentarily unavailable, and is retried.
    ///
    /// Transient: a dependency was unreachable. Non-zero here means something
    /// is broken *now*, where a non-zero [`Self::hosts_not_composable`] means
    /// this build was never going to advance those runs.
    pub hosts_unavailable: usize,
    /// Keys actually claimed at least once.
    pub attempted: usize,
    /// Calls to [`advance_once`] across the sweep.
    pub claims: usize,
    /// Phases committed across the sweep.
    pub phases_committed: usize,
    /// One entry per key whose turn was taken or refused, in scan order.
    pub outcomes: Vec<KeyOutcome>,
    /// Why the sweep ended early, if it did.
    pub stopped: Option<SweepStop>,
    /// Entries in the holdoff set after this sweep.
    pub held_keys: usize,
    /// Holdoff entries dropped to stay inside [`RunnerConfig::max_held_keys`].
    ///
    /// Non-zero means keys this runner had decided not to re-claim may be
    /// claimed again. Bounded and reported rather than silent, because it is the
    /// one path by which the permanent answers above become a slow spin.
    pub holdoff_evictions: usize,
    /// The earliest instant a held key becomes claimable again, if any.
    pub next_wake_ms: Option<i64>,
}

impl SweepReport {
    /// Whether this sweep did work, which is what decides if the runner sleeps.
    pub fn did_work(&self) -> bool {
        self.claims > 0
    }
}

/// Why [`WorkerRunner::run`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStop {
    /// The cancellation token fired.
    Cancelled,
    /// The sweep budget the caller passed ran out.
    SweepBudget,
    /// [`RunnerConfig::max_consecutive_store_failures`] consecutive sweeps were
    /// stopped by the store.
    StoreUnavailable { detail: String },
}

/// What the [`LoopReconciler`] passes inside one [`WorkerRunner::run`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcilerTally {
    /// Passes that completed. A pass the store refused is counted in
    /// [`Self::failed`] and in neither of the two below.
    pub passes: usize,
    /// Parked runs judged across those passes.
    pub examined: usize,
    /// Findings across those passes — proved and suspected together, because a
    /// caller reading one number wants "how much is stuck", and the split is in
    /// the log line beside it.
    pub findings: usize,
    /// Parks retired. Always zero under
    /// [`super::reconciler::Recovery::ReportOnly`], which is
    /// [`ReconcilerPolicy::default`]'s choice — and also zero under a
    /// `Recovery::RetireProvedParks` that named no grounds, which is a different
    /// policy with the same number. The per-row `ParkAction` in the reconciler's
    /// own report is what tells the two apart.
    pub retired: usize,
    /// **At least one pass did not see the whole store.** Sticky across passes:
    /// a caller must not read `findings == 0` as "nothing is stuck" while this
    /// is set.
    pub incomplete: bool,
    /// Passes the store refused outright. A reconciler failure never stops the
    /// runner — the sweep half is the half that advances work — so this is the
    /// only place such a pass is visible to a caller.
    pub failed: usize,
    /// Resume position returned by the last successful bounded pass.
    pub scan_resume: Option<ScanCursor>,
}

/// What a whole run of sweeps did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub sweeps: usize,
    pub claims: usize,
    pub phases_committed: usize,
    /// What the [`LoopReconciler`] passes between those sweeps found.
    ///
    /// All zeroes when [`RunnerConfig::reconcile_every`] is `None`, and also
    /// when a run was shorter than one cadence — which are different facts, and
    /// the caller knows which by knowing what it configured.
    pub reconciled: ReconcilerTally,
    /// **At least one sweep stopped paging at its page ceiling with work behind
    /// it.** Sticky across sweeps, for the same reason
    /// [`ReconcilerTally::incomplete`] is: a bounded sweep followed by a
    /// complete one has not made the store fully seen, because the next sweep
    /// starts the walk from the beginning again and meets the same prefix.
    ///
    /// It exists because [`SweepReport::scan_end`] does not otherwise reach an
    /// outer lifecycle owner that calls [`WorkerRunner::run`] and sees only a
    /// `RunReport`. Without this field a pass that gave up on the page ceiling is
    /// indistinguishable from one that found nothing.
    ///
    /// No production phase-host fleet currently drives this runner. The field
    /// remains part of the reusable boundary so a future owner cannot silently
    /// collapse an incomplete scan into an empty one, and
    /// the tests below are what keep it honest in the meantime.
    ///
    /// `false` is **not** a claim that the store was walked to the end: a sweep
    /// that stopped on its claim budget, or one whose scan could not answer, sets
    /// nothing here. It is the narrow claim that no sweep hit the page ceiling.
    pub scans_incomplete: bool,
    pub stop: RunStop,
}

// ============================================================================
// The classifier
// ============================================================================

/// One holdoff entry.
#[derive(Debug, Clone, Copy)]
struct Held {
    hold: Hold,
    reason: HoldReason,
    since_ms: i64,
}

/// Whether the last sweep left the store answering.
///
/// Its one caller is the reconciler gate in [`WorkerRunner::run`]: a pass whose
/// first call is `scan_parked` against a substrate that has just refused a scan
/// buys a second identical error and one more directory walk. Spelled here
/// rather than inline so it cannot drift from [`WorkerRunner::nap_after`]'s
/// reading of the same field.
fn store_ok(report: &SweepReport) -> bool {
    !matches!(report.stopped, Some(SweepStop::StoreUnavailable { .. }))
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// What the runner does about one answer.
///
/// # Every variant by name, and a catch-all here would be a bug
///
/// Not a style preference. [`Advanced`] has seventeen variants and two of them —
/// [`Advanced::EffectIndeterminate`] and [`Advanced::Quarantined`] — must never
/// become "keep going", which is exactly what a catch-all placed above them
/// would silently make of any variant invented later.
///
/// rustc already refuses a variant being *added* while there is no catch-all, so
/// the source gate in `run_loop/mod.rs` is not carrying that. What it carries is
/// narrower and worth stating exactly, because an earlier version of this
/// paragraph over-claimed it: the gate requires each variant to begin an **arm
/// head** in this match. That rules out the two edits rustc is happy with — a
/// fold into a neighbour's or-pattern, which leaves the folded name off any
/// head, and a rename through `use ... as`, which satisfies exhaustiveness under
/// a spelling no reader of this file would recognise.
///
/// Pure, and taking `now_ms` rather than reading a clock, so every holdoff it
/// produces can be asserted against an instant instead of against a sleep.
fn classify(advanced: &Advanced, now_ms: i64, config: &RunnerConfig) -> Disposition {
    let until = |after: Duration| Hold::Until(now_ms.saturating_add(millis(after)));
    match advanced {
        // ── Keep the key ────────────────────────────────────────────────────
        Advanced::Committed { .. } => Disposition::KeepGoing,
        Advanced::RecoveryRewound { .. } => Disposition::KeepGoing,
        // No phase ran, but a durable transition did, and the wait was cleared
        // before this returned — so the next claim cannot see the same park.
        // Bounded by the claim budget rather than by that argument alone.
        //
        // "Keep going" here is NOT "enter the next phase unconditionally", and
        // reading it that way is what made the pre-claim cursor check worthless
        // on the one path it carved out. `leave_park` clears the wait and
        // commits WITHOUT moving the cursor, so the very next claim of this turn
        // would enter whatever phase the run parked at — mid-iteration, from
        // inside the claim, charging an attempt. `take_turn` re-asks the cursor
        // question after this answer; this function is pure and cannot, which is
        // why the check lives there and is named in both places.
        Advanced::LeftPark { .. } => Disposition::KeepGoing,

        // ── The run ended, which is TWO answers and not one ─────────────────
        //
        // Reading a terminal as one thing was a bug in the first draft of this
        // file, and it is the shape of bug this whole module is about: the
        // permanent case and the alive case look identical at the call site.
        //
        // A pause, a confirmation, a park, a sleep: the run is ALIVE and
        // something outside will answer it. Holding one of those permanently
        // would mean a parked run whose wake resolves is offered by the store —
        // exactly the transition the wake index exists to produce — and refused
        // by this runner for the rest of its life.
        //
        // But "not permanent" is not "on a timer", and reading it as one put a
        // paid `Decide` on a sixty-second loop behind a human: a resumable
        // terminal that committed no wait is re-claimed by RE-RUNNING the phase
        // that ended, which is the resume mechanism and is also an LLM call, a
        // journal record and a revision. `Resumption` is where the two are
        // separated and why.
        Advanced::RunEnded {
            terminal, revision, ..
        } if terminal.is_resumable() => match resumption_of(*terminal) {
            Resumption::WhenTheStoreOffersItAgain => Disposition::Release {
                hold: until(config.resumable_terminal_holdoff),
                reason: HoldReason::RunSuspended,
            },
            Resumption::WhenTheCommittedStateChanges => Disposition::Release {
                hold: Hold::UntilCommittedStateChanges {
                    revision: *revision,
                },
                reason: HoldReason::AwaitingAnOutsideAnswer,
            },
        },

        // ── Permanent answers: nothing a further claim can change ───────────
        //
        // MOST of these are offered again by the very next scan, because the
        // discovery side cannot see them. The two ended-run arms are the
        // exception and it is worth naming rather than lumping: both stores
        // publish an ending marker and withhold a key whose committed watermark
        // vouches for a non-resumable terminal, so those two hold a key the next
        // scan was not going to offer anyway. They are kept for three reasons:
        // a decorated or regressed store is what a runner would meet
        // without them, and the hold stops a SECOND claim inside the very sweep
        // that ended the run, which no later store scan can reach. For the rest,
        // the holdoff is still the mitigation, not the fix.
        //
        // `Success`, `Failed`, an exhausted budget: over.
        Advanced::RunEnded { .. } => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::RunEnded,
        },
        // Always non-resumable, and not by coincidence: `advance_once` lets a
        // resumable terminal fall through on purpose and answers this only for
        // one it cannot come back from. So there is no alive case here to miss.
        //
        // Not reachable through `WorkerRunner::sweep` against either shipped
        // store — the ending marker withholds the key first — and reachable
        // through any other caller of `advance_once`, or a decorated/regressed
        // store. Which is why it is an arm and not an `unreachable!`.
        Advanced::RunAlreadyEnded { .. } => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::AlreadyEnded,
        },
        Advanced::TerminalSettlementPending { revision, .. } => Disposition::Release {
            hold: Hold::UntilCommittedStateChanges {
                revision: *revision,
            },
            reason: HoldReason::AwaitingAnOutsideAnswer,
        },
        // Only the executor's max-iterations ceremony concludes this — a pause
        // record, `AgenticMaxIterationsReached`, continue-or-cancel — and a
        // runner has no way to reach it. Re-claiming answers the same thing
        // forever.
        Advanced::IterationCeiling { .. } => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::IterationCeiling,
        },
        Advanced::Quarantined(_) => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::Quarantined,
        },
        // The failure-mode row this runner most needs to obey: a missing effect
        // result is not evidence. Reconciliation is an act by an operator or by
        // the far side, never by another claim.
        Advanced::EffectIndeterminate { .. } => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::EffectIndeterminate,
        },
        Advanced::ExecutionFailed { .. } => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::ExecutionFailed,
        },
        // ── Timed answers: something outside will change them ───────────────
        //
        // The three that carry their own time use it EXACTLY rather than adding a
        // configured interval to it. A run whose retry lands in ten minutes must
        // not be re-claimed in five, and a lease held for another four minutes
        // must not be probed every five seconds.
        Advanced::NotYetRunnable { runnable_at_ms } => Disposition::Release {
            hold: Hold::Until(*runnable_at_ms),
            reason: HoldReason::NotYetRunnable,
        },
        Advanced::LeaseHeld { until_ms, .. } => Disposition::Release {
            hold: Hold::Until(*until_ms),
            reason: HoldReason::LeaseHeld,
        },
        // The placement carries its durable expiry. Waiting for that instant
        // avoids both a hard failure and repeated polling during the full pin.
        Advanced::NotClaimable {
            pinned_until_ms, ..
        } => Disposition::Release {
            hold: Hold::Until(*pinned_until_ms),
            reason: HoldReason::PinnedElsewhere,
        },
        // The attempt is committed, so the quarantine counter advances whether
        // or not this runner comes back. The holdoff only stops one sweep
        // burning every remaining attempt.
        Advanced::PhaseFailed { .. } => Disposition::Release {
            hold: until(config.failure_holdoff),
            reason: HoldReason::PhaseFailed,
        },
        // `list_runnable` withholds a park with no resolution, so this means the
        // park was committed, or re-armed, between the scan and the claim.
        Advanced::Parked { .. } => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::Parked,
        },
        // The scan offered a key whose state then loaded as nothing. Timed
        // rather than permanent: the key names a directory, and a directory can
        // be committed to after this runner looked.
        Advanced::NothingToAdvance => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::NothingToAdvance,
        },

        // ── Refusals ────────────────────────────────────────────────────────
        Advanced::Refused(refusal) => classify_refusal(refusal, now_ms, config),
    }
}

/// The refusal half, split out only so [`classify`]'s match reads as one screen.
fn classify_refusal(refusal: &Refusal, now_ms: i64, config: &RunnerConfig) -> Disposition {
    let until = |after: Duration| Hold::Until(now_ms.saturating_add(millis(after)));
    match refusal {
        Refusal::Store { during, error } => classify_store_error(*during, error, now_ms, config),
        Refusal::Projection(projection) => classify_projection_refusal(projection),
        // Somebody else is working it. Come back.
        Refusal::StaleCommit { .. } => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::StaleCommit,
        },
        // The documented hang: a park with nothing to wake it. A reconciler owns
        // this, not another claim.
        Refusal::ParkWithNoLiveChild => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::ParkWithNoLiveChild,
        },
        // A phase bug. Re-claiming reproduces it.
        Refusal::ExitFromTheEpilogue => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::ExitFromTheEpilogue,
        },
    }
}

/// The projection half, and the reason it is not one blanket answer either.
///
/// `Refusal::Projection` was a catch-all one level below where the source gate
/// in `run_loop/mod.rs` looks — that gate reads arm *heads*, and this arm's head
/// starts upper-case, so five [`LoopStateRefusal`] variants were being answered
/// by one comment claiming *"every one of them re-refuses on the next claim"*.
/// Four do. [`LoopStateRefusal::PinnedElsewhere`] carries `until_ms` precisely
/// because it does not — it is temporary and says so — and it was inheriting a
/// hold that lasts as long as the process.
///
/// Not reachable today: `advance_under_lease` takes one `now_ms` and checks
/// `claimable_by` before it projects, so a pin live enough to refuse here would
/// already have answered [`Advanced::NotClaimable`]. Named variant by variant
/// anyway, because the hazard is the unguarded seam rather than the reachable
/// path: a transient refusal added later, or a second `project` call site
/// reading a different clock, would silently inherit a worker-lifetime hold.
fn classify_projection_refusal(projection: &LoopStateRefusal) -> Disposition {
    match projection {
        // Temporary, and it carries how long. Held to exactly that instant for
        // the same reason `LeaseHeld` is: an answer that knows its own expiry
        // must not be re-checked before it and must not be held past it.
        LoopStateRefusal::PinnedElsewhere { until_ms, .. } => Disposition::Release {
            hold: Hold::Until(*until_ms),
            reason: HoldReason::PinnedElsewhere,
        },
        // A placement invariant, an unreadable ceiling, a handover that would
        // strand a portable run, a budget going backwards. Every one of these
        // re-refuses on the next claim with the same inputs.
        LoopStateRefusal::PortableRunMayReachAProcessLocalBrowser { .. }
        | LoopStateRefusal::UnreadableCeiling { .. }
        | LoopStateRefusal::HandoverLeavesPortableRunNoTransport { .. }
        | LoopStateRefusal::WorkBudgetWentBackwards { .. } => Disposition::Release {
            hold: Hold::UntilThisWorkerRestarts,
            reason: HoldReason::ProjectionRefused,
        },
    }
}

/// THE fail-closed decision, and the reason it is not one blanket answer for
/// every [`StoreError`].
///
/// [`StoreError::Unavailable`] is the substrate saying it cannot be reached, and
/// continuing the batch against it would take fifteen more claims that can
/// record nothing. Every other variant names a defect in **one** run's records,
/// or a race with another worker, and calling either an outage would stall every
/// healthy execution on one bad file.
///
/// Named variant by variant rather than `other =>`, so that a `StoreError` added
/// later has to be classified rather than inheriting whichever answer happened
/// to sit at the bottom of this match.
fn classify_store_error(
    during: &'static str,
    error: &StoreError,
    now_ms: i64,
    config: &RunnerConfig,
) -> Disposition {
    let until = |after: Duration| Hold::Until(now_ms.saturating_add(millis(after)));
    match error {
        StoreError::Unavailable { detail } => Disposition::FailClosed {
            during,
            detail: detail.clone(),
        },
        // Races with another holder. It knows what it is doing; come back.
        StoreError::LeaseHeld { until_ms, .. } => Disposition::Release {
            hold: Hold::Until(*until_ms),
            reason: HoldReason::LeaseHeld,
        },
        StoreError::Conflict { .. } | StoreError::LeaseLost { .. } => Disposition::Release {
            hold: until(config.transient_holdoff),
            reason: HoldReason::StaleCommit,
        },
        // One run's records are wrong. Nothing is deleted and the key is held
        // for this worker's life, because every re-claim reproduces the refusal.
        StoreError::Corrupt { .. }
        | StoreError::WatermarkAhead { .. }
        | StoreError::WakeLedgerFull { .. }
        | StoreError::Journal(_)
        | StoreError::Effect(_)
        | StoreError::Key(_) => {
            error!(
                target: "agentic.loop_runner",
                during = %during,
                %error,
                "[LOOP_RUNNER] the store refused one execution's records; that key is held for \
                 this worker's lifetime and nothing was deleted"
            );
            Disposition::Release {
                hold: Hold::UntilThisWorkerRestarts,
                reason: HoldReason::StoreRecordRefused,
            }
        },
    }
}

// ============================================================================
// The runner
// ============================================================================

/// Discover runnable executions, advance what this worker can, move on.
pub struct WorkerRunner {
    worker: WorkerId,
    config: RunnerConfig,
    held: BTreeMap<ExecutionKey, Held>,
}

impl WorkerRunner {
    pub fn new(worker: WorkerId, config: RunnerConfig) -> Self {
        Self {
            worker,
            config,
            held: BTreeMap::new(),
        }
    }

    pub fn worker(&self) -> &WorkerId {
        &self.worker
    }

    /// What this runner is currently withholding a key for, if anything.
    ///
    /// Exposed because a holdoff is the runner's only durable-looking decision
    /// and it lives in memory: an operator asking "why is this run not being
    /// picked up" has no file to read, and the answer has to be reachable from
    /// somewhere.
    pub fn holdoff(&self, key: &ExecutionKey) -> Option<(Hold, HoldReason)> {
        self.held.get(key).map(|held| (held.hold, held.reason))
    }

    /// One pass: scan, take each offered key's turn with a host the caller lent
    /// for it, and ask for the next page of the walk until the batch is met.
    ///
    /// Never sleeps, and never reconciles — a [`LoopReconciler`] pass belongs
    /// between sweeps, which is [`Self::run`]'s job. A caller wanting a resident
    /// worker uses that; a caller wanting a short-lived one calls this and
    /// exits, which is the shape the design is aiming at.
    ///
    /// # It PAGES, and the loop is the point rather than an implementation
    /// # detail
    ///
    /// [`LoopStateStore::list_runnable`] takes a `limit` and no offset, so its
    /// caller sees the first `limit` offerable keys of the walk and can never
    /// see past them. A quarantined run, one past its iteration ceiling and one
    /// holding an indeterminate effect all answer a permanent refusal at claim
    /// time and commit nothing, so every later scan offers them again,
    /// unchanged — and none of the three can be filtered by the store, because
    /// each is judged against a threshold the store does not hold. `limit` such
    /// keys at the head of the walk starve every live execution behind them
    /// while the scan reports a full page and looks healthy.
    ///
    /// So this asks [`LoopStateStore::scan_runnable`] for a page, works it, and
    /// feeds the page's [`ScanCursor`] back for the next one. It stops on the
    /// first of: the batch met, the walk ended, the sweep stopped for its own
    /// reasons, or [`RunnerConfig::max_scan_pages`]. [`SweepReport::scan_end`]
    /// says which — and one of the four is a coverage hole rather than a
    /// stopping point.
    ///
    /// A sweep whose first page fills the batch takes exactly one scan, which is
    /// the overwhelmingly common shape and is unchanged from the single-call
    /// version this replaced.
    pub async fn sweep(
        &mut self,
        store: &dyn LoopStateStore,
        hosts: &mut dyn HostFleet,
        cancel: &CancellationToken,
    ) -> SweepReport {
        let mut report = SweepReport::default();
        self.expire_holdoffs(Utc::now().timestamp_millis());

        // The FIRST page asks for `batch` PLUS what is held; every later page
        // asks for `batch` alone. The two prefixes are different problems.
        //
        // The widening escapes a prefix of keys THIS RUNNER has already refused
        // and remembered: such a key is still claimable, unleased, unparked and
        // inside every timer, so it keeps its slot in the walk forever, and the
        // holdoff set is process-local — nothing the runner remembers moves
        // where the store stops. Bounded by construction: the holdoff set never
        // exceeds `max_held_keys`, so the first page never asks for more than
        // `batch + max_held_keys`.
        //
        // The CURSOR escapes a prefix this runner has never met, which the
        // widening cannot touch — a fresh process, or a runner whose whole life
        // is one sweep. Widening the later pages too would multiply the two
        // ceilings for no gain: the cursor has already carried the walk past the
        // held prefix that widening exists for.
        report.scan_limit = self.config.batch.saturating_add(self.held.len());
        // Zero is read as one. See `RunnerConfig::max_scan_pages`.
        let max_pages = self.config.max_scan_pages.max(1);
        // SWEEP-LOCAL, and never a field on `self`. A `ScanCursor` is a position
        // in the walk of the store that produced it, and `store` is an argument
        // to this function — the next sweep may be handed a different one, and a
        // cursor kept across that boundary would be fed to a store that never
        // minted it.
        let mut after: Option<ScanCursor> = None;
        // Whether the store said there is anything past the page just worked.
        let mut walk_has_more = false;
        // Whether the scan itself refused. Distinct from `report.stopped`, which
        // a per-key store defect also sets: after a failed scan nothing is known
        // about the rest of the walk, not even whether there is any.
        let mut scan_failed = false;

        'pages: loop {
            let limit = if report.scan_pages == 0 {
                report.scan_limit
            } else {
                self.config.batch
            };
            report.scan_pages += 1;
            let page = match store
                .scan_runnable(&self.worker, limit, after.as_ref())
                .await
            {
                Ok(page) => page,
                // Discovery failing is the same refusal as recording failing: a
                // runner that cannot see what is runnable must not guess. Every
                // scan error is an outage rather than a per-key defect, because
                // the error names no key.
                //
                // A store that has simply not implemented the cursored scan
                // arrives here too, carrying the trait default's refusal, and is
                // treated identically on purpose — falling back to the unpaged
                // `list_runnable` would put the starvation back silently. See
                // the module docs.
                Err(error) => {
                    let detail = error.to_string();
                    scan_failed = true;
                    error!(
                        target: "agentic.loop_runner",
                        worker = %self.worker,
                        page = report.scan_pages,
                        %detail,
                        "[LOOP_RUNNER] the runnable scan could not answer; the sweep advances \
                         nothing further"
                    );
                    report.stopped = Some(SweepStop::StoreUnavailable {
                        during: "scan_runnable",
                        detail,
                    });
                    break 'pages;
                },
            };
            let RunnableScan { keys, resume } = page;
            report.offered += keys.len();
            walk_has_more = resume.is_some();

            for key in keys {
                if cancel.is_cancelled() {
                    report.stopped = Some(SweepStop::Cancelled);
                    break 'pages;
                }
                if report.claims >= self.config.max_claims_per_sweep {
                    report.stopped = Some(SweepStop::ClaimBudget);
                    break 'pages;
                }
                // A FRESH clock per key, deliberately, and the opposite choice from
                // `advance_once`'s one-clock-per-cycle. That rule exists so a single
                // execution's claim, deadline and retry decisions cannot disagree
                // about what time it is. Here the keys are independent and a sweep
                // can run for minutes, so a clock read once at the top would go on
                // withholding keys whose holdoff lapsed while the sweep was working.
                if self
                    .still_held(store, &key, Utc::now().timestamp_millis(), &mut report)
                    .await
                {
                    report.skipped_held += 1;
                    continue;
                }

                // The committed cursor BEFORE the host, and both before the claim.
                //
                // A phase that reads what an earlier phase of the SAME iteration
                // produced cannot be entered by a holder that did not run that
                // phase — `state::ForeignPickup` is where the decision "a foreign
                // pickup starts at an iteration boundary" is written down, and
                // `state::in_memory_inputs_of` is the list.
                //
                // `run_phase` does refuse such an entry, so this is not the only
                // thing standing between a cold host and a re-decided turn. What it
                // changes is WHERE the refusal lands: `run_phase` refuses from
                // inside the claim, by which point the lease is taken, the fence has
                // moved and `commit_failed_attempt` has charged the run an attempt
                // against its quarantine counter — for a defect belonging to the
                // pickup rather than to the run. On the default `WorkerConfig` that
                // is a healthy execution quarantined by a handful of sweeps it did
                // nothing to deserve.
                report.entry_probes += 1;
                let committed = match store.load(&key).await {
                    Ok(committed) => committed,
                    Err(store_refusal) => {
                        let probe_now_ms = Utc::now().timestamp_millis();
                        match classify_store_error(
                            "load_before_provide",
                            &store_refusal,
                            probe_now_ms,
                            &self.config,
                        ) {
                            Disposition::FailClosed { during, detail } => {
                                error!(
                                    target: "agentic.loop_runner",
                                    worker = %self.worker,
                                    execution = %key,
                                    %detail,
                                    "[LOOP_RUNNER] the store could not answer the cursor read; the \
                                     sweep stops rather than claiming against a substrate that has \
                                     just said it can record nothing"
                                );
                                report.stopped =
                                    Some(SweepStop::StoreUnavailable { during, detail });
                                break 'pages;
                            },
                            Disposition::Release { hold, reason } => {
                                self.record_refusal(&mut report, key, hold, reason);
                                continue;
                            },
                            // Not reachable: every arm of `classify_store_error`
                            // either releases the key or fails the sweep closed.
                            // Spelled anyway, and spelled as a HOLD, because the
                            // alternative default — falling through to the claim —
                            // is the one answer that would assert something nothing
                            // established, on a key whose state could not be read.
                            Disposition::KeepGoing => {
                                let hold = Hold::Until(
                                    probe_now_ms
                                        .saturating_add(millis(self.config.transient_holdoff)),
                                );
                                self.record_refusal(
                                    &mut report,
                                    key,
                                    hold,
                                    HoldReason::StoreRecordRefused,
                                );
                                continue;
                            },
                        }
                    },
                };
                // Asked only of a run that is NOT parked. A parked key's pickup is a
                // park EXIT — the driver clears the wait and returns without running
                // a phase, so none of these values is read — and refusing it here
                // would withhold the one transition the wake index exists to
                // produce.
                //
                // The carve-out is only half of the answer, and on its own it is
                // none of it: `leave_park` clears the wait and commits WITHOUT
                // moving the cursor, so the turn's next claim would enter the phase
                // the run parked at. `take_turn` re-asks this same question after a
                // park exit — see `after_park`, and the module docs for the trace.
                //
                // `None` is deliberately not this guard's to answer: `advance_once`
                // reports `NothingToAdvance` for a key with no committed state, and
                // giving one condition two reasons in the report is how a reader
                // stops trusting either.
                if let Some(committed) = committed.as_ref() {
                    if committed.state.wait.is_none() {
                        if let super::state::ForeignPickup::MidIteration { phase, needs } =
                            committed.state.foreign_pickup()
                        {
                            report.mid_iteration_refusals += 1;
                            debug!(
                                target: "agentic.loop_runner",
                                execution = %key,
                                ?phase,
                                ?needs,
                                "[LOOP_RUNNER] this run's committed cursor is not at an iteration \
                                 boundary; the values that phase reads were produced in another \
                                 holder's memory — or the run ended there — so the key is not claimed"
                            );
                            // Timed, not permanent. A cursor is a snapshot: another
                            // holder can advance this one to a boundary and stop,
                            // and a worker-lifetime hold would refuse the pickup at
                            // exactly that moment. See
                            // `RunnerConfig::mid_iteration_holdoff`.
                            let hold = Hold::Until(
                                Utc::now()
                                    .timestamp_millis()
                                    .saturating_add(millis(self.config.mid_iteration_holdoff)),
                            );
                            self.record_refusal(
                                &mut report,
                                key,
                                hold,
                                HoldReason::MidIterationPickup,
                            );
                            continue;
                        }
                    }
                }

                // The host BEFORE the claim. A key with no host is never leased —
                // see the module docs for why that ordering is the safe one.
                //
                // Scoped, because `HostForKey::Ready` carries a borrow OF THE FLEET
                // and `hosts.run_ended` below needs the fleet back. The block is
                // what ends that borrow; without it the two would be a
                // borrow-checker error rather than a silent aliasing bug, but the
                // shape is written out because the reason is not obvious from the
                // call.
                let mut ended: Option<EndedRun> = None;
                let turn = {
                    let host = match hosts.host_for(&key) {
                        HostForKey::Ready(host) => host,
                        HostForKey::NotComposable { detail } => {
                            report.hosts_not_composable += 1;
                            debug!(
                                target: "agentic.loop_runner",
                                execution = %key,
                                %detail,
                                "[LOOP_RUNNER] this fleet holds no host for that run; it is not \
                                 claimed"
                            );
                            self.record_refusal(
                                &mut report,
                                key,
                                Hold::UntilThisWorkerRestarts,
                                HoldReason::HostNotComposable,
                            );
                            continue;
                        },
                        HostForKey::Unavailable { detail } => {
                            report.hosts_unavailable += 1;
                            warn!(
                                target: "agentic.loop_runner",
                                execution = %key,
                                %detail,
                                "[LOOP_RUNNER] the host for this run was momentarily unavailable"
                            );
                            let hold = Hold::Until(
                                Utc::now()
                                    .timestamp_millis()
                                    .saturating_add(millis(self.config.host_unavailable_holdoff)),
                            );
                            self.record_refusal(
                                &mut report,
                                key,
                                hold,
                                HoldReason::HostUnavailable,
                            );
                            continue;
                        },
                    };

                    report.attempted += 1;
                    self.take_turn(store, host, &key, &mut report, cancel, &mut ended)
                        .await
                };
                // The fleet is whole again here, which is the only place this can be
                // said. A run that ended is handed BACK rather than dropped: the
                // holder that lent the host is the participant that owes somebody
                // the answer, and the runner is not.
                if let Some(EndedRun { outcome, terminal }) = ended {
                    hosts.run_ended(&key, outcome, terminal);
                }
                let stop = matches!(turn.end, EndOfTurn::StoreStopped);
                report.outcomes.push(turn);
                if stop {
                    break 'pages;
                }
            }

            // ── the sweep claim budget, re-asked HERE ──────────────────────
            //
            // It is otherwise asked in exactly two places, and both of them need
            // a NEXT thing to be about: the top of the next key's iteration, and
            // `take_turn`'s own loop before a further claim of the same key. A
            // budget spent on the LAST key of a page has neither — the key loop
            // simply runs out — so `report.stopped` was still `None` at the
            // guard below and the sweep asked the store for one more page it
            // abandoned on that page's first key.
            //
            // That cost a full `scan_runnable` — on `store/fs.rs` a directory
            // walk of every level — per sweep, on a live run's critical path,
            // for a page nothing was ever going to work. In a one-claim caller
            // the condition reduces to *"the key it claimed was last on its
            // page"*.
            //
            // # It repeats the guard's OTHER conditions, deliberately
            //
            // The condition is *"the sweep was about to ask for another page and
            // could not use one"*, not *"the budget is spent"*. Written the
            // short way it would fire on a sweep that stopped for a better
            // reason and would DOWNGRADE its report: a sweep that also filled
            // its batch says `ScanEnd::BatchFilled` and one that also reached the
            // end of the walk says `ScanEnd::WalkedTheWholeStore`, and both are
            // stronger statements than the `SweepEnded` that `report.stopped`
            // being set produces. `SweepStop` is documented as *why a sweep
            // stopped before it ran out of keys*, which is only true when the
            // walk has more.
            //
            // Against the version without this block the report is identical
            // except that `scan_pages` is one lower and `offered` no longer
            // counts a page whose keys were never looked at — both of which are
            // the honest numbers. Setting `stopped` rather than adding a fifth
            // condition to the guard also keeps the classifier's branches the
            // exact negation of that guard, so its final arm stays unreachable.
            if report.stopped.is_none()
                && walk_has_more
                && report.attempted < self.config.batch
                && report.scan_pages < max_pages
                && report.claims >= self.config.max_claims_per_sweep
            {
                report.stopped = Some(SweepStop::ClaimBudget);
            }

            // ── ask for another page, or stop asking ───────────────────────
            //
            // FOUR conditions, all of which must hold, and the first two are
            // what keep the common sweep to a single round trip:
            //
            // - the sweep is still running. `report.stopped` covers a claim
            //   budget spent, a cancellation and a store refusal, and paging on
            //   past any of them would ask a store for work nothing is going to
            //   take;
            // - the store said there IS more. `resume: None` is a claim, not an
            //   absence — the contract on `scan_runnable` is that a walk which
            //   stopped short for ANY reason names a resume point — so this is
            //   the walk's own answer and not an inference from a short page;
            // - the batch is unmet, counted in keys this sweep could actually
            //   take a turn on. A page of keys this fleet cannot host has
            //   offered work and produced none, and counting `offered` here
            //   would stop the loop on exactly the input it exists for;
            // - the page ceiling has room. See `RunnerConfig::max_scan_pages`
            //   for what its firing costs.
            if report.stopped.is_none()
                && walk_has_more
                && report.attempted < self.config.batch
                && report.scan_pages < max_pages
            {
                // The cursor the page handed back, and never the one this page
                // was given: the contract guarantees `resume` is STRICTLY after
                // the position it started from, which is the property that makes
                // this loop terminate. Feeding back the arriving cursor — or
                // `None` — would re-ask the same question forever, wearing the
                // shape of the fix.
                after = resume;
                continue 'pages;
            }
            break 'pages;
        }

        // Why the paging stopped, decided in ONE place. Five of the exits above
        // are a `break 'pages`, four of them from inside the key loop, and a
        // field assigned at five sites is a field that is one day assigned at
        // four.
        report.scan_end = if scan_failed {
            // FIRST, and ahead of `report.stopped`, on ANY page and not only the
            // first. From the refused page onward nothing is known about the
            // walk — not even whether there is any more of it — so there is no
            // honest variant to name. `SweepEnded` would be the answer the next
            // branch gave (the scan failure sets `stopped` too) and it would be
            // a claim about a sweep that chose to stop, which this did not.
            //
            // It does NOT mean nothing was scanned. Pages before this one
            // offered their keys and took their turns; `scan_pages` and
            // `offered` carry that and `ScanEnd`'s docs say so.
            None
        } else if report.stopped.is_some() {
            Some(ScanEnd::SweepEnded)
        } else if !walk_has_more {
            Some(ScanEnd::WalkedTheWholeStore)
        } else if report.attempted >= self.config.batch {
            Some(ScanEnd::BatchFilled)
        } else if report.scan_pages >= max_pages {
            Some(ScanEnd::PageCap)
        } else {
            // Not reachable: these four are the negation of the `continue`
            // guard, so a break satisfying none of them does not exist. Spelled
            // as the answer that CLAIMS THE LEAST rather than as a panic in a
            // worker mid-sweep — and deliberately not as `WalkedTheWholeStore`,
            // which would tell an operator the store had been seen to the end
            // when nothing established it.
            Some(ScanEnd::SweepEnded)
        };
        if report.scan_end == Some(ScanEnd::PageCap) {
            warn!(
                target: "agentic.loop_runner",
                worker = %self.worker,
                pages = report.scan_pages,
                offered = report.offered,
                attempted = report.attempted,
                batch = self.config.batch,
                "[LOOP_RUNNER] the sweep stopped paging at its page ceiling without filling its \
                 batch; everything past the last page was not examined by this sweep, and if the \
                 prefix refuses every sweep the same way then that tail is unreachable from here"
            );
        }

        report.held_keys = self.held.len();
        report.next_wake_ms = self.next_wake_ms();
        info!(
            target: "agentic.loop_runner",
            worker = %self.worker,
            scan_limit = report.scan_limit,
            // The first page's limit, the number of pages, and why the paging
            // stopped. Three fields rather than one, because "the scan asked for
            // 16" and "the scan asked four times" and "and there is more behind
            // it" are three different things an operator acts on differently.
            scan_pages = report.scan_pages,
            scan_end = ?report.scan_end,
            offered = report.offered,
            skipped_held = report.skipped_held,
            holdoff_probes = report.holdoff_probes,
            entry_probes = report.entry_probes,
            // The shape of the durable work, not of this runner: a store full
            // of these is a store full of runs that stopped mid-iteration and
            // can be finished only by the process that started them.
            mid_iteration = report.mid_iteration_refusals,
            // Two fields rather than one sum: a structural gap this build will
            // never close and a dependency that was down for a moment call for
            // opposite actions, and a single `hosts_refused` said neither.
            hosts_not_composable = report.hosts_not_composable,
            hosts_unavailable = report.hosts_unavailable,
            attempted = report.attempted,
            claims = report.claims,
            phases = report.phases_committed,
            held = report.held_keys,
            evictions = report.holdoff_evictions,
            stopped = ?report.stopped,
            "[LOOP_RUNNER] sweep complete"
        );
        report
    }

    /// One key's turn: claim, advance, release, repeat until it says stop.
    ///
    /// `finished` is an out-parameter rather than part of [`KeyOutcome`] for one
    /// reason: `KeyOutcome` derives `PartialEq, Eq` and every report type in this
    /// file is compared in tests, while `AgenticOutcome` derives neither. Putting
    /// the outcome in the report would have cost every one of those comparisons.
    /// It is written **at most once** per turn, because a turn ends the moment a
    /// run does.
    ///
    /// Named `finished` and not `ended` because `ended` is already the
    /// [`KeyOutcome`] constructor closure a few lines into the body, and a
    /// parameter of that name would be shadowed by it silently. This paragraph
    /// exists because an earlier version of this doc explained the
    /// out-parameter under the name the rename removed.
    async fn take_turn(
        &mut self,
        store: &dyn LoopStateStore,
        host: &mut dyn WorkerHost,
        key: &ExecutionKey,
        report: &mut SweepReport,
        cancel: &CancellationToken,
        // NOT `ended`: this function already binds a closure of that name three
        // lines down, and the closure would shadow the parameter silently.
        finished: &mut Option<EndedRun>,
    ) -> KeyOutcome {
        let mut claims = 0usize;
        let mut phases = 0usize;
        let ended = |claims, phases, end| KeyOutcome {
            key: key.clone(),
            claims,
            phases_committed: phases,
            end,
        };
        loop {
            if claims >= self.config.max_claims_per_execution {
                return ended(claims, phases, EndOfTurn::YieldedOnClaimBudget);
            }
            if report.claims >= self.config.max_claims_per_sweep {
                report.stopped = Some(SweepStop::ClaimBudget);
                return ended(claims, phases, EndOfTurn::YieldedOnSweepBudget);
            }
            if cancel.is_cancelled() {
                report.stopped = Some(SweepStop::Cancelled);
                return ended(claims, phases, EndOfTurn::Cancelled);
            }

            let advanced = advance_once(store, host, key, &self.worker, &self.config.worker).await;
            claims += 1;
            report.claims += 1;
            if matches!(
                advanced,
                Advanced::Committed { .. } | Advanced::RunEnded { .. }
            ) {
                phases += 1;
                report.phases_committed += 1;
            }

            let left_park = matches!(advanced, Advanced::LeftPark { .. });
            let disposition = classify(&advanced, Utc::now().timestamp_millis(), &self.config);
            // Taken BEFORE the disposition is acted on, and by value, because
            // every arm below either continues the loop or returns — so there is
            // no later point at which `advanced` is still owned. `classify` took
            // it by reference precisely so this move is available here.
            if let Advanced::RunEnded {
                outcome, terminal, ..
            } = advanced
            {
                *finished = Some(EndedRun { outcome, terminal });
            }
            match disposition {
                // A park exit is the one `KeepGoing` that did not run a phase
                // and did not move the cursor, so the next claim of this turn
                // would be a COLD entry into whatever phase the run parked at.
                // The cursor question is therefore re-asked here, on the same
                // terms the sweep asks it before the first claim.
                Disposition::KeepGoing if left_park => {
                    let after = self.after_park(store, key, report).await;
                    match after {
                        AfterPark::Enter => continue,
                        AfterPark::Refuse { hold, reason } => {
                            self.hold(key, hold, reason, report);
                            return ended(claims, phases, EndOfTurn::Held { hold, reason });
                        },
                        AfterPark::StoreUnavailable { during, detail } => {
                            error!(
                                target: "agentic.loop_runner",
                                execution = %key,
                                during = %during,
                                %detail,
                                "[LOOP_RUNNER] the store could not answer the cursor read after \
                                 a park exit; the sweep stops rather than entering a phase it \
                                 could not check"
                            );
                            report.stopped = Some(SweepStop::StoreUnavailable { during, detail });
                            return ended(claims, phases, EndOfTurn::StoreStopped);
                        },
                    }
                },
                Disposition::KeepGoing => continue,
                Disposition::Release { hold, reason } => {
                    self.hold(key, hold, reason, report);
                    return ended(claims, phases, EndOfTurn::Held { hold, reason });
                },
                // Fail closed. Nothing is held against the key: nothing was
                // learned about it, and holding it off would withhold a healthy
                // run because the disk was full for a moment.
                Disposition::FailClosed { during, detail } => {
                    error!(
                        target: "agentic.loop_runner",
                        execution = %key,
                        during = %during,
                        %detail,
                        "[LOOP_RUNNER] the store could not answer; the sweep stops rather than \
                         running unrecorded work"
                    );
                    report.stopped = Some(SweepStop::StoreUnavailable { during, detail });
                    return ended(claims, phases, EndOfTurn::StoreStopped);
                },
            }
        }
    }

    /// Sweep until told to stop, reconciling between sweeps.
    ///
    /// The resident shape. A short-lived worker calls [`Self::sweep`] once and
    /// exits; this is for a process that stays up, and it is the only place in
    /// this module that sleeps. It sleeps **between** sweeps and never while
    /// holding a lease — the whole reason `BoundaryOutcome::Retry` became a
    /// committed time rather than an in-place `tokio::time::sleep`.
    ///
    /// # The reconciler runs here, and this is the ordering that matters
    ///
    /// A [`LoopReconciler`] pass is taken **after** a sweep and **before** the
    /// nap, on [`RunnerConfig::reconcile_every`]. Between the two rather than
    /// inside either, because a retirement takes a lease on a parked run and the
    /// one thing that must not happen is this process's reconciler and this
    /// process's sweep holding the same key at once. Sequenced in one loop, that
    /// is true by construction; in two tasks it would be true by luck.
    ///
    /// A pass that fails does **not** stop the runner and does not count toward
    /// [`RunnerConfig::max_consecutive_store_failures`]. The sweep is the half
    /// that advances work, and stalling live executions because a park listing
    /// could not be produced would trade the important failure for the
    /// diagnostic one. It is counted in [`ReconcilerTally::failed`] instead.
    ///
    /// # TWO GATES SIT AFTER `reconcile_every`, and a caller keeping its own
    /// # cadence has to know about both
    ///
    /// Setting [`RunnerConfig::reconcile_every`] to `Some` is a request, not a
    /// guarantee. The pass is skipped when the cancellation check at the top of
    /// this loop breaks first, and again when `store_ok` reads the sweep as an
    /// outage. A caller that decided *this process is due* before calling has
    /// therefore spent a slot on a pass that may never happen, and must
    /// reconcile its own stamp against
    /// [`ReconcilerTally::passes`] and [`ReconcilerTally::failed`] afterwards.
    /// Both being zero is the report saying *no pass took place*; that caller
    /// returns its stamp on exactly that condition.
    pub async fn run(
        &mut self,
        store: &dyn LoopStateStore,
        hosts: &mut dyn HostFleet,
        cancel: &CancellationToken,
        max_sweeps: Option<usize>,
    ) -> RunReport {
        let mut sweeps = 0usize;
        let mut claims = 0usize;
        let mut phases = 0usize;
        let mut consecutive_store_failures = 0usize;
        let mut reconciled = ReconcilerTally::default();
        // STICKY, and set from the sweep's own answer rather than re-derived
        // here: a later complete sweep does not make an earlier bounded one
        // complete, because the walk restarts at the head every time.
        let mut scans_incomplete = false;
        // Due at the FIRST opportunity, not one cadence in. A worker that comes
        // up after a crash is the process most likely to be looking at parks
        // nothing will ever wake, and making it wait half an hour to notice
        // would put the cadence's cost where its value is not.
        let mut reconcile_due_ms = Utc::now().timestamp_millis();

        let stop = loop {
            if cancel.is_cancelled() {
                break RunStop::Cancelled;
            }
            if max_sweeps.is_some_and(|max| sweeps >= max) {
                break RunStop::SweepBudget;
            }

            let report = self.sweep(store, hosts, cancel).await;
            sweeps += 1;
            claims += report.claims;
            phases += report.phases_committed;
            // A `match` with no catch-all, so a [`ScanEnd`] variant added later
            // has to be placed here rather than inheriting `false` — and `false`
            // is the answer that says "the store was covered", which is the
            // dangerous direction for a coverage flag to default to.
            scans_incomplete |= match report.scan_end {
                Some(ScanEnd::PageCap) => true,
                Some(ScanEnd::WalkedTheWholeStore)
                | Some(ScanEnd::BatchFilled)
                | Some(ScanEnd::SweepEnded)
                | None => false,
            };

            let store_detail = match &report.stopped {
                Some(SweepStop::StoreUnavailable { detail, .. }) => Some(detail.clone()),
                _ => None,
            };
            match store_detail {
                Some(detail) => {
                    consecutive_store_failures += 1;
                    if consecutive_store_failures >= self.config.max_consecutive_store_failures {
                        break RunStop::StoreUnavailable { detail };
                    }
                },
                None => consecutive_store_failures = 0,
            }

            // Skipped entirely when the store has just said it cannot answer:
            // a pass whose first call is `list_parked` against that substrate
            // buys a second identical error and one more directory walk.
            if let Some(every) = self.config.reconcile_every {
                let now_ms = Utc::now().timestamp_millis();
                if now_ms >= reconcile_due_ms && store_ok(&report) {
                    self.reconcile_once(store, &mut reconciled).await;
                    reconcile_due_ms = Utc::now().timestamp_millis().saturating_add(millis(every));
                }
            }

            // BEFORE the nap, and the placement is the point. The check at the
            // top of the loop runs after the wait, so a one-sweep pass used to
            // pay a full `idle_backoff` it could never spend — which is the
            // difference between a pickup that returns in milliseconds and one
            // that returns in seconds.
            if max_sweeps.is_some_and(|max| sweeps >= max) {
                break RunStop::SweepBudget;
            }

            let nap = self.nap_after(&report);
            if nap.is_zero() {
                continue;
            }
            tokio::select! {
                _ = cancel.cancelled() => break RunStop::Cancelled,
                _ = tokio::time::sleep(nap) => {},
            }
        };

        RunReport {
            sweeps,
            claims,
            phases_committed: phases,
            reconciled,
            scans_incomplete,
            stop,
        }
    }

    /// One [`LoopReconciler`] pass, folded into the tally.
    ///
    /// Separate from [`Self::run`] so the reconciler's failure handling is one
    /// place rather than an arm inside a loop that also decides three other
    /// things.
    async fn reconcile_once(&self, store: &dyn LoopStateStore, tally: &mut ReconcilerTally) {
        let mut policy = self.config.reconciler.clone();
        if tally.passes > 0 {
            policy.scan_after = tally.scan_resume.clone();
        }
        let reconciler = LoopReconciler::new(store, policy);
        match reconciler.reconcile().await {
            Ok(report) => {
                tally.passes += 1;
                tally.examined += report.examined;
                tally.findings += report.findings.len();
                tally.retired += report.retired;
                // STICKY as a report of this runner's history: once one pass was
                // capped, the tally records that fact. The resume cursor below
                // ensures the next pass continues after that page rather than
                // walking the same prefix again.
                tally.incomplete |= report.incomplete;
                tally.scan_resume = report.resume.clone();
                info!(
                    target: "agentic.loop_runner",
                    worker = %self.worker,
                    parked = report.parked,
                    examined = report.examined,
                    findings = report.findings.len(),
                    retired = report.retired,
                    incomplete = report.incomplete,
                    "[LOOP_RUNNER] reconciler pass complete"
                );
            },
            Err(error) => {
                tally.failed += 1;
                warn!(
                    target: "agentic.loop_runner",
                    worker = %self.worker,
                    %error,
                    "[LOOP_RUNNER] a reconciler pass could not be taken; the sweep half is \
                     unaffected and no park was judged"
                );
            },
        }
    }

    /// How long to wait before the next sweep.
    ///
    /// Zero when the last sweep did work or was cut by a budget: a worker that
    /// just advanced something has more to do, and a fixed interval after every
    /// sweep would put the idle backoff between two phases of a live run.
    fn nap_after(&self, report: &SweepReport) -> Duration {
        if matches!(report.stopped, Some(SweepStop::StoreUnavailable { .. })) {
            return self.config.store_backoff;
        }
        if report.did_work() || matches!(report.stopped, Some(SweepStop::ClaimBudget)) {
            return Duration::ZERO;
        }
        // Nothing ran. Wait the idle interval, but never past the moment a held
        // key becomes claimable again — which is what keeps a committed retry
        // delay from being served *late* as well as from being hot-looped.
        let idle = self.config.idle_backoff;
        match report.next_wake_ms {
            Some(wake) => {
                let until = wake.saturating_sub(Utc::now().timestamp_millis()).max(0);
                idle.min(Duration::from_millis(
                    u64::try_from(until).unwrap_or(u64::MAX),
                ))
            },
            None => idle,
        }
    }

    /// Whether the cursor a park exit left behind is one this runner may enter.
    ///
    /// One `load` — no claim, no lease, no fence, no write — counted in
    /// [`SweepReport::entry_probes`] beside the pre-claim probe, because it is
    /// the same question asked at the other end of the same two-step transition.
    ///
    /// The park exit itself is **not** undone. The wake was consumed and the
    /// commit stands; what this refuses is only the turn walking on into a phase
    /// whose in-memory inputs belong to a holder this runner is not.
    async fn after_park(
        &self,
        store: &dyn LoopStateStore,
        key: &ExecutionKey,
        report: &mut SweepReport,
    ) -> AfterPark {
        report.entry_probes += 1;
        let now_ms = Utc::now().timestamp_millis();
        let committed = match store.load(key).await {
            Ok(Some(committed)) => committed,
            // Nothing under the key one claim after a commit. Not this check's
            // to name: the next `advance_once` answers `NothingToAdvance` and
            // the classifier gives that condition the hold it already has, where
            // giving it a second reason here is how a report stops being read.
            Ok(None) => return AfterPark::Enter,
            Err(error) => {
                return match classify_store_error("load_after_park", &error, now_ms, &self.config) {
                    Disposition::FailClosed { during, detail } => {
                        AfterPark::StoreUnavailable { during, detail }
                    },
                    Disposition::Release { hold, reason } => AfterPark::Refuse { hold, reason },
                    // Not reachable: every arm of `classify_store_error` either
                    // releases or fails closed. Spelled as a HOLD anyway,
                    // because the alternative default — entering the phase — is
                    // the one answer nothing established.
                    Disposition::KeepGoing => AfterPark::Refuse {
                        hold: Hold::Until(
                            now_ms.saturating_add(millis(self.config.transient_holdoff)),
                        ),
                        reason: HoldReason::StoreRecordRefused,
                    },
                };
            },
        };
        // Still parked. `leave_park` re-armed, or another wait was committed
        // between the exit and this read; either way the next claim is another
        // park exit and not a phase entry, so the cursor does not gate it — the
        // same reason the pre-claim check asks only of an unparked run.
        if committed.state.wait.is_some() {
            return AfterPark::Enter;
        }
        let super::state::ForeignPickup::MidIteration { phase, needs } =
            committed.state.foreign_pickup()
        else {
            return AfterPark::Enter;
        };
        report.mid_iteration_refusals += 1;
        debug!(
            target: "agentic.loop_runner",
            execution = %key,
            ?phase,
            ?needs,
            "[LOOP_RUNNER] the park exit committed, and the cursor it left is not at an \
             iteration boundary; the turn stops here rather than entering that phase from \
             inside the claim"
        );
        AfterPark::Refuse {
            hold: Hold::Until(now_ms.saturating_add(millis(self.config.mid_iteration_holdoff))),
            reason: HoldReason::MidIterationPickup,
        }
    }

    // ── the holdoff set ────────────────────────────────────────────────────

    /// Whether a hold still binds, WITHOUT reading the store.
    ///
    /// `None` for [`Hold::UntilCommittedStateChanges`], which cannot be answered
    /// from memory — an `Option` rather than a `false` default so the one case
    /// that needs a probe cannot be silently dropped by a caller that forgot it.
    fn is_held(&self, key: &ExecutionKey, now_ms: i64) -> Option<bool> {
        match self.held.get(key) {
            None => Some(false),
            Some(held) => match held.hold {
                Hold::UntilThisWorkerRestarts => Some(true),
                Hold::Until(until_ms) => Some(until_ms > now_ms),
                Hold::UntilCommittedStateChanges { .. } => None,
            },
        }
    }

    /// [`Self::is_held`], resolving the one hold that needs the store.
    ///
    /// One `load` — no claim, no lease, no fence, no write. It is taken only for
    /// a key held on its revision, and a phase call is the thing it is being
    /// spent to avoid.
    ///
    /// A probe that cannot answer **keeps the hold**. That is the fail-closed
    /// rule at this grain: an unreadable state is not evidence the run moved,
    /// and guessing that it did costs the re-executed phase the hold exists to
    /// prevent. It does not stop the sweep, because nothing was claimed and the
    /// first real claim against the same substrate will fail closed properly.
    async fn still_held(
        &mut self,
        store: &dyn LoopStateStore,
        key: &ExecutionKey,
        now_ms: i64,
        report: &mut SweepReport,
    ) -> bool {
        if let Some(answer) = self.is_held(key, now_ms) {
            return answer;
        }
        let Some(Held {
            hold: Hold::UntilCommittedStateChanges { revision },
            ..
        }) = self.held.get(key).copied()
        else {
            // `is_held` answered `None`, so the entry was this variant a
            // statement ago. Unreachable behind `&mut self`; treated as held
            // rather than panicking in a worker mid-sweep.
            return true;
        };
        report.holdoff_probes += 1;
        match store.load(key).await {
            Ok(Some(committed)) if committed.revision != revision => {
                debug!(
                    target: "agentic.loop_runner",
                    execution = %key,
                    held_at = %revision,
                    now_at = %committed.revision,
                    "[LOOP_RUNNER] a run held for an outside answer has been committed to since \
                     it paused; the holdoff is released"
                );
                self.held.remove(key);
                false
            },
            Ok(_) => true,
            Err(error) => {
                debug!(
                    target: "agentic.loop_runner",
                    execution = %key,
                    %error,
                    "[LOOP_RUNNER] a holdoff probe could not read this run's committed state; \
                     the hold stands rather than the phase being re-run on a guess"
                );
                true
            },
        }
    }

    fn expire_holdoffs(&mut self, now_ms: i64) {
        self.held.retain(|_, held| match held.hold {
            Hold::UntilThisWorkerRestarts => true,
            Hold::Until(until_ms) => until_ms > now_ms,
            // Not a time, so no clock frees it. Released by the probe in
            // `still_held` when the run's committed revision moves.
            Hold::UntilCommittedStateChanges { .. } => true,
        });
    }

    fn hold(
        &mut self,
        key: &ExecutionKey,
        hold: Hold,
        reason: HoldReason,
        report: &mut SweepReport,
    ) {
        self.held.insert(
            key.clone(),
            Held {
                hold,
                reason,
                since_ms: Utc::now().timestamp_millis(),
            },
        );
        // Evict the entry that would have expired soonest. It is the least
        // harmful one to lose — a timed hold was going to lapse anyway — and it
        // keeps the permanent answers, which are the ones whose loss turns into
        // a re-claim of a run that has already ended.
        //
        // EXCLUDING the entry just inserted, which is not a detail. Without the
        // filter that entry takes part in its own comparison, so a timed hold
        // added to a set already full of permanent ones was the minimum by
        // construction and evicted itself: `hold` returned having done nothing,
        // silently, and the key was re-claimed on the very next sweep. A `hold`
        // that can be a no-op is worse than a full set, because nothing in the
        // report says it happened — the eviction count went up and the caller
        // read that as "some other key was dropped".
        while self.held.len() > self.config.max_held_keys {
            let victim = self
                .held
                .iter()
                .filter(|(candidate, _)| *candidate != key)
                .min_by_key(|(_, held)| match held.hold {
                    Hold::Until(until_ms) => (until_ms, held.since_ms),
                    // No expiry to compare, so both sort with the permanent
                    // answers: a revision hold is released by a probe, and
                    // dropping it puts a paused run's phase back on the
                    // re-execution path this whole hold exists to keep it off.
                    Hold::UntilThisWorkerRestarts | Hold::UntilCommittedStateChanges { .. } => {
                        (i64::MAX, held.since_ms)
                    },
                })
                .map(|(key, _)| key.clone());
            let Some(victim) = victim else { break };
            self.held.remove(&victim);
            report.holdoff_evictions += 1;
            warn!(
                target: "agentic.loop_runner",
                worker = %self.worker,
                execution = %victim,
                capacity = self.config.max_held_keys,
                "[LOOP_RUNNER] the holdoff set is full and an entry was evicted; this key may be \
                 claimed again even though nothing about it has changed"
            );
        }
    }

    fn next_wake_ms(&self) -> Option<i64> {
        self.held
            .values()
            .filter_map(|held| match held.hold {
                Hold::Until(until_ms) => Some(until_ms),
                // Neither has an instant to name. A nap shortened toward a hold
                // that no clock releases would be a shorter idle interval
                // wearing the word "wake".
                Hold::UntilThisWorkerRestarts | Hold::UntilCommittedStateChanges { .. } => None,
            })
            .min()
    }

    /// A key the runner refused before claiming: no claims, no phases, held.
    fn record_refusal(
        &mut self,
        report: &mut SweepReport,
        key: ExecutionKey,
        hold: Hold,
        reason: HoldReason,
    ) {
        self.hold(&key, hold, reason, report);
        report.outcomes.push(KeyOutcome {
            key,
            claims: 0,
            phases_committed: 0,
            end: EndOfTurn::Held { hold, reason },
        });
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    // Production no longer implements an async trait in this file — the fleet
    // seam is synchronous, because handing back a borrow needs no `await` — so
    // the attribute lives with the one fake that still needs it.
    use async_trait::async_trait;

    use super::super::driver_worker::{
        PhaseEntry, PhaseFailure, PhaseReport, Quarantine, ReattachState, RederivedDispatch,
    };
    use super::super::effects::{
        CommittedActRef, EffectId, EffectLedger, EffectLedgerEntry, EffectOutcome, ReconciledEffect,
    };
    use super::super::journal::{JournalAppend, JournalRecord, TerminalKind};
    use super::super::outcome::{Phase, PhaseStep};
    use super::super::state::{
        IterationCheckpoint, LoopCursor, LoopState, RunIdentity, WaitReason,
    };
    use super::super::store::contract::{fresh_state, key};
    use super::super::store::memory::MemoryLoopStateStore;
    use super::super::store::{CommittedLoopState, Lease, ParkedListing, Revision, StoreResult};
    use crate::magician_v2::execution::agentic::types::{
        AgenticContext, AgenticOutcome, EnvironmentState,
    };

    // ====================================================================
    // Fakes
    // ====================================================================

    /// What one phase call is told to do.
    #[derive(Clone, Copy)]
    enum Script {
        Continue,
        /// End the run with a NON-resumable terminal.
        ///
        /// Two consequences, and the second was not true when this fixture was
        /// written. A later claim by key answers `RunAlreadyEnded`, because a
        /// resumable terminal would fall through `verify_journal` by design; and
        /// the STORE stops offering the key at all, because `commit` publishes
        /// an ending marker for exactly this kind of terminal and the scan
        /// withholds it. So a fixture that wants a key the runner refuses and
        /// the scan keeps offering must use [`Self::Pause`], not this.
        EndRun,
        /// End the run at a resumable terminal the STORE can witness.
        ///
        /// `Sleeping` rather than `WaitingForChildren` because it is the cheap
        /// one to build — two fields against a park's child list — and the
        /// classifier reads `TerminalKind`, which both reach the same way.
        Sleep,
        /// End the run at a resumable terminal only a PERSON can answer.
        ///
        /// The whole point of the fixture: this commits `wait = None`, so the
        /// store goes on offering the key and every re-claim re-runs this phase.
        Pause,
        /// PARK on children: end the run AND commit a
        /// [`WaitReason`](super::super::state::WaitReason).
        ///
        /// The one thing `PhaseReport::ends_run` cannot say, and the fixture had
        /// no way to say it either — which is why the two-step park hazard had
        /// no test. `ends_run` inherits `wait: None`; only `executor.rs`'s
        /// `ended_run` computes a wait from the outcome, so the report is built
        /// by hand here exactly as that function builds it.
        Park,
    }

    /// The child a [`Script::Park`] parks on. Named once so the fixture's wake
    /// token and the report's wait cannot drift apart — a token computed from a
    /// different list resolves nothing and the test would measure
    /// `Advanced::Parked` while claiming to measure a park exit.
    const PARKED_ON_CHILD: &str = "child-1";

    /// An `AgenticPauseState`, built through serde rather than by hand.
    ///
    /// The struct is three hundred and fifty lines and all but eight of its
    /// fields carry a serde default; writing it out would put three hundred
    /// lines of irrelevant payload in a test about holdoffs, and every field
    /// added to it later would break this file. `environment_state` is
    /// serialized from the value rather than spelled as JSON so the fixture
    /// cannot disagree with the enum's own wire shape.
    fn pause_state() -> crate::magician_v2::execution::agentic::types::AgenticPauseState {
        serde_json::from_value(serde_json::json!({
            "iteration": 1,
            "goal": "goal",
            "success_criteria": "criteria",
            "environment_state": serde_json::to_value(EnvironmentState::Uninitialized)
                .expect("an environment state must serialize"),
            "action_history_summary": "",
            "paused_at": Utc::now(),
            "max_iterations": 10,
            "max_repeated_actions": 3,
        }))
        .expect("the eight fields with no serde default must be enough to build a pause state")
    }

    fn continuation_checkpoint(
        iteration: usize,
    ) -> Result<super::super::state::IterationContinuationCheckpoint, String> {
        let pause = crate::magician_v2::execution::agentic::types::AgenticPauseState::new(
            iteration,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "test continuation",
            10,
            iteration.saturating_sub(1),
        )
        .with_scope("owner", "default");
        super::super::state::IterationContinuationCheckpoint::try_new(&pause)
    }

    struct ScriptedHost {
        ctx: AgenticContext,
        script: Mutex<Vec<Script>>,
        live_decide_carry: bool,
        live_resolve_carry: bool,
        live_apply_carry: bool,
        /// Shared with the fleet that built this host, because a host is
        /// dropped at the end of every turn and a per-host count would reset.
        ///
        /// Counts PHASE CALLS, which is what a re-claim of a paused run costs —
        /// a paid `Decide`, a journal record, a revision. Counting claims
        /// instead would call a refusal and a re-execution the same thing.
        phase_runs: Arc<AtomicUsize>,
    }

    impl ScriptedHost {
        fn new(script: Vec<Script>, phase_runs: Arc<AtomicUsize>) -> Self {
            let mut ctx = AgenticContext::new("goal", "criteria");
            ctx.max_iterations = 10;
            Self {
                ctx,
                script: Mutex::new(script),
                live_decide_carry: false,
                live_resolve_carry: false,
                live_apply_carry: false,
                phase_runs,
            }
        }
    }

    #[async_trait]
    impl WorkerHost for ScriptedHost {
        fn context(&self) -> &AgenticContext {
            &self.ctx
        }

        fn context_mut(&mut self) -> &mut AgenticContext {
            &mut self.ctx
        }

        fn holds_user_typed_ephemeral_secret(&self) -> bool {
            false
        }

        fn browser_ceiling_for(&self, _agent_id: &str) -> Vec<String> {
            vec!["cdp".to_string()]
        }

        fn adopt_owner(&mut self, identity: &RunIdentity) -> Result<(), String> {
            self.ctx.agent_id = identity.agent_id.clone();
            Ok(())
        }

        fn rederive_dispatch(&self, _effect_id: &EffectId) -> Option<RederivedDispatch> {
            None
        }

        fn reconcile_by_ref(
            &self,
            _reconcile_ref: &CommittedActRef,
            _effect_id: &EffectId,
        ) -> ReconciledEffect {
            ReconciledEffect::SurfaceToUser {
                reason: "this fake never fires an effect".to_string(),
            }
        }

        fn reattach_state(&self, _identity: &RunIdentity, _reattach_ref: &str) -> ReattachState {
            ReattachState::Absent
        }

        async fn conclude_deadline<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
            deadline_at_ms: i64,
        ) -> PhaseReport {
            PhaseReport::ends_run(AgenticOutcome::Failed {
                reason: format!("execution deadline passed at {deadline_at_ms}"),
                last_state: EnvironmentState::Uninitialized,
                iterations_used: entry.iteration.saturating_sub(1),
            })
        }

        fn iteration_checkpoint(&self, iteration: usize) -> IterationCheckpoint {
            IterationCheckpoint {
                iteration,
                history_iterations_len_at_start: 0,
                started_at_ms: 1_000,
            }
        }

        fn continuation_checkpoint(
            &self,
            iteration: usize,
        ) -> Result<super::super::state::IterationContinuationCheckpoint, String> {
            continuation_checkpoint(iteration)
        }

        fn has_live_apply_carry(&self) -> bool {
            self.live_apply_carry
        }

        fn has_live_resolve_carry(&self) -> bool {
            self.live_resolve_carry
        }

        fn has_live_decide_carry(&self) -> bool {
            self.live_decide_carry
        }

        async fn run_phase<'a>(
            &mut self,
            entry: PhaseEntry<'a>,
        ) -> Result<PhaseReport, PhaseFailure> {
            self.phase_runs.fetch_add(1, Ordering::SeqCst);
            let script = {
                let mut script = self.script.lock().unwrap();
                if script.is_empty() {
                    Script::Continue
                } else {
                    script.remove(0)
                }
            };
            let report = match script {
                Script::Continue => PhaseReport::continued(),
                Script::EndRun => PhaseReport::ends_run(AgenticOutcome::Success {
                    completion: crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                    open: Vec::new(),
                    final_state: EnvironmentState::Uninitialized,
                    iterations_used: entry.iteration,
                    artifacts: Vec::new(),
                }),
                // No `wait` on either, which is not an omission in the fixture:
                // `PhaseReport::ends_run` inherits `wait: None` and only a park
                // sets one, so this is exactly the shape a real pause commits.
                Script::Sleep => PhaseReport::ends_run(AgenticOutcome::Sleeping {
                    wake_at: Utc::now() + chrono::Duration::seconds(3_600),
                    paused_state: None,
                    stateless_source_segment: None,
                    placement_continuation: None,
                }),
                Script::Pause => PhaseReport::ends_run(AgenticOutcome::PausedByUser {
                    pause_state: pause_state(),
                    iterations_used: entry.iteration,
                }),
                // Built by hand: the outcome ends the run and the `wait` is what
                // `commit_boundary` writes to `LoopState::wait`. Both halves, or
                // the fixture commits a terminal the store cannot withhold and
                // the park under test never exists.
                Script::Park => PhaseReport {
                    step: PhaseStep::Return(Box::new(AgenticOutcome::WaitingForChildren {
                        child_execution_ids: vec![PARKED_ON_CHILD.to_string()],
                        last_state: EnvironmentState::Uninitialized,
                        iterations_used: entry.iteration,
                        pause_state: None,
                    })),
                    records: Vec::new(),
                    pending: None,
                    resolve_checkpoint: None,
                    steer_consume_receipt: None,
                    terminal_steer_policy: TerminalSteerPolicy::SpeculativePhase,
                    wait: WaitReason::children(vec![PARKED_ON_CHILD.to_string()]),
                },
            };
            // A carry is live only after this same borrowed host produced the
            // preceding phase. A host reconstructed for a later sweep starts
            // cold and must prove the cursor boundary again.
            match entry.phase {
                Phase::Observe => self.live_decide_carry = true,
                Phase::Decide => self.live_resolve_carry = true,
                Phase::Resolve => self.live_apply_carry = true,
                Phase::Prepare | Phase::Apply | Phase::Epilogue => {},
            }
            Ok(report)
        }
    }

    /// A fleet that lends a freshly-built scripted host per key, per turn.
    ///
    /// Per TURN — one `host_for` call — is what the runner actually does. The
    /// host is rebuilt on every call **on purpose**, which is what the deleted
    /// `HostProvider` fixture did and what several tests below measure: the script is consumed
    /// within a turn and restarts on the next sweep, so a fixture can say "this
    /// run ends on its third phase of THIS turn" but not "…of its life". A fleet
    /// that kept its host between turns is now expressible — that is the whole
    /// point of lending rather than composing. It would be a different fixture and would change
    /// what `phase_runs` counts, so it is not what this one does.
    struct ScriptedFleet {
        scripts: std::collections::BTreeMap<ExecutionKey, Vec<Script>>,
        /// The host lent for the current turn. Held on the fleet because
        /// `host_for` returns a BORROW: there is nowhere else for it to live.
        lent: Option<ScriptedHost>,
        /// Phase calls across every host this fleet has ever lent.
        phase_runs: Arc<AtomicUsize>,
        /// Runs handed back through [`HostFleet::run_ended`], in the order they
        /// ended. The fixture's half of the one thing the seam gained: a runner
        /// that dropped a finished run's outcome would make the completion
        /// invisible to the only process that could deliver it.
        ended: Vec<(ExecutionKey, TerminalKind)>,
    }

    impl ScriptedFleet {
        fn new(scripts: Vec<(ExecutionKey, Vec<Script>)>) -> Self {
            Self {
                scripts: scripts.into_iter().collect(),
                lent: None,
                phase_runs: Arc::new(AtomicUsize::new(0)),
                ended: Vec::new(),
            }
        }

        fn phase_runs(&self) -> usize {
            self.phase_runs.load(Ordering::SeqCst)
        }

        fn ended(&self) -> &[(ExecutionKey, TerminalKind)] {
            &self.ended
        }
    }

    impl HostFleet for ScriptedFleet {
        fn host_for(&mut self, key: &ExecutionKey) -> HostForKey<'_> {
            let Some(script) = self.scripts.get(key).cloned() else {
                return HostForKey::NotComposable {
                    detail: "this fixture gave that key no script".to_string(),
                };
            };
            // Cloned above so the map borrow is over before the field write.
            self.lent = Some(ScriptedHost::new(script, Arc::clone(&self.phase_runs)));
            HostForKey::Ready(
                self.lent.as_mut().expect("the host was just assigned") as &mut dyn WorkerHost
            )
        }

        fn run_ended(
            &mut self,
            key: &ExecutionKey,
            _outcome: Box<AgenticOutcome>,
            terminal: TerminalKind,
        ) {
            self.ended.push((key.clone(), terminal));
        }
    }

    /// A fleet holding nothing at all.
    ///
    /// What a process that has set no run up supplies, and the replacement for
    /// the deleted `NoHostInThisProcess` provider. It is a fixture rather than a
    /// production type on purpose: a production "refuses every key" fleet is how
    /// a runner comes to look wired while advancing nothing, which is the state
    /// this change exists to leave.
    struct NoHostsHere;

    impl HostFleet for NoHostsHere {
        fn host_for(&mut self, _key: &ExecutionKey) -> HostForKey<'_> {
            HostForKey::NotComposable {
                detail: "this fleet holds no hosts at all".to_string(),
            }
        }
    }

    /// A real in-memory store with call counters and switchable failures.
    ///
    /// Wraps `MemoryLoopStateStore` rather than faking the whole trait: the
    /// properties under test are about what the runner does with what a REAL
    /// scan offers, and a hand-written listing would answer whatever the test
    /// wished for. That mattered when the runner's whole subject was a store
    /// that could not see a terminal, and it matters more now that it can: the
    /// ending marker is written by `commit`, read by `scan_runnable`, and
    /// compared against the committed watermark, and a fake listing would let
    /// every test in this file agree with a store that had none of that.
    struct CountingStore {
        inner: MemoryLoopStateStore,
        claims: AtomicUsize,
        /// `scan_runnable` calls, which is every runnable scan this fixture
        /// answers: `list_runnable` delegates to it, exactly as `fs` and
        /// `memory` do.
        scans: AtomicUsize,
        /// `list_parked` calls.
        ///
        /// The reconciler's first act, and the only evidence of a pass that is
        /// independent of the tally the code under test writes. A test asserting
        /// `reconciled.passes` alone would still pass if `reconcile_once`
        /// incremented the counter and never called the store.
        parks: AtomicUsize,
        fail_claim: bool,
        /// Every runnable scan refuses, on every page.
        fail_scan: bool,
        /// The scan answers this many pages and refuses every page after them.
        ///
        /// Separate from [`Self::fail_scan`] because the two produce reports
        /// that differ in the one place a doc and the code can disagree about:
        /// a first-page refusal leaves `offered` at zero, while a LATER-page
        /// refusal leaves every page before it counted — and `scan_end` is
        /// `None` for both. Without this switch nothing could drive the second
        /// shape, and `ScanEnd`'s docs described only the first.
        fail_scan_after_pages: Option<usize>,
        /// `list_parked` refuses.
        ///
        /// Separate from [`Self::fail_scan`] because the two are supposed to
        /// have OPPOSITE consequences: a scan the store cannot answer stops the
        /// runner after `max_consecutive_store_failures`, while a park listing
        /// it cannot answer must not stop it at all.
        fail_park_list: bool,
        /// The first `list_parked` answers `incomplete`; every later one does
        /// not. What a store with more parks than `scan_limit` does on one pass
        /// and not on the next.
        park_incomplete_once: bool,
    }

    impl CountingStore {
        fn new() -> Self {
            Self {
                inner: MemoryLoopStateStore::new(),
                claims: AtomicUsize::new(0),
                scans: AtomicUsize::new(0),
                parks: AtomicUsize::new(0),
                fail_claim: false,
                fail_scan: false,
                fail_scan_after_pages: None,
                fail_park_list: false,
                park_incomplete_once: false,
            }
        }

        fn park_listings(&self) -> usize {
            self.parks.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LoopStateStore for CountingStore {
        async fn load(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
            self.inner.load(key).await
        }

        async fn commit(
            &self,
            key: &ExecutionKey,
            state: &LoopState,
            expected: Revision,
        ) -> StoreResult<Revision> {
            self.inner.commit(key, state, expected).await
        }

        async fn append_journal(
            &self,
            key: &ExecutionKey,
            appends: &[JournalAppend],
        ) -> StoreResult<u64> {
            self.inner.append_journal(key, appends).await
        }

        async fn read_journal(
            &self,
            key: &ExecutionKey,
            from_seq: u64,
        ) -> StoreResult<Vec<JournalRecord>> {
            self.inner.read_journal(key, from_seq).await
        }

        async fn record_effect_intent(
            &self,
            key: &ExecutionKey,
            entry: &EffectLedgerEntry,
        ) -> StoreResult<()> {
            self.inner.record_effect_intent(key, entry).await
        }

        async fn record_effect_outcome(
            &self,
            key: &ExecutionKey,
            effect_id: &EffectId,
            outcome: EffectOutcome,
        ) -> StoreResult<()> {
            self.inner
                .record_effect_outcome(key, effect_id, outcome)
                .await
        }

        async fn load_effects(&self, key: &ExecutionKey) -> StoreResult<EffectLedger> {
            self.inner.load_effects(key).await
        }

        async fn claim(
            &self,
            key: &ExecutionKey,
            worker: &WorkerId,
            ttl: Duration,
        ) -> StoreResult<Lease> {
            self.claims.fetch_add(1, Ordering::SeqCst);
            if self.fail_claim {
                return Err(StoreError::Unavailable {
                    detail: "the substrate is down".to_string(),
                });
            }
            self.inner.claim(key, worker, ttl).await
        }

        async fn renew(&self, lease: &Lease, ttl: Duration) -> StoreResult<Lease> {
            self.inner.renew(lease, ttl).await
        }

        async fn release(&self, lease: Lease) -> StoreResult<()> {
            self.inner.release(lease).await
        }

        async fn resolve_wake(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
            resolution_id: &str,
        ) -> StoreResult<()> {
            self.inner
                .resolve_wake(key, wake_token, resolution_id)
                .await
        }

        async fn wake_resolutions(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
        ) -> StoreResult<Vec<String>> {
            self.inner.wake_resolutions(key, wake_token).await
        }

        async fn consume_wake(
            &self,
            key: &ExecutionKey,
            wake_token: &str,
            resolution_ids: &[String],
        ) -> StoreResult<usize> {
            self.inner
                .consume_wake(key, wake_token, resolution_ids)
                .await
        }

        /// Delegated to [`Self::scan_runnable`], exactly as both real stores
        /// write it.
        ///
        /// A decorator that forwards one of the two entry points and not the
        /// other is not the store it is wrapping: the runner pages, so a
        /// `scan_runnable` left on the trait's refusing default would fail every
        /// sweep against this fixture — and a `list_runnable` that reached the
        /// inner store directly would leave the counter and the failure switch
        /// below answering for only one of the two questions.
        async fn list_runnable(
            &self,
            worker: &WorkerId,
            limit: usize,
        ) -> StoreResult<Vec<ExecutionKey>> {
            Ok(self.scan_runnable(worker, limit, None).await?.keys)
        }

        async fn scan_runnable(
            &self,
            worker: &WorkerId,
            limit: usize,
            after: Option<&ScanCursor>,
        ) -> StoreResult<RunnableScan> {
            // The count BEFORE this call, so `fail_scan_after_pages: Some(2)`
            // means "answer pages one and two, refuse page three". Counted
            // across the whole fixture rather than per sweep, which the one
            // test using it takes exactly one sweep for.
            let answered = self.scans.fetch_add(1, Ordering::SeqCst);
            if self.fail_scan
                || self
                    .fail_scan_after_pages
                    .is_some_and(|pages| answered >= pages)
            {
                return Err(StoreError::Unavailable {
                    detail: "the scan cannot read its root".to_string(),
                });
            }
            self.inner.scan_runnable(worker, limit, after).await
        }

        async fn list_parked(&self, limit: usize) -> StoreResult<ParkedListing> {
            let call = self.parks.fetch_add(1, Ordering::SeqCst);
            if self.fail_park_list {
                return Err(StoreError::Unavailable {
                    detail: "the park listing cannot read its root".to_string(),
                });
            }
            let mut listing = self.inner.list_parked(limit).await?;
            if self.park_incomplete_once && call == 0 {
                listing.incomplete = true;
            }
            Ok(listing)
        }
    }

    fn worker() -> WorkerId {
        WorkerId::new("runner-under-test")
    }

    fn runner(config: RunnerConfig) -> WorkerRunner {
        WorkerRunner::new(worker(), config)
    }

    /// A config whose every holdoff is a DIFFERENT, recognisable interval.
    ///
    /// Distinct on purpose: a classifier that reached for the wrong field would
    /// still produce "some hold in the future" under a config where the
    /// intervals matched, and every assertion about which interval was used
    /// would pass on the wrong one.
    fn distinct_config() -> RunnerConfig {
        RunnerConfig {
            failure_holdoff: Duration::from_millis(3_000),
            transient_holdoff: Duration::from_millis(4_000),
            host_unavailable_holdoff: Duration::from_millis(5_000),
            resumable_terminal_holdoff: Duration::from_millis(6_000),
            // Not read by `classify` — the mid-iteration refusal is taken in the
            // sweep, before any `Advanced` exists — but set here so this
            // function's name stays true of every holdoff the config carries.
            mid_iteration_holdoff: Duration::from_millis(7_000),
            ..RunnerConfig::default()
        }
    }

    async fn seed(store: &CountingStore, key: &ExecutionKey) {
        store
            .commit(key, &fresh_state(key), Revision::INITIAL)
            .await
            .expect("the fixture's first commit");
    }

    /// Commit a state whose cursor sits at `phase` of an iteration already
    /// under way.
    ///
    /// Through the store's own `commit` rather than by reaching into the
    /// fixture, so what the sweep loads is a state the store really wrote and
    /// really read back.
    ///
    /// It does **not** go through `LoopState::for_commit`, and an earlier
    /// version of this comment claimed it did. No store calls that function:
    /// `store/memory.rs::commit` validates `state.pending` and the journal
    /// watermark and stores the clone, while `for_commit` is called from the
    /// driver's own commit path (`driver_worker::project`) and from
    /// `journal.rs`. So placement pinning, the portable-transport ceiling and
    /// work-budget monotonicity are NOT asserted over what this writes.
    /// `fresh_state` is the base for that reason rather than in spite of it —
    /// it is a state the projection has already accepted elsewhere, and the
    /// only field this touches is the cursor, which the projection does not
    /// read.
    async fn seed_at(
        store: &CountingStore,
        key: &ExecutionKey,
        phase: super::super::outcome::Phase,
    ) {
        let mut state = fresh_state(key);
        state.cursor = LoopCursor {
            iteration: 3,
            phase,
        };
        store
            .commit(key, &state, Revision::INITIAL)
            .await
            .expect("the fixture's first commit");
    }

    // ====================================================================
    // The property the holdoff set exists for
    // ====================================================================

    #[tokio::test]
    async fn a_finished_run_is_withheld_by_the_store_and_held_by_the_runner_too() {
        // The finding this module was built around, and the half of it the
        // stores have since closed. Asserted in both halves, and the first one
        // is now the OPPOSITE of what this test asserted when it was written.
        //
        // Half one is a claim about the STORE. `commit` publishes the run's
        // ending beside its state out of the journal prefix the watermark
        // vouches for — `slot.ended` in `store/memory.rs`, `ended.json` in
        // `store/fs.rs` — and the scan withholds any key whose published ending
        // satisfies `ended.seq == state.journal_seq`. A finished run is
        // therefore NOT offered. If this assertion starts failing, the store has
        // regressed and the paragraphs in this module's header that describe the
        // marker are wrong.
        //
        // Half two is the runner's answer, and it is why the holdoff entry is
        // kept rather than deleted along with the hazard: the ended key stays
        // held for this worker's life, which is what a runner meets a decorated
        // or regressed store with. The LIVE key beside it is still advanced in
        // the same later sweep — the half that stops this passing on a runner
        // that simply gave up.
        let store = CountingStore::new();
        let ended = key("aaa-ends");
        let live = key("bbb-continues");
        seed(&store, &ended).await;
        seed(&store, &live).await;
        let mut fleet = ScriptedFleet::new(vec![
            (ended.clone(), vec![Script::EndRun]),
            (live.clone(), vec![Script::Continue]),
        ]);
        let cancel = CancellationToken::new();
        let mut runner = runner(RunnerConfig {
            // One claim per key per sweep, so each sweep's arithmetic is a
            // statement about which keys were picked up rather than about how
            // far each got.
            max_claims_per_execution: 1,
            ..RunnerConfig::default()
        });

        let first = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(first.offered, 2);
        assert_eq!(first.attempted, 2);
        assert!(
            matches!(
                first
                    .outcomes
                    .iter()
                    .find(|outcome| outcome.key == ended)
                    .map(|outcome| &outcome.end),
                Some(EndOfTurn::Held {
                    hold: Hold::Until(_),
                    reason: HoldReason::RunEnded,
                })
            ),
            "the phase that ended the run must hold its key, not release it back into the scan. \
             This is the ONE sweep in which `HoldReason::RunEnded` is reachable against THIS \
             store — `MemoryLoopStateStore` assigns `slot.ended` infallibly, so it withholds the \
             key from every scan after this one — and it matters because no store filter can \
             stop a second claim inside the sweep that is ending the run"
        );

        // HALF ONE. The store withholds the finished run and goes on offering
        // the live one. Both assertions, because a scan that answered an empty
        // listing for any unrelated reason would satisfy the first alone — and
        // an empty listing would also make half two pass.
        let offered_now = store
            .list_runnable(&worker(), 16)
            .await
            .expect("the scan answers");
        assert!(
            !offered_now.contains(&ended),
            "the store must withhold a run whose authoritative journal ends at a non-resumable \
             terminal. A store that offers it again puts the starvation this module documents \
             back, and makes the holdoff set load-bearing rather than belt-and-braces"
        );
        assert!(
            offered_now.contains(&live),
            "and it must withhold ONLY that: a scan answering nothing at all satisfies the \
             assertion above for the wrong reason, and would satisfy every count below too"
        );

        // HALF TWO. The scan is what changed; the runner still holds the key.
        let second = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(
            second.offered, 1,
            "the store no longer offers the finished run, so it is not in the scan at all"
        );
        assert_eq!(
            second.skipped_held, 0,
            "and it never reached the holdoff check: `skipped_held` counts keys the SCAN offered \
             and this runner refused, so a store that withheld the key first leaves it at zero. \
             A non-zero value here means the store regressed"
        );
        assert_eq!(
            second.outcomes.len(),
            1,
            "exactly one key took a turn in the second sweep"
        );
        assert_eq!(
            second.outcomes[0].key, live,
            "the key that took the second sweep's turn must be the LIVE one; a runner that held \
             everything would also report zero skipped keys and one claim"
        );
        // NOT a claim, and this assertion asserted one until 2026-08-28.
        //
        // `max_claims_per_execution: 1` above means the first sweep advanced the
        // live key by exactly ONE phase, so its committed cursor stops mid
        // iteration — and `foreign_pickup` refuses exactly that. The values the
        // next phase reads were produced in the previous holder's memory
        // (`IterationCarry`), so a pickup there would find them freshly
        // defaulted. The guard is right and the expectation was written before
        // it existed.
        //
        // What the original assertion was FOR is kept, and it is the reason this
        // is not simply deleted: a runner that had given up would refuse both
        // keys the same way. These two are refused for different reasons, and
        // the difference is load-bearing rather than cosmetic — the ended key is
        // held for this worker's LIFE, the live key only until a holdoff window
        // passes, because another holder can advance it to a boundary and a
        // permanent hold would refuse that pickup forever.
        assert_eq!(
            second.claims, 0,
            "the live key is refused before it is claimed, not claimed"
        );
        assert!(
            matches!(
                second.outcomes[0].end,
                EndOfTurn::Held {
                    hold: Hold::Until(_),
                    reason: HoldReason::MidIterationPickup,
                }
            ),
            "the live key must be refused as a mid-iteration pickup on a TIMED hold; a permanent \
             hold here would strand a run another holder could finish. Got {:?}",
            second.outcomes[0].end
        );
        assert_eq!(
            second.mid_iteration_refusals, 1,
            "and the refusal must be counted, or an operator watching a run that never advances \
             has nothing that names why"
        );
        assert_eq!(
            runner.holdoff(&ended).map(|(_, reason)| reason),
            Some(HoldReason::RunEnded),
            "the entry survives the sweep that made it. Nothing offers the key any more, so \
             nothing reads this entry either — it is the runner's answer for a store that stops \
             filtering, and dropping it would remove the only thing between such a store and a \
             scan-rate re-claim"
        );
        // Also NOT `None`, and for the same reason as the claim count above: the
        // live key stopped mid iteration, so it carries a holdoff entry too.
        //
        // Asserting the PAIR is the point. A runner that had given up would hold
        // both keys the same way, and both of the assertions this replaced —
        // `claims == 1` and `holdoff(&live) == None` — were written to catch
        // exactly that. They no longer can, because the live key is legitimately
        // held. What still discriminates is the KIND of hold: the ended run is
        // held for this worker's life because nothing will ever make it
        // claimable again, while the live one is held only until a window passes,
        // because another holder can advance it to an iteration boundary and a
        // permanent hold would refuse that pickup forever.
        //
        // A runner that gave up would fail this: it would have to invent a
        // reason to hold a live key permanently.
        assert!(
            matches!(
                runner.holdoff(&live).map(|(hold, reason)| (hold, reason)),
                Some((Hold::Until(_), HoldReason::MidIterationPickup))
            ),
            "the live key must carry a TIMED mid-iteration hold, not a worker-lifetime one; got \
             {:?}",
            runner.holdoff(&live)
        );
        assert!(
            matches!(
                runner.holdoff(&ended).map(|(hold, _)| hold),
                Some(Hold::Until(_))
            ),
            "the store withholds an ended run, so its local retry guard is timed rather than \
             pinning projection debt to one worker forever"
        );
    }

    #[tokio::test]
    async fn a_held_key_does_not_keep_its_slot_in_the_scan_prefix() {
        // The OTHER half of the same finding, and the half a holdoff set alone
        // does not fix. Not re-claiming a held key stops the hot re-claim; it
        // does not stop the starvation, because the key is still in the scan.
        //
        // `list_runnable` takes a limit and no offset. Both implementations
        // return the first `limit` claimable keys in a fixed order and stop, and
        // a held key is still claimable, unleased, unparked and inside every
        // timer — so it keeps its slot in that prefix for the life of the
        // process, and the holdoff set is process-local, so nothing the runner
        // remembers moves where the walk stops.
        //
        // # THE POISON IS A PAUSE, and it used to be a finished run
        //
        // A finished run cannot express this any more: both stores publish an
        // ending marker and withhold the key from the very next scan, so it
        // stops occupying a slot without any help from the runner, and a fixture
        // built on one would measure the store rather than the widening.
        //
        // A pause is the population that still does it, and it is not a
        // contrivance — it is the shape this hazard actually has now.
        // `PhaseReport::ends_run` inherits `wait: None`, `TerminalKind::
        // PausedByUser` IS resumable so no ending is published, and `WaitReason`
        // has no variant for a human. So nothing in the committed state and
        // nothing in the journal marker withholds the key: it is claimable,
        // unleased, unparked and inside every timer, held by this runner on its
        // committed revision, and in the scan's prefix until a person answers.
        //
        // `batch = 1` is what makes this fixture able to express the defect at
        // all. The default 16 cannot: the whole two-key store fits inside one
        // scan, so the live key is offered no matter what the limit does. That
        // is why the test above — same two keys, `batch = 16` — passes on a
        // runner with this bug.
        //
        // The keys are named so the PAUSED one sorts first. `MemoryLoopStateStore`
        // walks an ordered map, so this puts it in front of the live one by
        // construction rather than by hoping about iteration order.
        let store = CountingStore::new();
        let paused = key("aaa-pauses");
        let live = key("bbb-continues");
        seed(&store, &paused).await;
        seed(&store, &live).await;
        let mut fleet = ScriptedFleet::new(vec![
            (paused.clone(), vec![Script::Pause]),
            (live.clone(), vec![Script::Continue]),
        ]);
        let cancel = CancellationToken::new();
        let mut runner = runner(RunnerConfig {
            batch: 1,
            max_claims_per_execution: 1,
            ..RunnerConfig::default()
        });

        let first = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(
            first.scan_limit, 1,
            "nothing is held yet, so the scan is `batch`"
        );
        assert_eq!(first.offered, 1);
        assert_eq!(
            first.outcomes[0].key, paused,
            "the fixture depends on the paused key sorting first; if the store's order changed, \
             this test is no longer about a held key blocking a live one"
        );
        assert_eq!(
            first.scan_pages, 1,
            "and the first sweep must stop after ONE page — the pause is an attempt, so it fills \
             `batch = 1`. A first sweep that paged would reach the live key here and the second \
             sweep would have nothing left to say about the widening"
        );

        // The premise, and it is the half the store used to supply for free: a
        // pause is invisible to the scan, so the held key really is still in the
        // prefix. Without this the widening below could be measuring a store
        // that had simply stopped offering the key.
        assert!(
            store
                .list_runnable(&worker(), 16)
                .await
                .expect("the scan answers")
                .contains(&paused),
            "a resumable terminal that commits no wait publishes no ending and sets no \
             `WaitReason`, so the scan must still offer it. If this fails the store has learned \
             to withhold pauses and this fixture no longer contains a prefix to widen past"
        );

        let second = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(
            second.scan_limit, 2,
            "the scan must ask for `batch` PLUS what is held; asking for `batch` alone lets a \
             prefix of held keys hide every live run behind it, permanently and silently"
        );
        assert_eq!(second.offered, 2);
        assert_eq!(second.skipped_held, 1);
        assert_eq!(
            second.outcomes.len(),
            1,
            "the paused key is skipped and the live one takes a turn"
        );
        assert_eq!(
            second.outcomes[0].key, live,
            "the LIVE key must get the second sweep's turn. With a fixed scan limit it is never \
             offered at all: `offered = 1, skipped_held = 1, attempted = 0`, for the rest of the \
             process's life, while the report looks like a quiet queue"
        );
        assert_eq!(second.claims, 1);
        assert_eq!(
            runner.holdoff(&paused).map(|(_, reason)| reason),
            Some(HoldReason::AwaitingAnOutsideAnswer),
            "the widened scan must not be a runner that stopped holding; the paused key is still \
             held, it is simply no longer occupying the only slot"
        );
        assert_eq!(
            second.scan_pages, 1,
            "and it must do it in ONE page. The widening is the answer for a prefix this runner \
             has already met and remembered; paging is the answer for one it has not. A runner \
             that had NOT widened would reach the live key too — on page two — and every count \
             above would still hold: `offered = 2` summed over two pages of one, \
             `skipped_held = 1`, one outcome on the live key. This is the only assertion here \
             that tells the two apart"
        );
    }

    // ====================================================================
    // The half the holdoff set cannot reach: a prefix this worker has
    // never met
    // ====================================================================

    #[tokio::test]
    async fn a_prefix_of_unhostable_keys_does_not_starve_a_live_run_in_one_sweep() {
        // The starvation the cursor exists for, and the one the widened first
        // page CANNOT fix.
        //
        // The widening only escapes keys this runner has already refused and put
        // in its holdoff set — which is empty on the first sweep of a fresh
        // runner. A one-sweep caller with a one-key fleet therefore sees only
        // that first-sweep behavior, and the widening never gets a turn at all.
        //
        // Two refused keys sort in front of the live one by name, because
        // `MemoryLoopStateStore` walks an ordered map — by construction rather
        // than by hoping about iteration order. `batch = 1` is what makes one
        // page hold one key, which is what makes the prefix a prefix.
        let store = CountingStore::new();
        let first_refused = key("aaa-no-host");
        let second_refused = key("bbb-no-host");
        let live = key("ccc-continues");
        seed(&store, &first_refused).await;
        seed(&store, &second_refused).await;
        seed(&store, &live).await;

        // Only the live key has a script, so the fleet answers `NotComposable`
        // for the other two — the same permanent refusal a real narrow fleet
        // gives every key but its own.
        let mut fleet = ScriptedFleet::new(vec![(live.clone(), vec![Script::Continue])]);
        let mut runner = runner(RunnerConfig {
            batch: 1,
            max_claims_per_execution: 1,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.scan_pages, 3,
            "the sweep must ask for the next page while its batch is unmet; with `batch = 1` and \
             two refused keys in front, the live key is on the third page and nothing else \
             reaches it in this sweep"
        );
        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            3,
            "counted at the STORE as well as on the report: a report field the runner increments \
             without calling the store would satisfy the assertion above on its own"
        );
        assert_eq!(report.offered, 3);
        assert_eq!(
            report
                .outcomes
                .iter()
                .map(|outcome| outcome.key.clone())
                .collect::<Vec<_>>(),
            vec![first_refused.clone(), second_refused.clone(), live.clone()],
            "each key exactly once, in walk order. A runner that fed back `None` — or the cursor \
             it arrived with — instead of the page's own `resume` would re-offer the head of the \
             walk on every page: three pages, three outcomes, and the live key never reached. \
             That failure LOOKS like this one's fix, which is why the assertion is on the keys \
             and not on the count"
        );
        assert_eq!(report.hosts_not_composable, 2);
        assert_eq!(
            report.attempted, 1,
            "the live key was claimed. Without paging this is zero, for the life of the process"
        );
        assert_eq!(report.claims, 1);
        assert_eq!(fleet.phase_runs(), 1, "and a phase actually ran on it");
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::WalkedTheWholeStore),
            "the third page was the end of the store, and that is a stronger statement than \
             `BatchFilled`: it says there is nothing behind this sweep"
        );
    }

    #[tokio::test]
    async fn a_sweep_that_fills_its_batch_on_the_first_page_takes_one_scan() {
        // The other direction, and the reason the paging loop counts
        // `attempted` rather than "keep going until the walk ends".
        //
        // The overwhelmingly common sweep finds its work on page one, and a
        // second round trip there is a cost paid by every healthy worker for a
        // pathology it does not have. This test is meaningless on its own — a
        // runner that never paged would also take one scan — and it is paired
        // with the test above, which fails on exactly that runner. One pins the
        // floor, the other the ceiling.
        //
        // The store holds MORE offerable work than the batch, so `resume` is
        // `Some` and stopping is a decision rather than the walk running out.
        let store = CountingStore::new();
        let first = key("aaa-continues");
        let second = key("bbb-continues");
        seed(&store, &first).await;
        seed(&store, &second).await;

        let mut fleet = ScriptedFleet::new(vec![
            (first.clone(), vec![Script::Continue]),
            (second.clone(), vec![Script::Continue]),
        ]);
        let mut runner = runner(RunnerConfig {
            batch: 1,
            max_claims_per_execution: 1,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            1,
            "one page filled the batch, so the store is asked once. A second call here is a \
             second directory walk on every sweep of every healthy worker"
        );
        assert_eq!(report.scan_pages, 1);
        assert_eq!(
            report.offered, 1,
            "and the key behind the batch is not even offered; it is the next sweep's work"
        );
        assert_eq!(report.attempted, 1);
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::BatchFilled),
            "`BatchFilled` and not `WalkedTheWholeStore`: the store still holds an offerable key \
             this sweep chose not to look at, and a report claiming the walk was finished would \
             be a false statement about coverage"
        );
    }

    #[tokio::test]
    async fn a_spent_claim_budget_does_not_buy_a_page_it_cannot_use() {
        // `max_claims_per_sweep` is asked at the top of the next key's turn and
        // before a further claim of the key being worked. Both need a NEXT thing
        // to be about, and a budget spent on the LAST key of a page has neither:
        // the key loop simply runs out with `report.stopped` still `None`, the
        // paging guard sees a walk with more in it and a batch unmet, and the
        // sweep buys one more page it abandons on that page's first key.
        //
        // One extra `scan_runnable` per sweep — on `store/fs.rs` a directory
        // walk of every level — on a caller's critical path. At one claim per
        // sweep the condition is only *"the key it claimed sorted last on its
        // page"*.
        //
        // THREE scenarios, and the second and third are not padding: the obvious
        // fix — stop paging whenever the budget is spent — downgrades the report
        // of a sweep that stopped for a better reason, turning `BatchFilled` and
        // `WalkedTheWholeStore` into `SweepEnded` and losing what each of those
        // says about coverage. A fix with that defect passes scenario one alone.

        // ── ONE: the budget is spent on the last key of a page ──────────────
        //
        // `aaa` has no script so the fleet refuses it; `bbb` takes the sweep's
        // one claim and is last on a page of two; `ccc` is behind the page and
        // is what an over-eager runner pays a scan to look at.
        let store = CountingStore::new();
        let unhostable = key("aaa-no-host");
        let claimed = key("bbb-continues");
        let behind = key("ccc-continues");
        seed(&store, &unhostable).await;
        seed(&store, &claimed).await;
        seed(&store, &behind).await;
        let mut fleet = ScriptedFleet::new(vec![
            (claimed.clone(), vec![Script::Continue]),
            (behind.clone(), vec![Script::Continue]),
        ]);
        let mut spends_its_budget = runner(RunnerConfig {
            // Two, so one attempt does NOT fill the batch — otherwise
            // `BatchFilled` would stop the paging and this scenario would pass
            // on a runner with the defect.
            batch: 2,
            max_claims_per_execution: 1,
            max_claims_per_sweep: 1,
            ..RunnerConfig::default()
        });

        let report = spends_its_budget
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            1,
            "counted at the STORE, which is where a page costs something. Without this the sweep \
             takes two: `scan_pages = 2`, `offered = 3`, and the third key is offered to a sweep \
             that has no claim left to spend on it"
        );
        assert_eq!(report.scan_pages, 1);
        assert_eq!(
            report.offered, 2,
            "and the key behind the page is not offered at all. `offered` counting a page whose \
             keys were never looked at is a second, quieter cost of the same bug"
        );
        assert_eq!(report.claims, 1);
        assert_eq!(report.attempted, 1);
        assert_eq!(
            report.stopped,
            Some(SweepStop::ClaimBudget),
            "the sweep says WHY it stopped short of a walk that had more in it; stopping with \
             `stopped: None` would report a sweep that simply ran out of keys"
        );
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::SweepEnded),
            "which is the same answer the extra page produced, one page later"
        );

        // ── TWO: it must not downgrade a sweep that FILLED ITS BATCH ────────
        //
        // Same spent budget, same walk with more behind it. The only difference
        // is that one attempt is now the whole batch, and `BatchFilled` — *the
        // sweep had what it wanted* — is a stronger statement than `SweepEnded`.
        let filled = CountingStore::new();
        let taken = key("aaa-continues");
        let next_time = key("bbb-continues");
        seed(&filled, &taken).await;
        seed(&filled, &next_time).await;
        let mut small_fleet = ScriptedFleet::new(vec![
            (taken.clone(), vec![Script::Continue]),
            (next_time.clone(), vec![Script::Continue]),
        ]);
        let mut fills_its_batch = runner(RunnerConfig {
            batch: 1,
            max_claims_per_execution: 1,
            max_claims_per_sweep: 1,
            ..RunnerConfig::default()
        });

        let report = fills_its_batch
            .sweep(&filled, &mut small_fleet, &CancellationToken::new())
            .await;

        assert_eq!(filled.scans.load(Ordering::SeqCst), 1);
        assert_eq!(report.claims, 1, "the budget really is spent");
        assert_eq!(
            report.stopped, None,
            "`SweepStop` is documented as why a sweep stopped BEFORE it ran out of keys, and \
             this one stopped because it had what it came for"
        );
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::BatchFilled),
            "not `SweepEnded`. A fix that set `stopped` on any spent budget would report the \
             weaker answer here, and an operator would stop being able to tell a satisfied sweep \
             from a cut one"
        );

        // ── THREE: nor a sweep that WALKED THE WHOLE STORE ──────────────────
        //
        // The strongest statement of the three — *nothing is behind this sweep*
        // — and the one it would be worst to lose. Two keys inside a page of
        // two, so `resume` is `None`.
        let whole = CountingStore::new();
        let refused = key("aaa-no-host");
        let worked = key("bbb-continues");
        seed(&whole, &refused).await;
        seed(&whole, &worked).await;
        let mut one_key_fleet = ScriptedFleet::new(vec![(worked.clone(), vec![Script::Continue])]);
        let mut walks_the_store = runner(RunnerConfig {
            batch: 2,
            max_claims_per_execution: 1,
            max_claims_per_sweep: 1,
            ..RunnerConfig::default()
        });

        let report = walks_the_store
            .sweep(&whole, &mut one_key_fleet, &CancellationToken::new())
            .await;

        assert_eq!(whole.scans.load(Ordering::SeqCst), 1);
        assert_eq!(report.claims, 1, "the budget is spent here too");
        assert_eq!(report.attempted, 1, "and the batch of two is NOT filled");
        assert_eq!(report.stopped, None);
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::WalkedTheWholeStore),
            "the walk ended inside the page, so there is nothing behind this sweep — and saying \
             `SweepEnded` instead would tell an operator the store might not have been covered"
        );
    }

    #[tokio::test]
    async fn the_page_ceiling_stops_the_sweep_and_says_the_tail_was_not_examined() {
        // A loop that pages until it fills its batch walks the WHOLE store when
        // every key is refused, which is the ordinary state of a narrow fleet.
        // The ceiling is what stops that, and the report is what stops the
        // ceiling being a silent hole: a sweep that gives up early is better
        // than one that walks a million directories, but only if it says so.
        //
        // `NoHostsHere` refuses every key, so the batch is never met and the
        // ONLY thing that can stop the paging is the ceiling.
        let store = CountingStore::new();
        for id in ["aaa", "bbb", "ccc", "ddd"] {
            seed(&store, &key(id)).await;
        }

        let mut runner = runner(RunnerConfig {
            batch: 1,
            max_scan_pages: 2,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut NoHostsHere, &CancellationToken::new())
            .await;

        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            2,
            "the ceiling is a ceiling on calls to the STORE, which is where the walk costs \
             something; a runner that stopped counting pages but kept scanning would satisfy a \
             report-only assertion"
        );
        assert_eq!(report.scan_pages, 2);
        assert_eq!(
            report.offered, 2,
            "two of the four keys were looked at; the other two were not offered to anything"
        );
        assert_eq!(
            report.scan_end,
            Some(ScanEnd::PageCap),
            "and the report SAYS the tail was not examined. Reporting `BatchFilled` here — the \
             batch is, after all, as full as this sweep is going to make it — would turn a \
             coverage hole into a normal completion, which is the shape of every bug this \
             module is about"
        );
        assert_eq!(
            report.attempted, 0,
            "nothing was hosted, which is what made the batch unmeetable in the first place"
        );
    }

    #[tokio::test]
    async fn a_bounded_sweep_reaches_a_caller_that_only_sees_the_run_report() {
        // `SweepReport::scan_end` does not reach a caller that invokes `run` and
        // reads only a `RunReport`. Without a field there, a pass that gave up at its page
        // ceiling is indistinguishable from one that found nothing — which is
        // the confusion this module exists to remove, reintroduced one layer up.
        //
        // BOTH halves are here on purpose. A `scans_incomplete` hardcoded true
        // passes the first and fails the second; one never assigned passes the
        // second and fails the first.
        let store = CountingStore::new();
        for id in ["aaa", "bbb", "ccc", "ddd"] {
            seed(&store, &key(id)).await;
        }
        let bounded = RunnerConfig {
            batch: 1,
            max_scan_pages: 2,
            // Off, so this test is about the scan and not about a park walk.
            reconcile_every: None,
            ..RunnerConfig::default()
        };

        // Named rather than `runner`, because a second `let runner = runner(..)`
        // in one block resolves the initializer against the FIRST binding —
        // which is a `WorkerRunner`, not a function.
        let mut over_a_big_store = runner(bounded.clone());
        let report = over_a_big_store
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(1))
            .await;
        assert_eq!(report.sweeps, 1);
        assert!(
            report.scans_incomplete,
            "the sweep stopped at its page ceiling with two keys behind it, and the caller that \
             can only see this report has to be told"
        );

        // The same runner shape against a store its pages DO cover. Two keys and
        // two pages, so the walk genuinely ends inside the ceiling.
        let small = CountingStore::new();
        for id in ["aaa", "bbb"] {
            seed(&small, &key(id)).await;
        }
        let mut over_a_small_one = runner(bounded);
        let report = over_a_small_one
            .run(&small, &mut NoHostsHere, &CancellationToken::new(), Some(1))
            .await;
        assert_eq!(report.sweeps, 1);
        assert!(
            !report.scans_incomplete,
            "the walk ended inside the ceiling, so nothing was left unexamined and the flag must \
             stay down; a flag that is always up is a flag an operator learns to ignore"
        );
    }

    // ====================================================================
    // The holdoff that is neither a time nor forever
    // ====================================================================

    #[tokio::test]
    async fn a_run_that_stopped_for_a_person_is_not_re_run_until_something_outside_commits() {
        // A pause is a resumable terminal that commits NO wait —
        // `PhaseReport::ends_run` inherits `wait: None`, only a park sets one,
        // and `WaitReason` has no variant for a human. So `list_runnable` has
        // nothing to withhold the key by, and `advance_under_lease` lets a
        // resumable terminal fall through and RE-RUNS the phase that ended.
        //
        // That makes a re-check interval a paid poll of a person: one `Decide`,
        // one journal record, one revision and more work budget, per interval,
        // for as long as the human takes. This is the measurement that a
        // re-claim does not happen — and, in the same test, that the key is not
        // stranded either, because a permanent hold would defeat the resume.
        let store = CountingStore::new();
        let paused = key("aaa-pauses");
        seed(&store, &paused).await;
        let mut fleet = ScriptedFleet::new(vec![(paused.clone(), vec![Script::Pause])]);
        let cancel = CancellationToken::new();
        let mut runner = runner(RunnerConfig::default());

        let first = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(first.attempted, 1);
        assert_eq!(fleet.phase_runs(), 1, "the phase that paused ran once");
        let paused_at = match runner.holdoff(&paused) {
            Some((Hold::UntilCommittedStateChanges { revision }, reason)) => {
                assert_eq!(reason, HoldReason::AwaitingAnOutsideAnswer);
                revision
            },
            other => panic!(
                "a run waiting on a person must be held on its committed revision, not on a \
                 clock and not forever, got {other:?}"
            ),
        };
        assert_eq!(
            first.next_wake_ms, None,
            "no clock releases this hold, so it must not shorten the nap"
        );

        // The store still offers it, and this is the premise the finished-run
        // test above no longer shares: nothing about a pause is visible to the
        // scan. `TerminalKind::PausedByUser` is resumable so no ending marker is
        // published, and `WaitReason` has no variant for a human so the state
        // carries nothing to withhold it by. A finished run IS withheld; this
        // one is not, and everything below depends on the difference.
        assert!(
            store
                .list_runnable(&worker(), 16)
                .await
                .expect("the scan answers")
                .contains(&paused),
            "a store that learned to withhold pauses would make the holdoff below untestable \
             and this whole fixture a statement about the store"
        );

        let second = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(second.offered, 1);
        assert_eq!(second.skipped_held, 1);
        assert_eq!(
            second.holdoff_probes, 1,
            "the one hold that cannot be answered from memory costs exactly one `load` — no \
             claim, no lease, no write — and says so"
        );
        assert_eq!(second.attempted, 0);
        assert_eq!(
            fleet.phase_runs(),
            1,
            "nothing outside answered, so the phase must not have run again. A timed re-check \
             here is a paid `Decide` every interval for as long as a human takes to reply"
        );

        // Something outside commits. A resume has to, before the run makes any
        // progress, which is what makes the revision the cheap honest witness.
        let committed = store
            .load(&paused)
            .await
            .expect("the store answers")
            .expect("the run is committed");
        assert_eq!(committed.revision, paused_at);
        store
            .commit(&paused, &committed.state, committed.revision)
            .await
            .expect("an outside commit");

        let third = runner.sweep(&store, &mut fleet, &cancel).await;
        assert_eq!(third.holdoff_probes, 1);
        assert_eq!(
            third.attempted, 1,
            "the moment the committed state moves the hold is gone; holding a paused run for \
             the worker's lifetime would strand every run a person was about to answer"
        );
        assert_eq!(fleet.phase_runs(), 2);
    }

    #[test]
    fn a_timed_hold_is_honoured_by_the_runner_and_then_lapses_on_its_own() {
        // Sleeping runs are now withheld durably by the store until their wake
        // time, while pauses are revision-held for an outside answer. Exercise
        // the runner's clock-only hold directly so this test continues to pin
        // both halves of `Hold::Until` without pretending either terminal has
        // the old classification.
        let sleeper = key("aaa-sleeps");
        let mut runner = runner(RunnerConfig::default());
        let mut report = SweepReport::default();
        let now_ms = 10_000;
        let until_ms = now_ms + 500;

        runner.hold(
            &sleeper,
            Hold::Until(until_ms),
            HoldReason::RunSuspended,
            &mut report,
        );
        assert_eq!(runner.is_held(&sleeper, now_ms), Some(true));
        assert_eq!(runner.next_wake_ms(), Some(until_ms));
        assert_eq!(
            runner.holdoff(&sleeper),
            Some((Hold::Until(until_ms), HoldReason::RunSuspended))
        );

        runner.expire_holdoffs(until_ms - 1);
        assert_eq!(runner.is_held(&sleeper, until_ms - 1), Some(true));

        runner.expire_holdoffs(until_ms);
        assert_eq!(runner.is_held(&sleeper, until_ms), Some(false));
        assert_eq!(runner.next_wake_ms(), None);
        assert_eq!(runner.holdoff(&sleeper), None);
    }

    #[tokio::test]
    async fn a_full_holdoff_set_evicts_something_other_than_the_entry_being_added() {
        // Eviction, which nothing reached before: the set could not grow past
        // `batch` while the scan asked for `batch`, so `max_held_keys` and this
        // whole loop were unreachable against either shipped store.
        //
        // The property is narrower than "it evicts". `hold` inserts and THEN
        // evicts, and the just-inserted entry took part in the comparison — so a
        // timed hold added to a set already full of permanent ones was the
        // minimum by construction and evicted itself. `hold` returned having
        // done nothing, the key was re-claimed on the very next sweep, and the
        // report said an eviction had happened, which a reader takes to mean
        // some OTHER key was dropped.
        let store = CountingStore::new();
        let ends = key("aaa-ends");
        let sleeps = key("bbb-sleeps");
        seed(&store, &ends).await;
        seed(&store, &sleeps).await;
        let mut fleet = ScriptedFleet::new(vec![
            (ends.clone(), vec![Script::EndRun]),
            (sleeps.clone(), vec![Script::Sleep]),
        ]);
        let mut runner = runner(RunnerConfig {
            max_held_keys: 1,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(report.attempted, 2, "both keys took a turn");
        assert_eq!(report.holdoff_evictions, 1);
        assert_eq!(report.held_keys, 1);
        assert_eq!(
            runner.holdoff(&sleeps).map(|(_, reason)| reason),
            Some(HoldReason::RunSuspended),
            "the entry just added must survive; a `hold` that can silently be a no-op is worse \
             than a full set, because nothing in the report says which key was really dropped"
        );
        assert_eq!(
            runner.holdoff(&ends),
            None,
            "the victim is one of the OTHERS, chosen by which lapses soonest"
        );
    }

    // ====================================================================
    // Fail closed
    // ====================================================================

    #[tokio::test]
    async fn a_store_that_cannot_answer_stops_the_sweep_rather_than_the_key() {
        // "Refuse to advance rather than run unrecorded work" is not "skip this
        // one and try the next": the next fifteen claims would be taken against
        // a store that has just said it can record nothing. The measurement is
        // the SECOND key — a runner that treated the outage as a per-key problem
        // would attempt it, and would look correct in every other assertion.
        let mut store = CountingStore::new();
        let first = key("aaa-first");
        let second = key("bbb-second");
        // Seeded before the failure is armed, so the fixture's own writes are
        // not what fails.
        seed(&store, &first).await;
        seed(&store, &second).await;
        store.fail_claim = true;

        let mut fleet = ScriptedFleet::new(vec![
            (first.clone(), vec![Script::Continue]),
            (second.clone(), vec![Script::Continue]),
        ]);
        let mut runner = runner(RunnerConfig::default());
        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(report.offered, 2);
        assert_eq!(
            report.attempted, 1,
            "the sweep must stop at the first key the store could not answer for"
        );
        assert_eq!(
            store.claims.load(Ordering::SeqCst),
            1,
            "exactly one claim was attempted; a runner that carried on would have taken two"
        );
        assert!(
            matches!(
                report.stopped,
                Some(SweepStop::StoreUnavailable {
                    during: "claim",
                    ..
                })
            ),
            "the sweep must report WHY it stopped and WHERE, got {:?}",
            report.stopped
        );
        assert_eq!(
            runner.holdoff(&first),
            None,
            "an outage teaches nothing about the key; holding it off would withhold a healthy \
             run because the disk was full for a moment"
        );
    }

    #[tokio::test]
    async fn a_scan_that_cannot_answer_claims_nothing_at_all() {
        // The other end of the same rule. Discovery failing is not "assume
        // there is nothing runnable" and it is not "fall back to some other way
        // of finding work" — there is no other way, and inventing one is how a
        // worker starts advancing runs nobody offered it.
        let mut store = CountingStore::new();
        let only = key("aaa-only");
        seed(&store, &only).await;
        store.fail_scan = true;

        let mut fleet = ScriptedFleet::new(vec![(only.clone(), vec![Script::Continue])]);
        let mut runner = runner(RunnerConfig::default());
        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.offered, 0,
            "the FIRST page failed, so nothing was ever offered. This is the half that \
             distinguishes this test from the one below: both report `scan_end: None`, and only \
             this one also reports an empty sweep"
        );
        assert_eq!(report.claims, 0);
        assert_eq!(store.claims.load(Ordering::SeqCst), 0);
        assert!(matches!(
            report.stopped,
            Some(SweepStop::StoreUnavailable {
                during: "scan_runnable",
                ..
            })
        ));
        assert_eq!(
            report.scan_pages, 1,
            "one page was asked for and refused; a runner that retried the same failing scan \
             would spin against a substrate that has just said it cannot answer"
        );
        assert_eq!(
            report.scan_end, None,
            "a scan that could not answer teaches nothing about the WALK either. Naming any \
             `ScanEnd` here would claim something about a store that refused to be read — and \
             `WalkedTheWholeStore` in particular would tell an operator the tail had been seen"
        );
    }

    #[tokio::test]
    async fn a_scan_that_fails_on_a_later_page_keeps_what_the_earlier_pages_offered() {
        // The shape `ScanEnd`'s docs used to deny. They said *"`None` means the
        // first page itself could not be answered, so nothing was learned about
        // the walk at all"*, and the code has never said that: `scan_failed` is
        // set by the `Err` arm of ANY page and is the first branch of the
        // classifier. So a sweep that worked two pages and was refused the third
        // reports `scan_end: None` beside a non-zero `offered`, and an operator
        // reading the old doc would conclude the store had not been touched.
        //
        // This is the test that pins which of the two is meant. Nothing drove a
        // later-page failure before it, so the doc and the code could disagree
        // indefinitely.
        //
        // `NoHostsHere` is what keeps the batch unmet so the sweep keeps paging,
        // and `batch = 1` is what makes each page hold one key.
        let mut store = CountingStore::new();
        for id in ["aaa", "bbb", "ccc"] {
            seed(&store, &key(id)).await;
        }
        // Pages one and two answer; page three refuses.
        store.fail_scan_after_pages = Some(2);

        let mut runner = runner(RunnerConfig {
            batch: 1,
            ..RunnerConfig::default()
        });
        let report = runner
            .sweep(&store, &mut NoHostsHere, &CancellationToken::new())
            .await;

        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            3,
            "three pages were asked for; a runner that stopped after the first refusal is the \
             behaviour under test, and a runner that retried would ask for more"
        );
        assert_eq!(report.scan_pages, 3);
        assert_eq!(
            report.offered, 2,
            "THE POINT: the two pages that answered offered a key each, and those keys are not \
             un-offered by a later refusal. A sweep whose report zeroed this would be the old \
             doc's claim made true, and the two keys' turns would be in no counter at all"
        );
        assert_eq!(
            report.hosts_not_composable, 2,
            "and both were really worked — the fleet was asked about each — so the sweep did \
             something before the store stopped it"
        );
        assert_eq!(
            report.outcomes.len(),
            2,
            "one outcome per key the sweep reached, which is what a caller reconciles against"
        );
        assert_eq!(
            report.entry_probes, 2,
            "one pre-claim cursor read per offered key"
        );
        assert_eq!(
            report.scan_end, None,
            "`None` on a LATER page, not only the first one. There is no honest `ScanEnd` for a \
             walk that refused to be read: `SweepEnded` — which the next branch of the \
             classifier would give, since the failure also sets `stopped` — would say the sweep \
             chose to stop, and it did not"
        );
        assert!(
            matches!(
                report.stopped,
                Some(SweepStop::StoreUnavailable {
                    during: "scan_runnable",
                    ..
                })
            ),
            "and the refusal is still an outage that stops the sweep closed, got {:?}",
            report.stopped
        );
        assert_eq!(
            report.attempted, 0,
            "nothing was hosted, which is what kept the batch unmet and the sweep paging into \
             the failure"
        );
    }

    // ====================================================================
    // A pickup starts at an iteration boundary
    // ====================================================================

    #[tokio::test]
    async fn a_run_that_stopped_inside_an_iteration_is_never_claimed() {
        // The fleet in this fixture is READY and its host would run the
        // phase, which is the whole point: with the cursor check missing this
        // test sees a claim and a phase run, not merely a different hold reason.
        // A fixture whose fleet refused could not tell the two apart — it
        // would report `hosts_not_composable` either way.
        //
        // `Decide` because it is the expensive one to be wrong about: re-running
        // it against an `IterationCarry` this runner never filled is a paid LLM
        // call spent re-deciding a turn the run had already decided.
        let store = CountingStore::new();
        let stalled = key("aaa-stalled");
        seed_at(&store, &stalled, Phase::Decide).await;

        let mut fleet = ScriptedFleet::new(vec![(stalled.clone(), vec![Script::Continue])]);
        let mut runner = runner(RunnerConfig {
            // Recognisable, and distinct from every other interval in the
            // default config, so the assertion below is about THIS field rather
            // than about "some hold in the future".
            mid_iteration_holdoff: Duration::from_millis(8_000),
            ..RunnerConfig::default()
        });
        let before_ms = Utc::now().timestamp_millis();
        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.offered, 1,
            "the store cannot see a mid-iteration cursor — nothing in `list_runnable` reads the \
             phase — so the key IS offered, which is what makes this the runner's problem"
        );
        assert_eq!(
            report.entry_probes, 1,
            "the cursor is read once here: one probe per offered key, before the host and before \
             the claim. A park exit adds a second, and this fixture never parks"
        );
        assert_eq!(report.mid_iteration_refusals, 1);
        assert_eq!(
            report.attempted, 0,
            "a key refused on its cursor is never claimed"
        );
        assert_eq!(
            store.claims.load(Ordering::SeqCst),
            0,
            "no lease is taken, so no fence moves and no attempt is charged against the run's \
             quarantine counter"
        );
        assert_eq!(
            fleet.phase_runs(),
            0,
            "the ready host must never be handed a phase whose inputs live in another holder's \
             memory"
        );
        assert_eq!(
            report.hosts_not_composable, 0,
            "the host was never asked for; counting this as a composition failure would send an \
             operator looking at the fleet"
        );
        // TIMED, and this is the assertion that was inverted. The first cut held
        // the key for the worker's lifetime on the argument that "no further
        // claim changes the answer" — which is a claim about the future, not an
        // observation. Another holder advancing this cursor to a boundary is the
        // ordinary case, and a permanent hold refuses the pickup at exactly the
        // moment it becomes the right one, with `expire_holdoffs` never dropping
        // the entry and `is_held` answering `Some(true)` forever.
        match runner.holdoff(&stalled) {
            Some((Hold::Until(until_ms), HoldReason::MidIterationPickup)) => {
                assert!(
                    until_ms >= before_ms + 8_000,
                    "the hold must use `mid_iteration_holdoff`; {until_ms} is not {before_ms} \
                     plus that interval, so the guard reached for a different field"
                );
            },
            other => panic!(
                "a mid-iteration cursor must be held to an INSTANT. A worker-lifetime hold \
                 strands every run whose holder later stops at an iteration boundary, got \
                 {other:?}"
            ),
        }
        assert_eq!(
            report.next_wake_ms,
            runner.holdoff(&stalled).and_then(|(hold, _)| match hold {
                Hold::Until(until_ms) => Some(until_ms),
                _ => None,
            }),
            "a timed hold has an instant, so the sweep must report it; a permanent one reported \
             None and the nap ran past the moment the key was claimable again"
        );
    }

    #[tokio::test]
    async fn a_park_exit_does_not_carry_the_turn_into_the_phase_the_run_parked_at() {
        // The two-step transition, and the reason the pre-claim carve-out for a
        // parked key is worth nothing on its own.
        //
        // `driver_worker::leave_park` clears the wait and commits and does NOT
        // move the cursor, and `Advanced::LeftPark` classifies to `KeepGoing`.
        // So a parent parked on children out of `Apply` wakes with `wait: None`
        // and a cursor still at `Apply`, and the SAME turn re-claims and enters
        // that phase — `run_phase`'s `carry_missing`, from inside the claim,
        // with the lease taken, the fence moved and `commit_failed_attempt`
        // charging `phase_attempts`. Every wake burns one attempt until the
        // parent is durably quarantined for a defect belonging to the pickup.
        //
        // The measurement is the SECOND fleet's `phase_runs`. A fixture
        // asserting only the hold reason would pass on a runner that ran the
        // phase and then held.
        //
        // The park is produced by driving the run rather than by writing a
        // parked state into the store, and that is not fastidiousness:
        // `verify_journal` compares the committed cursor against what the
        // journal replays to, so a hand-built cursor at `Apply` over an empty
        // log is quarantined before the park branch is ever reached, and the
        // test would pass while measuring nothing.
        let store = CountingStore::new();
        let parent = key("aaa-parked-parent");
        seed(&store, &parent).await;

        // Prepare → Observe → Decide → Resolve → Apply, then park there.
        let mut opening_fleet = ScriptedFleet::new(vec![(
            parent.clone(),
            vec![
                Script::Continue,
                Script::Continue,
                Script::Continue,
                Script::Continue,
                Script::Park,
            ],
        )]);
        let mut opening_runner = runner(RunnerConfig::default());
        let opening = opening_runner
            .sweep(&store, &mut opening_fleet, &CancellationToken::new())
            .await;
        assert_eq!(
            opening.mid_iteration_refusals, 0,
            "it started at a boundary"
        );
        assert_eq!(opening_fleet.phase_runs(), 5);
        let parked = store
            .load(&parent)
            .await
            .expect("the store answers")
            .expect("the parent is committed");
        assert_eq!(
            parked.state.cursor.phase,
            Phase::Apply,
            "the premise: a terminal does not move the cursor, so the park's cursor is the phase \
             that parked"
        );
        let wait = parked
            .state
            .wait
            .clone()
            .expect("the park must have committed a wait, or nothing below is about a park");

        // The store now withholds the key, which is the whole point of a wait.
        assert!(
            !store
                .list_runnable(&worker(), 16)
                .await
                .expect("the scan answers")
                .contains(&parent),
            "a parked run is invisible to the scan until something resolves its wake"
        );

        // The wake RESOLVES. This is the transition the pre-claim carve-out
        // exists to allow, and the one a cursor-gated parked key would withhold
        // forever.
        store
            .resolve_wake(&parent, &wait.wake_token(), "child-1-completed")
            .await
            .expect("the fixture's wake resolution");

        // A SECOND worker, cold, with its own counter — the shape a real pickup
        // has, and the only shape in which "another holder's memory" means
        // anything. READY and scripted to continue, so a missing check shows up
        // as a phase run rather than as a different hold reason.
        let mut pickup_fleet = ScriptedFleet::new(vec![(parent.clone(), vec![Script::EndRun])]);
        let mut pickup_runner = runner(RunnerConfig::default());
        let report = pickup_runner
            .sweep(&store, &mut pickup_fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.offered, 1,
            "a parked key whose wake resolved IS offered — that is what the wake index is for — \
             so the pre-claim cursor check must let it through"
        );
        assert_eq!(
            report.attempted, 1,
            "the park EXIT still happens; refusing the parked key before the claim would \
             withhold the transition the wake index exists to produce"
        );
        assert_eq!(
            report.claims, 3,
            "the park exit is followed by one cold rewind and one recovered phase"
        );
        assert_eq!(
            pickup_fleet.phase_runs(),
            1,
            "the cold holder must rewind before running one safely reconstructed phase"
        );
        assert_eq!(
            report.mid_iteration_refusals, 0,
            "Apply has a durable rewind path, so the cold pickup is recovered rather than refused"
        );
        assert_eq!(
            report.entry_probes, 2,
            "one probe before the claim (which passed, because the key was parked) and one after \
             the exit (which refused). Both are the same question"
        );
        assert!(
            matches!(
                pickup_runner.holdoff(&parent),
                Some((Hold::Until(_), HoldReason::RunEnded))
            ),
            "the recovered phase ended the run, so the runner installs its transient ended \
             guard; got {:?}",
            pickup_runner.holdoff(&parent)
        );

        // The exit itself is not undone: the wait is cleared and the commit
        // stands. The cold recovery rewinds away from the parked Apply and the
        // recovered phase ends at Observe. Rolling the exit back would trade
        // one hang for another; carrying straight into Apply would repeat the
        // side-effecting phase the test exists to exclude.
        let after = store
            .load(&parent)
            .await
            .expect("the store answers")
            .expect("the parent is committed");
        assert_eq!(after.state.wait, None, "the park exit must have committed");
        assert_ne!(
            after.revision, parked.revision,
            "and it must have been a real commit, not a no-op the assertion above would also \
             accept if `leave_park` had stopped writing"
        );
        assert_eq!(
            after.state.cursor.phase,
            Phase::Observe,
            "the cold rewind must end on the safely reconstructed Observe phase, never carry \
             the turn into the parked Apply"
        );
    }

    #[tokio::test]
    async fn a_finished_run_is_withheld_and_a_paused_one_recovers_cold() {
        // Not a defect being pinned — a BOUNDARY being pinned, because the
        // report it shapes is read by an operator, and it moved.
        //
        // `driver_worker::next_cursor` answers `RecordedStep::RunEnded =>
        // Ok(current)`, so ANY terminal leaves the cursor at the phase that
        // ended the run. What happens to such a key on a cold worker's sweep
        // then depends on which terminal it was, and the two answers are the
        // whole point of this fixture:
        //
        // - **non-resumable** — the stores publish an ending marker at commit
        //   and withhold the key from every later scan. A cold worker is never
        //   offered it, so the cursor check is not reached, `mid_iteration_
        //   refusals` does not move, and `HoldReason::AlreadyEnded` is
        //   unreachable through the sweep path. This half used to assert the
        //   opposite — *"the store still offers a finished run"* — and it was
        //   true when it was written. It is asserted here against
        //   `MemoryLoopStateStore`; filesystem crash ordering has its own
        //   revision-bound marker regression in `store/fs.rs`;
        // - **resumable, committing no wait** — a pause. No marker is published
        //   and `WaitReason` has no variant for a human, so the key IS offered,
        //   its cursor is mid-iteration, and the pre-claim check refuses it
        //   before the claim. That is the population `SweepReport::
        //   mid_iteration_refusals` now counts.
        //
        // TWO runners, because that is the deployment shape this module targets
        // — a short-lived worker that sweeps and exits. The first runner's
        // holdoff set would hide both effects inside one process.
        //
        // Both cursors are produced by driving the real phases rather than by
        // `seed_at`, which is what this fixture has that
        // `a_run_that_stopped_inside_an_iteration_is_never_claimed` does not: a
        // hand-seeded cursor would agree with whatever the driver did.
        let store = CountingStore::new();
        let finished = key("aaa-finished");
        let paused = key("bbb-paused");
        seed(&store, &finished).await;
        seed(&store, &paused).await;

        // Prepare → Observe → Decide → Resolve → Apply, then end it. The
        // terminal leaves the cursor at `Apply` on both keys; only the KIND of
        // terminal differs.
        let mut fleet = ScriptedFleet::new(vec![
            (
                finished.clone(),
                vec![
                    Script::Continue,
                    Script::Continue,
                    Script::Continue,
                    Script::Continue,
                    Script::EndRun,
                ],
            ),
            (
                paused.clone(),
                vec![
                    Script::Continue,
                    Script::Continue,
                    Script::Continue,
                    Script::Continue,
                    Script::Pause,
                ],
            ),
        ]);
        let mut first = runner(RunnerConfig::default());
        let opening = first
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;
        assert_eq!(
            opening.mid_iteration_refusals, 0,
            "both started at a boundary"
        );
        assert_eq!(opening.attempted, 2);
        assert_eq!(fleet.phase_runs(), 10, "five phases on each key");
        // `execution` and not `key`: the fixture's key constructor is a function
        // called `key`, and a binding of that name would shadow it for the rest
        // of the block.
        for (execution, what) in [(&finished, "the finished run"), (&paused, "the paused run")] {
            assert_eq!(
                store
                    .load(execution)
                    .await
                    .expect("the store answers")
                    .expect("committed")
                    .state
                    .cursor
                    .phase,
                Phase::Apply,
                "the premise, and it holds for BOTH terminals: a terminal does not move the \
                 cursor, so {what}'s cursor is wherever it ended"
            );
        }

        // A SECOND worker, cold. This is what a real deployment looks like.
        let mut second = runner(RunnerConfig::default());
        let report = second
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.offered, 1,
            "the store withholds the finished run and offers the paused one. A store that had \
             not learned the ending marker would offer two, and the assertions below would all \
             still pass — which is why this count is asserted rather than inferred"
        );
        assert_eq!(
            report.outcomes.len(),
            1,
            "and only the paused key produced an outcome"
        );
        assert_eq!(report.outcomes[0].key, paused);
        assert_eq!(
            report.mid_iteration_refusals, 0,
            "Apply carries a durable rewind path, so the paused run recovers instead of being refused"
        );
        assert_eq!(
            report.hosts_not_composable, 0,
            "the configured fleet composes the paused execution"
        );
        assert_eq!(report.attempted, 1);
        assert_eq!(report.claims, 6, "one rewind plus five recovered phases");
        assert_eq!(
            fleet.phase_runs(),
            15,
            "the finished run stays withheld while the paused run safely replays five phases"
        );
        assert!(
            matches!(
                second.holdoff(&paused),
                Some((
                    Hold::UntilCommittedStateChanges { .. },
                    HoldReason::AwaitingAnOutsideAnswer
                ))
            ),
            "the cold worker reaches the human pause again and holds until an outside commit, \
             got {:?}",
            second.holdoff(&paused)
        );
        assert_eq!(
            second.holdoff(&finished),
            None,
            "and it holds NOTHING for the finished run: a key the scan never offered is a key \
             this runner never met. `HoldReason::AlreadyEnded` is unreachable through the sweep \
            path against either shipped store; seeing this reason in a runner's logs points to \
             a decorated/regressed store or a direct non-sweep caller"
        );
    }
    #[tokio::test]
    async fn the_cursor_check_gates_the_pickup_and_not_every_phase() {
        // The other direction, and it is what stops the guard above from being
        // written as "refuse everything".
        //
        // Two properties in one fixture, and the second is the one a narrower
        // test would miss. `fresh_state` commits the default cursor — iteration
        // one at `Prepare`, the only phase reading nothing an earlier phase of
        // the same iteration produced — so the key is claimed. The first script
        // step then COMMITS `Observe`, which is a mid-iteration cursor: a guard
        // placed inside the turn rather than at the pickup would refuse its own
        // run's second phase and stall every execution after one step.
        let store = CountingStore::new();
        let ready = key("aaa-ready");
        seed(&store, &ready).await;

        let mut fleet = ScriptedFleet::new(vec![(
            ready.clone(),
            vec![Script::Continue, Script::EndRun],
        )]);
        let mut runner = runner(RunnerConfig::default());
        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(report.mid_iteration_refusals, 0);
        assert_eq!(report.attempted, 1, "a boundary cursor is claimed");
        assert_eq!(
            fleet.phase_runs(),
            2,
            "both scripted phases must run in ONE turn; a guard that re-asked the cursor between \
             phases would stop after the first"
        );
    }

    // ====================================================================
    // Bounded
    // ====================================================================

    #[tokio::test]
    async fn one_execution_cannot_hold_the_whole_sweep() {
        // Fairness, and it is the reason the per-execution budget exists at all.
        // A run scripted to continue forever will happily take every claim a
        // sweep has; the second key never being reached would be invisible from
        // any single-key test.
        let store = CountingStore::new();
        let greedy = key("aaa-greedy");
        let waiting = key("bbb-waiting");
        seed(&store, &greedy).await;
        seed(&store, &waiting).await;
        let mut fleet =
            ScriptedFleet::new(vec![(greedy.clone(), vec![]), (waiting.clone(), vec![])]);
        let mut runner = runner(RunnerConfig {
            max_claims_per_execution: 3,
            max_claims_per_sweep: 4,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(report.claims, 4, "the sweep budget is the total");
        assert_eq!(report.outcomes.len(), 2);
        assert_eq!(report.outcomes[0].key, greedy);
        assert_eq!(report.outcomes[0].claims, 3);
        assert_eq!(report.outcomes[0].end, EndOfTurn::YieldedOnClaimBudget);
        assert_eq!(
            report.outcomes[1].key, waiting,
            "the second key must get a turn; a runner honouring only the sweep budget would \
             have spent all four claims on the first"
        );
        assert_eq!(report.outcomes[1].claims, 1);
        assert_eq!(report.outcomes[1].end, EndOfTurn::YieldedOnSweepBudget);
        assert_eq!(
            runner.holdoff(&greedy),
            None,
            "yielding on a budget is not a refusal; the key must be immediately eligible again"
        );
    }

    // ====================================================================
    // No host means no lease
    // ====================================================================

    #[tokio::test]
    async fn a_run_with_no_composable_host_is_never_claimed() {
        // The ordering claim in the module docs, as a measurement rather than a
        // comment.
        //
        // Fixtures above measure the claims that follow. What this pins is the
        // narrower and permanent half — a key the fleet does not answer for costs the
        // store nothing, which is the property that makes a one-run fleet
        // affordable against a scan that offers hundreds of keys.
        let store = CountingStore::new();
        let only = key("aaa-only");
        seed(&store, &only).await;
        let mut runner = runner(RunnerConfig::default());

        let report = runner
            .sweep(&store, &mut NoHostsHere, &CancellationToken::new())
            .await;

        assert_eq!(report.offered, 1);
        assert_eq!(
            (report.hosts_not_composable, report.hosts_unavailable),
            (1, 0),
            "a structural gap and a momentary outage are different reports; one counter for \
             both told an operator neither, and the two call for opposite actions"
        );
        assert_eq!(report.attempted, 0);
        assert_eq!(report.claims, 0);
        assert_eq!(
            store.claims.load(Ordering::SeqCst),
            0,
            "a claim taken and released is still a write and a fence increment; the host is \
             asked FIRST so a runner with nothing to run touches nothing"
        );
        assert_eq!(
            runner.holdoff(&only).map(|(hold, reason)| (hold, reason)),
            Some((Hold::UntilThisWorkerRestarts, HoldReason::HostNotComposable)),
            "`NotComposable` means not while this process lives, so re-asking every sweep would \
             be a scan-rate spin against a fleet that has already answered"
        );
    }

    #[tokio::test]
    async fn a_run_that_ends_hands_its_outcome_back_to_the_fleet() {
        // The half of the seam `HostProvider` could not have had. `advance_once`
        // answers `RunEnded { outcome, .. }` and the runner used to drop it on
        // the floor: the only participant that can deliver a finished run's
        // answer is the holder that lent the host, and it had no way to be told.
        //
        // Measured through the FLEET rather than through the report, because
        // that is where a caller reads it — and because a `KeyOutcome` carrying
        // an `AgenticOutcome` is exactly what this design refused (it would cost
        // `PartialEq` on every report type in the file).
        let store = CountingStore::new();
        let ending = key("aaa-ending");
        seed(&store, &ending).await;
        let mut fleet = ScriptedFleet::new(vec![(ending.clone(), vec![Script::EndRun])]);
        let mut runner = runner(RunnerConfig::default());

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(
            report.phases_committed, 1,
            "the run ended on its first phase"
        );
        assert_eq!(
            fleet.ended().len(),
            1,
            "a run that reached a terminal must be handed back exactly once; a runner that \
             dropped it makes the completion invisible to the process that owes the answer, \
             and that process's next claim gets `RunAlreadyEnded` — which `StatelessArm` turns \
             into a hard error"
        );
        assert_eq!(fleet.ended()[0].0, ending);
        assert_eq!(
            fleet.ended()[0].1,
            TerminalKind::Success,
            "the terminal travels beside the outcome so a caller can act on it without \
             matching twenty outcome variants"
        );
    }

    #[tokio::test]
    async fn a_run_that_only_continues_hands_nothing_back() {
        // The other direction, and it is the one that would be wrong silently:
        // an out-parameter written on every turn would tell a holder its live
        // run had finished. `Script::Continue` never returns a terminal.
        //
        // Distinct from the test above rather than an extra assertion inside it:
        // an assertion that cannot fire because an earlier one already
        // constrained the value is the failure mode this file has paid for
        // before.
        let store = CountingStore::new();
        let going = key("aaa-going");
        seed(&store, &going).await;
        let mut fleet = ScriptedFleet::new(vec![(going.clone(), vec![Script::Continue])]);
        let mut runner = runner(RunnerConfig {
            max_claims_per_execution: 1,
            ..RunnerConfig::default()
        });

        let report = runner
            .sweep(&store, &mut fleet, &CancellationToken::new())
            .await;

        assert_eq!(report.claims, 1);
        assert!(
            fleet.ended().is_empty(),
            "nothing ended, so nothing may be handed back"
        );
    }

    // ====================================================================
    // The classifier
    // ====================================================================

    #[test]
    fn an_answer_that_carries_its_own_time_is_held_until_exactly_that_time() {
        // The requirement in one line: a run that says `NotYetRunnable` must not
        // be hot-looped — and, just as much, must not be held past its own
        // moment. Both are broken by the same edit, which is adding a configured
        // interval to a time the answer already carried.
        let config = distinct_config();
        let now = 1_000;

        assert_eq!(
            classify(
                &Advanced::NotYetRunnable {
                    runnable_at_ms: 9_000
                },
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::Until(9_000),
                reason: HoldReason::NotYetRunnable,
            },
            "the committed retry time IS the hold; nothing may be added to it"
        );
        assert_eq!(
            classify(
                &Advanced::LeaseHeld {
                    by: WorkerId::new("somebody-else"),
                    until_ms: 4_242,
                },
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::Until(4_242),
                reason: HoldReason::LeaseHeld,
            },
            "another worker's lease expiry is a known time; probing before it is pure noise"
        );

        // And the two that have no time of their own use the interval named for
        // them, which is what the distinct config is for: a classifier reaching
        // for `transient_holdoff` here would look identical under a config whose
        // intervals matched.
        assert_eq!(
            classify(
                &Advanced::NotClaimable {
                    pinned_to: WorkerId::new("the-pin-holder"),
                    pinned_until_ms: now + 1_000,
                },
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::Until(now + 1_000),
                reason: HoldReason::PinnedElsewhere,
            }
        );
    }

    #[test]
    fn a_run_that_paused_is_not_held_for_the_life_of_the_worker() {
        // The defect this test exists for was in the first draft of `classify`,
        // which read `RunEnded` as one thing.
        //
        // A park commits a `wait`, the store withholds the key until a wake
        // resolution lands, and the wake landing is exactly the moment
        // `list_runnable` offers it again. A permanent holdoff would refuse it at
        // that moment and for the rest of the worker's life — the runner would
        // defeat the wake index the store was built around, and the only symptom
        // would be a delegated parent that never continues.
        //
        // Both halves are asserted, because a fix that made BOTH terminals timed
        // would pass the first assertion alone and reintroduce the scan-rate
        // spin on finished runs that this whole module exists to stop.
        let config = distinct_config();
        let now = 1_000;
        let ended = |terminal: TerminalKind, outcome: AgenticOutcome| Advanced::RunEnded {
            revision: Revision::INITIAL,
            cursor: LoopCursor::default(),
            outcome: Box::new(outcome),
            terminal,
        };

        assert_eq!(
            classify(
                &ended(
                    TerminalKind::WaitingForChildren,
                    AgenticOutcome::WaitingForChildren {
                        child_execution_ids: vec!["child-1".to_string()],
                        last_state: EnvironmentState::Uninitialized,
                        iterations_used: 1,
                        pause_state: None,
                    },
                ),
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::Until(now + 6_000),
                reason: HoldReason::RunSuspended,
            },
            "a resumable terminal leaves the execution ALIVE; the hold must lapse so the thing \
             that answers it can be acted on"
        );

        assert_eq!(
            classify(
                &ended(
                    TerminalKind::Success,
                    AgenticOutcome::Success {
                        completion:
                            crate::magician_v2::execution::agentic::types::CompletionKind::Full,
                        open: Vec::new(),
                        final_state: EnvironmentState::Uninitialized,
                        iterations_used: 1,
                        artifacts: Vec::new(),
                    },
                ),
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::Until(now + 4_000),
                reason: HoldReason::RunEnded,
            },
            "a terminal with no projection debt disappears from discovery; a transient hold \
             prevents a second claim in this sweep while allowing durable outbox debt to retry"
        );

        // And the third answer, which reading "resumable" as one thing hid: a
        // run that stopped for a PERSON committed no wait, so the store cannot
        // withhold it and a re-claim re-runs the phase that paused. Neither of
        // the two holds above is right for it — a timer makes it a paid poll of
        // a human, forever makes it a stranded run — so it is held on the
        // revision it ended at.
        assert_eq!(
            classify(
                &ended(
                    TerminalKind::PausedByUser,
                    AgenticOutcome::PausedByUser {
                        pause_state: pause_state(),
                        iterations_used: 1,
                    },
                ),
                now,
                &config
            ),
            Disposition::Release {
                hold: Hold::UntilCommittedStateChanges {
                    revision: Revision::INITIAL,
                },
                reason: HoldReason::AwaitingAnOutsideAnswer,
            },
            "the interval named for a park must not be reached for a pause; `resumable_terminal_\
             holdoff` is 6_000 in this config and seeing it here means the two were folded again"
        );
    }

    #[test]
    fn a_projection_refusal_that_carries_its_own_expiry_is_not_held_for_the_workers_life() {
        // `Refusal::Projection(_)` was a catch-all one level below where the
        // source gate looks — that gate reads arm heads, and this arm's head is
        // upper-case — so five `LoopStateRefusal` variants shared one answer and
        // one comment claiming *"every one of them re-refuses on the next
        // claim"*. `PinnedElsewhere` carries `until_ms` precisely because it
        // does not.
        //
        // Not reachable through `advance_under_lease` today, which checks
        // `claimable_by` against the same clock before it projects. The defect
        // is the seam: a transient refusal added later, or a second `project`
        // call site reading a different clock, inherits a worker-lifetime hold
        // from a comment asserting that cannot happen.
        let config = distinct_config();

        assert_eq!(
            classify(
                &Advanced::Refused(Refusal::Projection(LoopStateRefusal::PinnedElsewhere {
                    owner: WorkerId::new("the-pin-holder"),
                    asked: worker(),
                    until_ms: 7_777,
                })),
                1_000,
                &config,
            ),
            Disposition::Release {
                hold: Hold::Until(7_777),
                reason: HoldReason::PinnedElsewhere,
            },
            "a refusal that knows its own expiry is held to exactly that instant, for the same \
             reason `LeaseHeld` is"
        );
        assert_eq!(
            classify(
                &Advanced::Refused(Refusal::Projection(
                    LoopStateRefusal::WorkBudgetWentBackwards {
                        committed_ms: 9,
                        projected_ms: 4,
                    }
                )),
                1_000,
                &config,
            ),
            Disposition::Release {
                hold: Hold::UntilThisWorkerRestarts,
                reason: HoldReason::ProjectionRefused,
            },
            "the four that really do re-refuse must keep the permanent hold; making them all \
             timed would put a scan-rate spin back on a state no claim can fix"
        );
    }

    #[test]
    fn the_two_answers_that_must_never_become_keep_going_do_not() {
        // `EffectIndeterminate` is an effect with no result and no licence to
        // fire; `Quarantined` is a run held for an operator. A runner that
        // classified either as `KeepGoing` would re-enter the phase that was
        // refused — which for the first one means firing a send nobody can
        // vouch for.
        let config = distinct_config();

        assert_eq!(
            classify(
                &Advanced::EffectIndeterminate {
                    effect_id: EffectId::parse("llm-1:tool:send-1").expect("a well-formed id"),
                    reason: "the outward record could not answer".to_string(),
                },
                1_000,
                &config
            ),
            Disposition::Release {
                hold: Hold::UntilThisWorkerRestarts,
                reason: HoldReason::EffectIndeterminate,
            }
        );
        assert_eq!(
            classify(
                &Advanced::Quarantined(Quarantine::PhaseAttemptsExhausted {
                    phase: Phase::Decide,
                    attempts: 3,
                }),
                1_000,
                &config
            ),
            Disposition::Release {
                hold: Hold::UntilThisWorkerRestarts,
                reason: HoldReason::Quarantined,
            }
        );
    }

    #[test]
    fn only_an_unreachable_substrate_stops_the_sweep() {
        // The distinction the fail-closed rule turns on, and the one a blanket
        // `StoreError => FailClosed` would erase: one run's unparseable record
        // is not an outage, and treating it as one stalls every healthy
        // execution behind it.
        let config = distinct_config();

        assert!(
            matches!(
                classify(
                    &Advanced::Refused(Refusal::Store {
                        during: "commit",
                        error: StoreError::Unavailable {
                            detail: "no such directory".to_string(),
                        },
                    }),
                    1_000,
                    &config,
                ),
                Disposition::FailClosed {
                    during: "commit",
                    ..
                }
            ),
            "an unreachable substrate must stop the sweep"
        );
        assert_eq!(
            classify(
                &Advanced::Refused(Refusal::Store {
                    during: "commit",
                    error: StoreError::WatermarkAhead {
                        watermark: 9,
                        last_seq: 4,
                    },
                }),
                1_000,
                &config,
            ),
            Disposition::Release {
                hold: Hold::UntilThisWorkerRestarts,
                reason: HoldReason::StoreRecordRefused,
            },
            "a watermark past its own log names ONE run's records; the sweep carries on"
        );
    }

    // ====================================================================
    // Pacing
    // ====================================================================

    #[test]
    fn a_sweep_that_did_work_does_not_sleep_and_an_outage_does() {
        // The pacing policy, pinned as a pure function of a report rather than
        // measured — a timing test would pass on a nap short enough not to be
        // noticed, which is exactly the nap that turns an idle backoff into a
        // pause between two phases of a live run.
        let runner = runner(RunnerConfig {
            idle_backoff: Duration::from_secs(7),
            store_backoff: Duration::from_secs(11),
            ..RunnerConfig::default()
        });

        let worked = SweepReport {
            claims: 1,
            ..SweepReport::default()
        };
        assert_eq!(
            runner.nap_after(&worked),
            Duration::ZERO,
            "a worker that just advanced something has more to do"
        );

        let outage = SweepReport {
            stopped: Some(SweepStop::StoreUnavailable {
                during: "claim",
                detail: "down".to_string(),
            }),
            // Claims were taken before the store failed, so this report ALSO
            // satisfies `did_work`. The outage has to win, or a worker backs off
            // for zero seconds against a substrate that is down.
            claims: 1,
            ..SweepReport::default()
        };
        assert_eq!(runner.nap_after(&outage), Duration::from_secs(11));

        let idle = SweepReport::default();
        assert_eq!(runner.nap_after(&idle), Duration::from_secs(7));

        // A key that becomes claimable sooner than the idle interval shortens
        // the nap; one that becomes claimable later does not lengthen it.
        let now = Utc::now().timestamp_millis();
        let soon = SweepReport {
            next_wake_ms: Some(now),
            ..SweepReport::default()
        };
        assert_eq!(runner.nap_after(&soon), Duration::ZERO);
        let far = SweepReport {
            next_wake_ms: Some(now + 600_000),
            ..SweepReport::default()
        };
        assert_eq!(runner.nap_after(&far), Duration::from_secs(7));
    }

    #[tokio::test]
    async fn a_cancelled_runner_stops_without_sweeping() {
        let store = CountingStore::new();
        let mut runner = runner(RunnerConfig::default());
        let cancel = CancellationToken::new();
        cancel.cancel();

        let report = runner.run(&store, &mut NoHostsHere, &cancel, Some(4)).await;

        assert_eq!(report.stop, RunStop::Cancelled);
        assert_eq!(report.sweeps, 0);
        assert_eq!(
            store.scans.load(Ordering::SeqCst),
            0,
            "a cancelled worker must not take one more scan on its way out"
        );
    }

    #[tokio::test]
    async fn a_worker_gives_up_on_a_substrate_that_stays_down() {
        // A process spinning against a dead store forever is indistinguishable
        // from one that is working. It stops, says why, and an operator restarts
        // it when the substrate is back.
        let mut store = CountingStore::new();
        store.fail_scan = true;
        let mut runner = runner(RunnerConfig {
            max_consecutive_store_failures: 3,
            // Zero, so the test does not wait out three real backoffs. The
            // property under test is the COUNT, not the interval.
            store_backoff: Duration::ZERO,
            ..RunnerConfig::default()
        });

        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), None)
            .await;

        assert!(matches!(report.stop, RunStop::StoreUnavailable { .. }));
        assert_eq!(report.sweeps, 3);
        assert_eq!(store.scans.load(Ordering::SeqCst), 3);
        assert_eq!(
            report.reconciled,
            ReconcilerTally::default(),
            "the sweep failed, so the reconciler gate below it never opened"
        );
    }

    // ====================================================================
    // The reconciler pass `run` takes between sweeps
    //
    // Added 2026-08-27 because the block had none. The two `.run()` tests
    // above are the whole of this file's coverage of that function, and
    // NEITHER reaches `reconcile_once`: one cancels the token before the
    // first sweep, the other fails every `list_runnable` so `store_ok` is
    // false on every pass. Both leave `reconciled` at its default for
    // reasons that have nothing to do with the reconciler, so deleting the
    // whole `if let Some(every) = self.config.reconcile_every` block — or
    // inverting `store_ok`, or dropping the sweep-budget check above the nap
    // — left every test in the file green. That block is what turns on a
    // lease-taking write path (`Recovery::RetireProvedParks`).
    //
    // Every test here reads the store's OWN `list_parked` counter alongside
    // the tally, so a `reconcile_once` that incremented `passes` without
    // walking the store would fail rather than pass.
    // ====================================================================

    #[tokio::test]
    async fn a_runner_takes_one_reconciler_pass_before_its_first_nap() {
        let store = CountingStore::new();
        let mut runner = runner(RunnerConfig::default());

        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(1))
            .await;

        assert_eq!(report.stop, RunStop::SweepBudget);
        assert_eq!(report.sweeps, 1);
        assert_eq!(
            report.reconciled.passes, 1,
            "the slot is due at the FIRST opportunity — a worker that has just \
             come up is the one most likely to be looking at parks nothing will \
             wake — so one sweep takes one pass"
        );
        assert_eq!(
            store.park_listings(),
            1,
            "a pass is a `list_parked` walk, and this is the half of it the code \
             under test does not write"
        );
    }

    #[tokio::test]
    async fn a_second_sweep_inside_the_cadence_takes_no_second_pass() {
        let store = CountingStore::new();
        let mut runner = runner(RunnerConfig {
            // Zero so the two sweeps are back to back. The property under test
            // is the CADENCE, not the interval between sweeps.
            idle_backoff: Duration::ZERO,
            ..RunnerConfig::default()
        });

        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(2))
            .await;

        assert_eq!(report.sweeps, 2);
        assert_eq!(
            report.reconciled.passes, 1,
            "`reconcile_due_ms` moves a whole `reconcile_every` forward after a \
             pass; a runner that re-read the clock without moving it would walk \
             the store on every sweep"
        );
        assert_eq!(store.park_listings(), 1);
    }

    #[tokio::test]
    async fn a_sweep_the_store_could_not_answer_takes_no_reconciler_pass() {
        // The `store_ok` gate. Its control is
        // `a_runner_takes_one_reconciler_pass_before_its_first_nap` above: same
        // config shape, same one sweep, and one `list_parked` there against zero
        // here — so this assertion is a differential rather than a claim that
        // the reconciler never runs.
        let mut store = CountingStore::new();
        store.fail_scan = true;
        let mut runner = runner(RunnerConfig {
            max_consecutive_store_failures: 2,
            store_backoff: Duration::ZERO,
            ..RunnerConfig::default()
        });

        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), None)
            .await;

        assert!(matches!(report.stop, RunStop::StoreUnavailable { .. }));
        assert_eq!(report.reconciled.passes, 0);
        assert_eq!(report.reconciled.failed, 0);
        assert_eq!(
            store.park_listings(),
            0,
            "a pass whose first call is `list_parked` against a substrate that \
             has just refused a scan buys a second identical error and one more \
             directory walk"
        );
    }

    #[tokio::test]
    async fn a_reconciler_pass_that_fails_does_not_stop_the_runner() {
        // The important failure is the sweep, which advances work. Stalling live
        // executions because a park listing could not be produced would trade it
        // for the diagnostic one.
        let mut store = CountingStore::new();
        store.fail_park_list = true;
        let mut runner = runner(RunnerConfig {
            idle_backoff: Duration::ZERO,
            // Low, so that a failed PASS wrongly counted against this ceiling
            // would end the run before its sweep budget did.
            max_consecutive_store_failures: 2,
            ..RunnerConfig::default()
        });

        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(3))
            .await;

        assert_eq!(
            report.stop,
            RunStop::SweepBudget,
            "the runner stopped for its own budget, not for the reconciler"
        );
        assert_eq!(report.sweeps, 3);
        assert_eq!(report.reconciled.failed, 1);
        assert_eq!(
            report.reconciled.passes, 0,
            "a pass the store refused is counted as failed and in neither of the \
             tallies a finding would move"
        );
        assert_eq!(
            store.park_listings(),
            1,
            "the cadence still advances on a failed pass; retrying it every sweep \
             would hammer a substrate that has just said it cannot answer"
        );
    }

    #[tokio::test]
    async fn an_incomplete_pass_stays_incomplete_after_a_complete_one() {
        // `list_parked` has no cursor, so the second pass walks the same prefix
        // as the first. A complete pass following an incomplete one therefore
        // says nothing about the tail, and the flag must be `|=` rather than `=`.
        let sticky = {
            let mut store = CountingStore::new();
            store.park_incomplete_once = true;
            let mut runner = runner(RunnerConfig {
                // Every sweep, so the second pass can contradict the first.
                reconcile_every: Some(Duration::ZERO),
                idle_backoff: Duration::ZERO,
                ..RunnerConfig::default()
            });
            let report = runner
                .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(2))
                .await;
            assert_eq!(report.reconciled.passes, 2);
            assert_eq!(store.park_listings(), 2);
            report.reconciled.incomplete
        };

        // The control, and it is what makes the assertion above mean something:
        // same fixture with the first listing complete, so a flag that was
        // simply always true would fail here.
        let clean = {
            let store = CountingStore::new();
            let mut runner = runner(RunnerConfig {
                reconcile_every: Some(Duration::ZERO),
                idle_backoff: Duration::ZERO,
                ..RunnerConfig::default()
            });
            let report = runner
                .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(2))
                .await;
            assert_eq!(report.reconciled.passes, 2);
            report.reconciled.incomplete
        };

        assert!(sticky, "one incomplete pass makes the whole run incomplete");
        assert!(!clean, "two complete passes are complete");
    }

    #[tokio::test]
    async fn a_one_sweep_run_does_not_pay_an_idle_backoff_it_can_never_spend() {
        // The sweep-budget check sits ABOVE the nap. The check at the top of the
        // loop runs after the wait, so a one-sweep pass used to pay a full
        // `idle_backoff` before noticing it was already done — which is the
        // difference between a pickup that returns in milliseconds and one that
        // returns in seconds, on a live run's critical path.
        //
        // A wall-clock assertion because that is what the property is. The
        // margin is wide: five seconds paid against two seconds allowed.
        let store = CountingStore::new();
        let mut runner = runner(RunnerConfig {
            idle_backoff: Duration::from_secs(5),
            ..RunnerConfig::default()
        });

        let started = std::time::Instant::now();
        let report = runner
            .run(&store, &mut NoHostsHere, &CancellationToken::new(), Some(1))
            .await;
        let elapsed = started.elapsed();

        assert_eq!(report.stop, RunStop::SweepBudget);
        assert!(
            elapsed < Duration::from_secs(2),
            "a one-sweep run returned in {elapsed:?}, which is the idle backoff \
             being paid before the budget was checked"
        );
    }
}
