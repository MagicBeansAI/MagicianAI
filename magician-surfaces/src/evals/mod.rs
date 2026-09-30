//! Eval lane registry, readiness, runs, and the `/v2/evals` API.
//!
//! Lanes are DERIVED from `## eval:` annotations in the repo Makefile — one
//! source of truth, so there is no second list to drift. Uniform run records are
//! produced OUTSIDE the lanes (see later tasks), which is what lets all 31 evals
//! gain status/duration/history without touching any of them.
//!
//! [`run::EvalRun`] is the ONLY thing this feature persists, and
//! [`store::EvalRunStore`] keeps it append-only: everything else — the lane
//! list, readiness, cost, and whether a lane is running right now — is derived
//! at read time from a source that already owns it (the Makefile, live probes,
//! the LLM ledger, and the task system respectively), so none of them can go
//! stale here.
//!
//! [`runner`] starts a lane as an ordinary execution and guarantees that every
//! run leaves exactly one terminal record, however it ended; [`executor`] is the
//! production implementation of the task-system seam it runs against.

pub mod cost;
pub mod executor;
pub mod options;
pub mod provider_replay;
pub mod readiness;
pub mod registry;
pub mod run;
pub mod runner;
pub mod store;
