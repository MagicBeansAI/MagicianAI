//! The VibeDev cockpit, behind the lane seam as one module tree (plan
//! workstream 3.4; docs/plans/2026-08-26-platform-layering-and-app-
//! extraction-plan.md).
//!
//! This tree is the cockpit's product logic — seam-registered module form,
//! not an app package: the coding engines (`execution/coding_engine/`) stay
//! core infrastructure it calls, and `apps/vibedev_artifact_handoff.rs`
//! stays the platform integration that publishes its verified output.
//! Nothing here changed behavior in 3.4; the four flat modules below became
//! submodules so the product has one home, and `chat/service.rs` keeps turn
//! orchestration while the decisions a turn's VibeDev arm makes moved
//! behind this seam (see `rail`).
//!
//! * [`projects`] — the scoped `projects.json` substrate the HTTP surface
//!   and the run service share.
//! * [`dispatch_intent`] — the durable admission that makes starting a run
//!   idempotent and restart-recoverable.
//! * [`run_service`] — the one server-owned way a VibeDev run is created:
//!   prose, admission, atomic create, dispatch, rollback, and the
//!   coding-choice bounding a run carries.
//! * [`rail`] — the `@vibedev` chat rail: the invoke grammar's caller-side
//!   decisions, which sentence answers which outcome, and the lane triple
//!   a leading invoke mints.
//!
//! The pre-3.4 flat paths (`magician_v2::vibedev_run_service` and siblings)
//! were `pub use` re-export shims in `magician_v2/mod.rs` through the Phase 4
//! soak window; Phase 5 (removal inventory batch 2, 2026-08-28) removed them
//! and repointed every consumer to the tree paths above.

pub mod dispatch_intent;
pub mod projects;
pub mod rail;
pub mod run_service;
