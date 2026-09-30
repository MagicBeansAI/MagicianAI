//! What a human widened for this run, and why it must not outlive the process.
//!
//! Two fields off `ActionExecutors`. Both are named *session* and are actually
//! per-execution — their own declarations said so before this move: *"for this
//! execution only"* and *"for the duration of the execution run"*. The field
//! audit's open question about their scope is answered in
//! `docs/archive/plans/2026-08-26-scratch-extraction-field-audit.md`.
//!
//! # These are grants, not ceilings, and that inverts everything
//!
//! Every other group in this module wants to survive a boundary. These must not,
//! and the asymmetry is the point:
//!
//! - A **ceiling** narrows. Losing one widens the run, so it has to be carried,
//!   and a tampered record can only be as bad as no record at all.
//! - A **grant** widens. Losing one narrows the run — the user is asked again —
//!   which fails **closed**. A tampered record that forged a sandbox root would
//!   hand a run filesystem access nobody approved.
//!
//! Neither field exists on `AgenticPauseState` today, so a restart already
//! re-prompts. Carrying them safely would need the `approved_confirmation_actions`
//! treatment — covered by `authorization_hash` **and** elevation-bearing so the
//! hash is actually verified — and that is worse than the status quo: the
//! in-process authority entry does not survive a restart, so such a pause could
//! not resume at all and the user's work would be discarded rather than merely
//! re-asked.
//!
//! # How that is enforced
//!
//! By the compiler. [`RunGrants`] deliberately derives **neither `Serialize` nor
//! `Deserialize`**, so no boundary record can contain one by accident — a future
//! `LoopState` that tried to embed it would not build. That is a stronger
//! guarantee than a `#[serde(skip)]` a later edit could quietly remove, and it is
//! why there is no snapshot type here beside the live one, unlike
//! [`super::browser::BrowserRunSnapshot`].

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Authority a human granted to this execution while it was running.
///
/// The `Arc`s are kept inside the group rather than flattened, for the same
/// reason as [`super::browser::BrowserRunState`]:
/// `session_file_sandbox_roots` is handed by reference into `PrimitiveExecCtx`
/// (`with_session_file_sandbox_roots`), and a HITL merge has to be visible to the
/// dispatch that retries. Flattening it to a plain `HashSet` would leave the
/// retry unable to see the roots the user had just approved.
#[derive(Debug, Clone, Default)]
pub struct RunGrants {
    /// Extra native-file roots approved through HITL for this execution only.
    ///
    /// Written when a `SandboxOverride` pause is answered `allow_once`, and read
    /// by the file-action sandbox check. Shared by reference into the exec ctx so
    /// the retry that follows the approval can see it.
    pub session_file_sandbox_roots: Arc<Mutex<HashSet<String>>>,

    /// Tools authorized via "Allow for This Run".
    ///
    /// Written when a `ToolAuthorization` pause is answered `allow_always`, and
    /// read to skip future authorization prompts for the same tool within this
    /// execution. `allow_once` deliberately writes nothing — that is what makes
    /// it once.
    pub session_tool_allowlist: Arc<Mutex<HashSet<String>>>,
}

impl RunGrants {
    /// Whether a tool has already been authorized for this run.
    ///
    /// Recovers from a poisoned lock rather than treating it as "not authorized".
    /// Both answers are safe here — the worst case of recovering is one skipped
    /// re-prompt for a tool the user did approve — but silently re-asking after a
    /// user chose "Allow for This Run" reads as the setting not working.
    pub fn tool_is_allowed(&self, tool_name: &str) -> bool {
        self.session_tool_allowlist
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(tool_name)
    }

    /// Record an "Allow for This Run" answer.
    pub fn allow_tool(&self, tool_name: impl Into<String>) {
        self.session_tool_allowlist
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(tool_name.into());
    }

    /// Merge roots approved by a HITL sandbox override.
    ///
    /// Extends rather than replaces: a run may be granted more than one root over
    /// its life, and a second approval must not revoke the first.
    pub fn allow_sandbox_roots(&self, roots: impl IntoIterator<Item = String>) {
        self.session_file_sandbox_roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(roots);
    }

    /// The roots granted so far.
    pub fn sandbox_roots(&self) -> HashSet<String> {
        self.session_file_sandbox_roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_can_never_reach_a_boundary_record() {
        // The module docs claim the compiler enforces this. That claim is only
        // true while nobody adds the derives, and a claim about what does not
        // compile is exactly the kind that rots silently — a `#[derive(…)]`
        // added for some unrelated convenience would take a security property
        // with it and break no test.
        //
        // What is at stake: these are GRANTS. Losing one re-prompts, which fails
        // closed. Forging one through a tampered record hands a run filesystem
        // access nobody approved. That asymmetry is why this type is the one
        // member of `run_loop` that must not be serializable.
        static_assertions::assert_not_impl_any!(
            RunGrants: serde::Serialize,
            serde::de::DeserializeOwned
        );
    }

    #[test]
    fn an_allow_for_this_run_is_remembered_within_the_run() {
        let grants = RunGrants::default();
        assert!(!grants.tool_is_allowed("shell"));
        grants.allow_tool("shell");
        assert!(grants.tool_is_allowed("shell"));
    }

    #[test]
    fn a_second_sandbox_approval_does_not_revoke_the_first() {
        // A run can be granted more than one root over its life. Replacing rather
        // than extending would silently withdraw a root the user approved earlier
        // and the next file action under it would fail for no visible reason.
        let grants = RunGrants::default();
        grants.allow_sandbox_roots(["/tmp/first".to_string()]);
        grants.allow_sandbox_roots(["/tmp/second".to_string()]);

        let roots = grants.sandbox_roots();
        assert!(roots.contains("/tmp/first"));
        assert!(roots.contains("/tmp/second"));
    }

    #[test]
    fn the_grants_are_shared_by_reference_not_copied() {
        // REGRESSION GUARD. `session_file_sandbox_roots` is handed into
        // `PrimitiveExecCtx` by reference so the dispatch that retries after a
        // HITL approval can see the root just granted. A group that copied
        // instead of sharing would leave that retry looking at a stale set and
        // failing on the path the user had explicitly allowed.
        let grants = RunGrants::default();
        let held_by_exec_ctx = Arc::clone(&grants.session_file_sandbox_roots);

        grants.allow_sandbox_roots(["/tmp/approved".to_string()]);

        assert!(
            held_by_exec_ctx.lock().unwrap().contains("/tmp/approved"),
            "a reference taken before the approval must observe it"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_silently_re_prompt() {
        // Recovering is safe in both directions here; the reason to recover is
        // that a user who chose "Allow for This Run" and is asked again reads it
        // as the setting being broken.
        let grants = RunGrants::default();
        grants.allow_tool("shell");

        let poisoner = Arc::clone(&grants.session_tool_allowlist);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the allowlist");
        })
        .join();

        assert!(
            grants.tool_is_allowed("shell"),
            "an unrelated panic must not un-approve what the user approved"
        );
    }

    // There is deliberately no round-trip test here, and no snapshot type.
    // `RunGrants` derives neither `Serialize` nor `Deserialize`, so "these never
    // reach a boundary record" is enforced by the compiler rather than asserted
    // at runtime: a `LoopState` that tried to embed one would not build. See the
    // module docs for why a forged grant is the failure being designed out.
}
