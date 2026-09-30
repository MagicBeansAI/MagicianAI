//! What this run did with the browser, and what cleanup owes it.
//!
//! Five fields off `ActionExecutors`. See
//! `docs/archive/plans/2026-08-25-stateless-loop-design.md`.
//!
//! # Why this group keeps its `Arc`s
//!
//! [`super::state::ExecutorRunIdentity`] and [`super::outputs::RunOutputs`] both
//! own their data outright, because nothing outside the loop writes them. These
//! are different: **four** of them are handed **by reference** into
//! `PrimitiveExecCtx` — `with_browser_session_used`,
//! `with_browser_additional_session_ids`, and the two keep-alive overrides — and
//! the browser dispatcher writes through that reference. The fifth,
//! `session_id_override`, is read and CLONED into a plain `Option<String>`
//! parameter, so a later write to it is not visible to a dispatch already built.
//!
//! For the four that ARE shared, the sharing is the mechanism rather than an
//! accident of how the field was declared: `session_used` is set deep inside a
//! dispatch and read at outer-loop terminal time.
//!
//! So this group is a **naming** move, not a locking one. The fields become one
//! named per-execution value instead of five loose ones on a struct that is
//! supposed to hold rebuildable service handles, and every `Arc` stays exactly
//! where it was. Flattening them would break the write-through that makes
//! terminal cleanup know whether a Chrome window was ever opened.
//!
//! The boundary is served by [`BrowserRunSnapshot`] instead — a plain value taken
//! from the live state and merged into a fresh one on the far side. That split is
//! honest about the two different things being asked for: shared mutability for
//! the dispatch that is running, and a value for the worker that is not.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The live browser state of one execution.
#[derive(Debug, Clone, Default)]
pub struct BrowserRunState {
    /// Set the first time an inner-loop browser dispatch resolves a CLI path for
    /// this execution — i.e. this execution actually opened an `agent-browser`
    /// Chrome window.
    ///
    /// Read at outer-loop terminal time to decide whether to fire
    /// `agent-browser --session … close`. Never set for non-browser tasks, which
    /// is why cleanup costs them nothing.
    pub session_used: Arc<AtomicBool>,

    /// Sessions this run owns beyond the ordinary per-execution one.
    ///
    /// Retrieval handoffs may transfer a unique browser session to the flat
    /// agent; those ids are tracked separately so terminal cleanup can close
    /// every owned session rather than only the obvious one.
    pub additional_session_ids: Arc<Mutex<BTreeSet<String>>>,

    /// Set by the terminal `yield` handler when the LLM's yield carries
    /// `keep_browser_cdp_connection_alive=true` (or the legacy
    /// `keep_browser_session_alive` alias).
    ///
    /// Read at terminal time alongside the static
    /// `AgenticContext::keep_browser_cdp_connection_alive` and the outcome
    /// classifier; ANY of those being true means the runtime skips cleanup and
    /// leaves the agent's CDP socket attached. Sticky for the execution, reset on
    /// cleanup.
    pub keep_alive_override: Arc<AtomicBool>,

    /// Set by the terminal `yield` handler when the yield carries
    /// `keep_browser_window_open=true`.
    ///
    /// When this fires and cdp-alive does NOT, cleanup invokes
    /// `agent-browser close --keep-browser`: the daemon exits and Chromium stays
    /// visible. **cdp-alive wins when both are set.**
    pub window_open_override: Arc<AtomicBool>,

    /// Pre-resolved `agent-browser --session` id.
    ///
    /// When set, the inner-loop browser dispatcher attaches to this existing
    /// session instead of spawning a fresh Chrome window keyed on its own
    /// execution id. `None` preserves the legacy per-execution derivation.
    pub session_id_override: Arc<Mutex<Option<String>>>,
}

