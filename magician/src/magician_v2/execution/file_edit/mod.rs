//! File-edit approvals for the coding-agent surface.
//!
//! When the LLM proposes a code change (via `edit_file`, `apply_patch`,
//! …), the tool computes the resulting file contents *in memory*, runs
//! [`diff::compute_unified_diff`] against the existing content, and
//! stages a [`transaction::FileEditTransaction`] without writing
//! anything to disk. The transaction id rides in a
//! [`HitlRequested { input_type: "diff_approval" }`] event; the chat
//! UI renders the diff via the `DiffStrip` component and gates the
//! actual write on operator approval.
//!
//! On apply: the runtime takes a [`snapshot::Snapshot`] of the
//! affected paths (so revert is possible), then writes the queued
//! contents atomically. On reject: the staged transaction is dropped
//! with no disk mutation.
//!
//! This module is **pure-additive** — it does not replace any existing
//! file-write path. Tools that want approval-gated edits opt in by
//! staging through this layer; tools that don't (e.g. internal
//! workspace bootstrap) continue to write directly as before.
//!
//! Module layout:
//!   - [`diff`] — unified-diff computation via the `similar` crate
//!     (no `git` shell-out required; works on any text content).
//!   - [`transaction`] — `FileEditTransaction` lifecycle: stage, load,
//!     apply, reject. Persistence under `<scope>/transactions/`.
//!   - [`proposal`] — `CodeChangeProposal` lifecycle for Pi-backed
//!     shadow-workspace diffs. Persistence under
//!     `<scope>/code_change_proposals/`.
//!   - [`snapshot`] — pre-apply content snapshots for revert. Stored
//!     under `<scope>/snapshots/`.

pub mod checkpoint;
pub mod diff;
pub mod proposal;
pub mod snapshot;
pub mod transaction;
