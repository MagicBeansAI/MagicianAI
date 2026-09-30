//! One iteration, as six functions instead of one ~4,000-line block.
//!
//! Before the lift the block was 4,042 lines between `'iteration_body: {` and
//! its close.
//! `decide` and `apply` hold almost all of it — roughly 950 and 2,450 lines of
//! function body — while `prepare`, `observe` and `epilogue` are under 110 each
//! and `resolve` under 500. What is left in `executor.rs` is a driver under 200
//! lines that calls them in order and re-performs whatever they decide.
//!
//! Those sizes are approximate on purpose. An exact table here is a claim about
//! six other files that nobody re-derives when one of them changes, and the
//! first version of this note had already drifted from what the files measure.
//! [`super::outcome::Phase`]'s own variant docs carry the same magnitudes, and
//! that is the copy to keep in step, because a phase and its size sit together
//! there.
//!
//! See `docs/archive/plans/2026-08-25-stateless-loop-design.md`. The cursor is
//! [`super::outcome::Phase`]; this module is what each of its variants actually
//! runs.
//!
//! # The contract the design asks for, and the one that is reachable today
//!
//! The design states the target signature plainly:
//!
//! ```text
//! fn(&LoopState, &Services) -> PhaseOutput
//! ```
//!
//! and the reason is testability — *"each phase is testable without a runtime"*.
//! That is the destination. It is not yet the signature, and writing it before it
//! is true would be a lie the compiler happily accepts.
//!
//! What the scratch extraction achieved is the `Services` half. Grouping folded
//! roughly thirty loose per-execution fields on `ActionExecutors` — each one an
//! `Arc<Mutex<…>>` or an atomic — into five values: `run_identity`,
//! `browser_run`, `api_mining_run`, `controls`, `run_outputs`.
//!
//! Ten fields still name a lock or an atomic in their own type, and the field
//! audit (`docs/archive/plans/2026-08-26-scratch-extraction-field-audit.md`) accounts
//! for nine: three scope-level caches deliberately shared *across* concurrent
//! executions, six process-shared service handles. The tenth is `run_outputs`,
//! which postdates the audit and is the one that really is this run's state. So
//! `&ActionExecutors` **is** the services bundle the design describes, near
//! enough to build on, with one field still owing a home.
//!
//! The `LoopState` half is not there. Per-execution state currently lives in
//! three places at once:
//!
//! 1. the seven groups under `run_loop/` — reachable as values,
//! 2. plain fields on `AgenticContext` — already boundary-carried by
//!    `AgenticPauseState`,
//! 3. **locals on the loop's own stack** — the twenty-eight `let mut` bindings
//!    the design calls out, which no field audit finds because they are not
//!    fields.
//!
//! Until (3) moves, a phase cannot take one `&LoopState`, because the state it
//! needs does not exist as one value to take.
//!
//! # What the extraction actually produced, 2026-08-26
//!
//! All six phases are now lifted and the iteration body is under 200 lines of
//! driver.
//!
//! An earlier note predicted that a shared per-iteration context struct would
//! grow the borrows each phase needs as the phases moved. **It did not**, and the
//! reason is worth keeping rather than quietly deleting: the six take 6, 7, 8, 9,
//! 14 and 17 bindings, and no two take the same set. `Prepare` wants the semantic
//! checkpoint hints; `Apply` wants the merkle baseline and the ephemeral secret
//! scope; `Epilogue` wants the iteration's start `Instant` and the history
//! length it began with; nobody else wants any of them. A struct
//! holding the union would have been the twenty-eight loop locals with a new
//! name — the ambient bag this design exists to dismantle — and every phase
//! would still have had to state which fields it may touch, in prose, with
//! nothing checking it.
//!
//! An explicit parameter list is the checked version of that statement. Where a
//! phase takes `&ExecutionHistory` it cannot write history; where it takes
//! `EnvironmentState` by value the caller cannot read the state afterwards. That
//! is the property the eventual `LoopState` has to preserve, and it is easier to
//! preserve from here than from a struct that had already erased it.
//!
//! # The cursor type that was here, and why it is not
//!
//! A `PhaseCx { phase, iteration }` carrying [`super::outcome::Phase`] lived in
//! this module until 2026-08-26, with an `expect_phase` guard so a phase could
//! refuse to run in a position it was not called for. Nothing ever constructed
//! one. The resident driver sequences phases by falling through code, so it
//! cannot present a stale cursor; the worker driver that can is the increment
//! after this one, and it does not exist yet.
//!
//! It was deleted rather than left in place, because a type carrying a guard
//! nobody invokes reads as a protection that is in force. When the worker driver
//! lands, the check has to come back with it — [`super::outcome::Phase::ORDER`]
//! and [`super::outcome::Phase::next_in_iteration`] already carry the ordering,
//! so what has to be rebuilt is the assertion, not the knowledge.

// Each phase's entry point is `pub(in …::agentic)`, not `pub`.
//
// Not tidiness: `resolve::run` and `apply`'s `gate`/`dispatch` pair take
// `TrustDispatchGuard` and `AgenticToolLineageState`, which are `pub(super)` in
// `executor.rs`. A `pub`
// function taking a type no outside caller can name is what
// `private_interfaces` warns about, and the honest reading of that warning is
// that the function was over-published, not that the types were under-published.
// The other four are scoped the same way so the set has one rule rather than a
// rule and four exceptions.
pub mod apply;
pub mod decide;
pub mod decision_rail;
mod decision_rail_events;
pub mod epilogue;
pub mod observe;
// Not a phase: the producer half of the event outbox, which every phase that
// emits a transport event now goes through. It sits here rather than beside
// `journal.rs` because the emission sites are here and because it depends on the
// emitter's own event vocabulary — which `journal.rs` deliberately does not, so
// that the log never becomes a second definition of an event.
pub mod outbox;
pub mod prepare;
pub mod resolve;