/// The same state as a value, for a holder that is not this process.
///
/// Taken at a boundary and merged into a fresh state on the far side — see
/// [`BrowserRunState::merge_from`], which is monotone and is NOT an install. A
/// worker resuming an execution must know whether a Chrome window is open and
/// which sessions this run owns, or terminal cleanup either leaves a browser
/// running forever or closes a session it does not own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRunSnapshot {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub session_used: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub additional_session_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_alive_override: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub window_open_override: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id_override: Option<String>,
}

impl BrowserRunState {
    /// Read the whole group as one value.
    ///
    /// `SeqCst` on the flags, matching every existing store: they are read at
    /// terminal time against decisions made elsewhere in the run, and a relaxed
    /// read could miss a `keep_alive` set by a yield that has already returned.
    pub fn snapshot(&self) -> BrowserRunSnapshot {
        BrowserRunSnapshot {
            session_used: self.session_used.load(Ordering::SeqCst),
            additional_session_ids: self
                .additional_session_ids
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_else(|poisoned| poisoned.into_inner().clone()),
            keep_alive_override: self.keep_alive_override.load(Ordering::SeqCst),
            window_open_override: self.window_open_override.load(Ordering::SeqCst),
            session_id_override: self
                .session_id_override
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_else(|poisoned| poisoned.into_inner().clone()),
        }
    }

    /// Merge a snapshot into a **fresh** state. This is not an install.
    ///
    /// Renamed and re-documented 2026-08-26: it was called `restore` and
    /// described as installing a snapshot, and it does neither. Every member is
    /// monotone — the three flags are OR-ed, `additional_session_ids` extends,
    /// and `session_id_override` is only ever set. Nothing is ever cleared, so
    /// after `merge_from(&snap)` the state does NOT equal `snap` unless it
    /// started empty.
    ///
    /// # Correct on a fresh state, wrong on a live one
    ///
    /// Monotone is right for the case this exists to serve: a worker builds an
    /// empty `BrowserRunState` and merges what the committed record says the run
    /// already did. Losing a flag there leaks a browser or closes a window a
    /// yield asked to keep open.
    ///
    /// It is wrong in the other direction, which is why the name mattered. A run
    /// whose committed record says `session_id_override: None` — the parent
    /// window was withdrawn — keeps attaching to the stale window; one whose
    /// record says `keep_alive_override: false` still skips cleanup and leaks
    /// the browser. **Do not call this to reconcile a live state against a
    /// record.** No current worker path reconciles a checkpoint into a non-fresh
    /// browser state. If one is added, it needs a real install plus an explicit
    /// per-field rule for whether the record or the live state wins; this merge
    /// must not be reused for that different operation.
    ///
    /// # One member the exec ctx will not see
    ///
    /// The doc used to say every member is written through an `Arc` the
    /// dispatcher holds. Four are. `session_id_override` is **read and cloned**
    /// into `with_browser_session_id_override`, whose parameter is a plain
    /// `Option<String>` — so a merge that lands after `build_primitive_exec_ctx`
    /// has run is invisible to that dispatch, unlike the other four.
    pub fn merge_from(&self, snapshot: &BrowserRunSnapshot) {
        if snapshot.session_used {
            self.session_used.store(true, Ordering::SeqCst);
        }
        if snapshot.keep_alive_override {
            self.keep_alive_override.store(true, Ordering::SeqCst);
        }
        if snapshot.window_open_override {
            self.window_open_override.store(true, Ordering::SeqCst);
        }
        {
            let mut guard = self
                .additional_session_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.extend(snapshot.additional_session_ids.iter().cloned());
        }
        if let Some(session_id) = snapshot.session_id_override.as_ref() {
            let mut guard = self
                .session_id_override
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(session_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untouched_run_adds_nothing_to_the_wire() {
        let encoded =
            serde_json::to_string(&BrowserRunState::default().snapshot()).expect("serialize");
        assert_eq!(
            encoded, "{}",
            "a task that never opened a browser must not grow the record"
        );
    }

    #[test]
    fn what_cleanup_needs_survives_the_boundary() {
        // The failure is concrete in both directions: a resumed run that lost
        // `session_used` leaves a Chrome window running forever, and one that
        // lost `additional_session_ids` closes only the obvious session while a
        // handed-off one leaks.
        let live = BrowserRunState::default();
        live.session_used.store(true, Ordering::SeqCst);
        live.keep_alive_override.store(true, Ordering::SeqCst);
        live.additional_session_ids
            .lock()
            .unwrap()
            .insert("handoff-1".to_string());
        *live.session_id_override.lock().unwrap() = Some("parent-window".to_string());

        let encoded = serde_json::to_string(&live.snapshot()).expect("serialize");
        let restored: BrowserRunSnapshot = serde_json::from_str(&encoded).expect("read back");

        let far_side = BrowserRunState::default();
        far_side.merge_from(&restored);

        assert!(far_side.session_used.load(Ordering::SeqCst));
        assert!(far_side.keep_alive_override.load(Ordering::SeqCst));
        assert!(!far_side.window_open_override.load(Ordering::SeqCst));
        assert!(far_side
            .additional_session_ids
            .lock()
            .unwrap()
            .contains("handoff-1"));
        assert_eq!(
            far_side.session_id_override.lock().unwrap().as_deref(),
            Some("parent-window")
        );
    }

    #[test]
    fn merging_is_monotone_and_never_clears() {
        // REGRESSION GUARD for what this actually does, replacing a test that
        // asserted something the signature already guarantees: `merge_from`
        // takes `&self`, so it structurally CANNOT reassign an `Arc` field, and
        // "writes through rather than replacing" could never have failed.
        //
        // What can fail is the monotonicity, and it is the property callers must
        // know about: merging a record that says "no override, not kept alive"
        // onto a live state does NOT turn those off. A worker that used this to
        // reconcile a live state against a record would keep attaching to a
        // withdrawn window and would skip a cleanup the record says is due.
        let live = BrowserRunState::default();
        live.session_used.store(true, Ordering::SeqCst);
        live.keep_alive_override.store(true, Ordering::SeqCst);
        *live.session_id_override.lock().unwrap() = Some("parent-window".to_string());

        // An all-empty snapshot: every field says "off".
        live.merge_from(&BrowserRunSnapshot::default());

        let after = live.snapshot();
        assert!(
            after.session_used,
            "monotone: an empty snapshot clears nothing"
        );
        assert!(after.keep_alive_override);
        assert_eq!(
            after.session_id_override.as_deref(),
            Some("parent-window"),
            "the override survives a snapshot that does not name one; merging is \
             not installing, which is why this is not called `restore`"
        );
    }

    #[test]
    fn a_restore_never_clears_a_sticky_keep_alive() {
        // These flags mean "something in this run asked the browser to stay".
        // A segment whose yield asked to keep the window open must not have that
        // undone by a later restore carrying an older snapshot — the visible
        // result would be a window closing under a user who asked for it.
        let live = BrowserRunState::default();
        live.keep_alive_override.store(true, Ordering::SeqCst);
        live.window_open_override.store(true, Ordering::SeqCst);

        live.merge_from(&BrowserRunSnapshot::default());

        assert!(
            live.keep_alive_override.load(Ordering::SeqCst),
            "a snapshot that predates the yield must not clear its request"
        );
        assert!(live.window_open_override.load(Ordering::SeqCst));
    }

    #[test]
    fn a_poisoned_lock_still_reports_the_sessions_this_run_owns() {
        // Terminal cleanup reads this to decide what to close. Dropping it
        // because an unrelated panic poisoned the lock leaks every handed-off
        // session — the same reasoning as the identity's ceiling.
        let live = BrowserRunState::default();
        live.additional_session_ids
            .lock()
            .unwrap()
            .insert("handoff-1".to_string());

        let poisoner = Arc::clone(&live.additional_session_ids);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the session set");
        })
        .join();

        assert!(
            live.snapshot().additional_session_ids.contains("handoff-1"),
            "a poisoned lock must not hide a session cleanup still owns"
        );
    }
}
