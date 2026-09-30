//! How something outside a run reaches into it, and the one fact a run keeps
//! about its own decisions.
//!
//! Five fields off `ActionExecutors`, and they are **not the same kind of thing**
//! despite sharing a classification. Extracting the earlier groups made the split
//! visible; it is recorded in
//! `docs/archive/plans/2026-08-26-scratch-extraction-field-audit.md`.
//!
//! - [`RunControls::manual_pause_signal`] and [`RunControls::steer_queue`] are
//!   **inbound channels**. They are not state the run owns — they are how an
//!   operator reaches a run already in flight, via `/executions/{id}/steer` or a
//!   cooperative pause request. The `Arc` is the delivery mechanism, and a worker
//!   cannot be handed a delivery mechanism as a value. On the explicit
//!   `inprocess` rollback arm, the queue is still the one resident delivery
//!   path. On the stateless arm it is not authoritative: accepted redirects are
//!   written to the separately sealed, scoped `steer_inbox` and a Decide
//!   boundary claims/commit-couples/acknowledges them durably.
//!
//! - [`RunControls::app_labeled_tool_results`] is an **in-flight handoff** —
//!   inserted after server labeling and removed by the owning loop, both inside
//!   one dispatch. It does not outlive the operation that creates it and must not
//!   reach a boundary record.
//!
//! - [`RunControls::shell_stream_ctx`] is a **live handle plus derived
//!   attribution**. The broadcaster itself cannot cross a boundary. The stable
//!   base step id is recomposed from the canonical runtime execution, and every
//!   phase entry derives the current iteration's `step_id`/`step_index` from the
//!   committed cursor. Cold Resolve/Apply entry therefore cannot retain the
//!   constructor's index zero merely because it skipped Prepare.
//!
//! - [`RunControls::decision_provider_is_yutori`] is the only one that is
//!   ordinary per-execution state. It gates the Yutori N1.5 action-translation
//!   shim in browser dispatch, so a worker that was not told it would translate
//!   actions for the wrong provider.
//!
//! So this group is a naming move that keeps every `Arc` where it is. What it
//! buys is that `ActionExecutors` stops carrying five loose per-execution fields
//! and a reader can see, in one place, that **the live handles** deliberately do
//! not cross a boundary. Their durable consequences, when any, travel through
//! the purpose-built state/inbox records rather than by serializing an `Arc`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The control surface of one execution.
#[derive(Clone, Default)]
pub struct RunControls {
    /// Cooperative manual-pause signal for exact continuation.
    ///
    /// `None` is meaningful and load-bearing: a delegated child is built with
    /// `without_manual_pause_signal`, because a resident pause aimed only at the
    /// parent must not leak through shared executors. A stateless tree pause can
    /// still address that child explicitly: the worker validates its exact
    /// durable tree member and passes that disposition directly to settlement,
    /// without relying on this optional inherited signal.
    pub manual_pause_signal: Option<Arc<AtomicBool>>,

    /// Resident-arm operator steer queue. Under `inprocess`, redirects pushed
    /// via `/executions/{id}/steer` are drained into the next decision turn as
    /// `[OPERATOR STEER]` user turns. Stateless admission uses the sealed
    /// store-backed inbox instead and must not dual-publish here.
    pub steer_queue: Option<Arc<Mutex<Vec<String>>>>,

    /// Shell streaming context for the current step.
    ///
    /// **Corrected 2026-08-26.** This was documented as "set before executing a
    /// bash action and cleared after". It is neither: it is set ONCE at run setup
    /// (`executor.rs`, guarded on an event broadcaster and a step id), mutated
    /// per iteration by `phases::prepare` to carry the current `step_index` and
    /// `step_id`, and read at bash dispatch. **There is no clear site anywhere.**
    ///
    /// That matters because the false claim was the whole argument for calling
    /// this an in-flight handoff rather than per-execution state. The real reason
    /// it does not cross a boundary is different and simpler: it holds an
    /// `Arc<RuntimeTransportBroadcaster>`, a live event-bus handle that no record
    /// can carry.
    ///
    /// The iteration suffix and index are derived at *every* worker phase entry,
    /// not only Prepare. That distinction is load-bearing for a cold Resolve or
    /// Apply recovery, which legitimately does not execute Prepare first.
    pub shell_stream_ctx:
        Arc<Mutex<Option<crate::magician_v2::execution::native_executors::ShellStreamContext>>>,

    /// Runtime-only exact-result handoff for governed app workflows.
    ///
    /// The dispatcher inserts after server labeling; the owning loop removes it
    /// before any generic artifact, evidence or projection sink sees the bytes.
    /// The same in-flight class as `shell_stream_ctx`: inserted and removed
    /// inside one dispatch, so it never outlives the operation that creates it
    /// and must never reach a boundary record — a resumed worker has no
    /// dispatcher waiting to collect the entry, so it would simply leak labeled
    /// bytes into a record the loop's own contract keeps them out of.
    ///
    /// **Deliberately not `pub`.** It was a private field on `ActionExecutors`,
    /// reachable only through store/take/clear, and that privacy carried the
    /// invariant in the paragraph above: the owning loop must remove the entry
    /// before any sink sees the bytes. Publishing the map as part of a group
    /// would let any holder read a labeled result and hand it onward, turning a
    /// type-enforced rule back into a convention. The three methods below are the
    /// whole surface.
    app_labeled_tool_results: Arc<
        Mutex<
            std::collections::HashMap<
                String,
                crate::magician_v2::apps::tool_disclosure::AppLabeledToolResultRecord,
            >,
        >,
    >,

    /// Whether the most recent agentic decision came from the Yutori provider.
    ///
    /// Set after each decision from the call telemetry; read by
    /// `build_primitive_exec_ctx` so the browser dispatcher enables the Yutori
    /// N1.5 action-translation shim **only** for Yutori-driven runs. The one
    /// member of this group a resumed holder must actually be told.
    pub decision_provider_is_yutori: Arc<AtomicBool>,
}

/// Restores the caller's shell attribution after a nested same-owner run.
///
/// The nested executor shares [`RunControls`] and installs its own base step
/// during bootstrap. Keeping restoration in a drop guard makes an early error
/// or unwind no more capable of leaking that nested identity into the parent.
pub(crate) struct ShellStreamContextRestore {
    target: Arc<Mutex<Option<crate::magician_v2::execution::native_executors::ShellStreamContext>>>,
    saved: Option<crate::magician_v2::execution::native_executors::ShellStreamContext>,
}

impl Drop for ShellStreamContextRestore {
    fn drop(&mut self) {
        let mut guard = self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = self.saved.take();
    }
}

impl std::fmt::Debug for RunControls {
    /// Hand-written for two reasons. `ShellStreamContext` derives no `Debug`, so
    /// a derive here would not compile — and even if it did, the two in-flight
    /// members carry a live command's streaming output and server-labeled app
    /// results. Neither belongs in whatever log line formats the executors, so
    /// this renders whether each channel is wired, plus the provider flag —
    /// which is not a channel, but is the one member here a resumed holder must
    /// actually be told, so it is worth seeing in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunControls")
            .field("manual_pause_signal", &self.manual_pause_signal.is_some())
            .field("steer_queue", &self.steer_queue.is_some())
            .field("provider_is_yutori", &self.provider_is_yutori())
            .finish_non_exhaustive()
    }
}

impl RunControls {
    /// Preserve the exact shell attribution currently owned by an outer run.
    ///
    /// Same-owner sub-goals intentionally reuse the executor bundle, so their
    /// bootstrap overwrites this shared cell. The caller keeps the returned
    /// guard across the recursive invocation and drops it before the parent can
    /// dispatch another shell action.
    pub(crate) fn preserve_shell_stream_context(&self) -> ShellStreamContextRestore {
        let saved = self
            .shell_stream_ctx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        ShellStreamContextRestore {
            target: Arc::clone(&self.shell_stream_ctx),
            saved,
        }
    }

    /// Whether an operator has asked this run to pause.
    ///
    /// A run with no signal wired answers `false`, which is the correct reading
    /// of "nobody can address this run" rather than a missing check.
    pub fn manual_pause_requested(&self) -> bool {
        self.manual_pause_signal
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::SeqCst))
    }

    /// Mirror durable manual-pause authority into a resident signal when this
    /// execution owns one. This is a convenience for in-phase readers, not the
    /// durable disposition: addressed children may deliberately have no signal,
    /// so their worker carries the validated pause directly into settlement.
    pub fn request_manual_pause(&self) {
        if let Some(signal) = self.manual_pause_signal.as_ref() {
            signal.store(true, Ordering::SeqCst);
        }
    }

    /// Whether the last decision came from Yutori.
    pub fn provider_is_yutori(&self) -> bool {
        self.decision_provider_is_yutori.load(Ordering::Relaxed)
    }

    /// Record which provider produced the decision just taken.
    pub fn set_provider_is_yutori(&self, is_yutori: bool) {
        self.decision_provider_is_yutori
            .store(is_yutori, Ordering::Relaxed);
    }

    /// Bind shell-stream attribution to the committed iteration being entered.
    ///
    /// The context is initialized before the stateless driver loads its cursor,
    /// so its initial `step_index == 0` cannot be authoritative. Prepare calls
    /// this on ordinary turns, and the worker host also calls it before every
    /// phase so cold Resolve/Apply entry receives the same identity. Re-deriving
    /// from the stable prefix keeps the operation idempotent across phase
    /// retries instead of producing `...-iter-7-iter-7`.
    pub fn set_shell_stream_iteration(&self, iteration: usize) {
        let mut guard = self
            .shell_stream_ctx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(ctx) = guard.as_mut() else {
            return;
        };
        ctx.step_index = iteration;
        ctx.step_id = format!("{}-iter-{iteration}", ctx.base_step_id);
    }

    /// Stash a server-labeled app result for the owning loop to collect.
    ///
    /// Three refusals, all of them load-bearing and none of them incidental:
    ///
    /// - a **poisoned lock** is an error rather than a recovery, unlike every
    ///   other accessor on this type. The rest are reads whose worst case is a
    ///   stale answer; this one hands labeled bytes to a map the loop promises to
    ///   drain, and a caller that believes the stash succeeded when it did not
    ///   would leave the result to be collected by nobody.
    /// - a **conflict** on an invocation ref means two dispatches claimed the
    ///   same handoff slot. Overwriting would silently discard one workflow's
    ///   result and hand the other's to whoever collects.
    /// - a **capacity** bound, because entries are removed by collection and an
    ///   uncollected one is a leak; unbounded growth would turn a missed
    ///   collection into a memory fault instead of an error.
    pub fn store_app_labeled_result(
        &self,
        invocation_ref: &str,
        record: crate::magician_v2::apps::tool_disclosure::AppLabeledToolResultRecord,
    ) -> anyhow::Result<()> {
        const MAX_PENDING_APP_RESULTS: usize = 64;
        let mut results = self
            .app_labeled_tool_results
            .lock()
            .map_err(|_| anyhow::anyhow!("app_workflow_result_handoff_unavailable"))?;
        if results.contains_key(invocation_ref) {
            return Err(anyhow::anyhow!("app_workflow_result_handoff_conflict"));
        }
        if results.len() >= MAX_PENDING_APP_RESULTS {
            return Err(anyhow::anyhow!("app_workflow_result_handoff_capacity"));
        }
        results.insert(invocation_ref.to_owned(), record);
        Ok(())
    }

    /// Collect a stashed result, removing it.
    ///
    /// Taking rather than reading is the invariant: the entry must not survive
    /// its collection, or a later sink could still find it.
    pub fn take_app_labeled_result(
        &self,
        invocation_ref: &str,
    ) -> Option<crate::magician_v2::apps::tool_disclosure::AppLabeledToolResultRecord> {
        self.app_labeled_tool_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(invocation_ref)
    }

    /// Drop a stashed result nobody will collect.
    pub fn clear_app_labeled_result(&self, invocation_ref: &str) {
        self.app_labeled_tool_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(invocation_ref);
    }

    /// Take every steer an operator has pushed and not yet had delivered.
    ///
    /// Draining, not reading: a steer delivered twice would read to the model as
    /// the operator repeating themselves, and the queue is the only record that
    /// it was already handed over.
    pub fn drain_steers(&self) -> Vec<String> {
        let Some(queue) = self.steer_queue.as_ref() else {
            return Vec::new();
        };
        let mut guard = queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_without_a_resident_pause_channel_is_not_locally_paused() {
        // `None` means no inherited resident channel was wired to this run — a
        // delegated child, typically. A durable exact tree request is carried
        // separately by its worker and must not make this shared local read true.
        assert!(!RunControls::default().manual_pause_requested());
    }

    #[test]
    fn a_pause_aimed_at_this_run_is_seen() {
        let signal = Arc::new(AtomicBool::new(false));
        let controls = RunControls {
            manual_pause_signal: Some(Arc::clone(&signal)),
            ..RunControls::default()
        };
        assert!(!controls.manual_pause_requested());

        // The operator's HTTP handler holds the other end.
        signal.store(true, Ordering::SeqCst);
        assert!(
            controls.manual_pause_requested(),
            "the Arc is the delivery mechanism; a copy would never see this"
        );
    }

    #[test]
    fn steers_are_delivered_once() {
        // REGRESSION GUARD. Draining rather than reading is the whole contract:
        // the queue is the only record that a redirect was already handed to the
        // model, so a read that left the entry behind would replay the operator's
        // instruction on every subsequent turn.
        let queue = Arc::new(Mutex::new(vec!["focus on pricing".to_string()]));
        let controls = RunControls {
            steer_queue: Some(Arc::clone(&queue)),
            ..RunControls::default()
        };

        assert_eq!(
            controls.drain_steers(),
            vec!["focus on pricing".to_string()]
        );
        assert!(
            controls.drain_steers().is_empty(),
            "a steer already delivered must not be delivered again"
        );
        assert!(
            queue.lock().unwrap().is_empty(),
            "the drain must empty the queue the operator pushes into, not a copy"
        );
    }

    #[test]
    fn a_run_with_no_steer_channel_drains_nothing() {
        assert!(RunControls::default().drain_steers().is_empty());
    }

    #[test]
    fn the_debug_rendering_names_only_the_fields_it_declares() {
        // The two in-flight members carry a live command's streaming output and
        // server-labeled app results; a derived `Debug` would put both into any
        // log line that formats the executors.
        //
        // Asserted as an allowlist rather than by inserting a payload and
        // grepping for it. An earlier cut did the latter and was doubly wrong:
        // `AppLabeledToolResultRecord` has no `Default` and private fields, so it
        // did not compile — and `finish_non_exhaustive` never touches the map, so
        // even had it compiled the assertion could not fail. A test that cannot
        // fail is worse than no test, because it reads as coverage.
        let controls = RunControls::default();
        let rendered = format!("{controls:?}");

        for named in ["manual_pause_signal", "steer_queue", "provider_is_yutori"] {
            assert!(rendered.contains(named), "expected {named} in {rendered}");
        }
        for withheld in ["app_labeled_tool_results", "shell_stream_ctx"] {
            assert!(
                !rendered.contains(withheld),
                "{withheld} carries in-flight payload and must not be rendered; \
                 got {rendered}"
            );
        }
    }

    #[test]
    fn the_provider_flag_round_trips() {
        // The one member a resumed holder must be told: it gates the Yutori
        // action-translation shim, so a worker that guessed would translate
        // browser actions for the wrong provider.
        let controls = RunControls::default();
        assert!(!controls.provider_is_yutori());
        controls.set_provider_is_yutori(true);
        assert!(controls.provider_is_yutori());
    }

    #[test]
    fn shell_stream_position_is_derived_idempotently_for_cold_phase_entry() {
        use crate::magician_v2::execution::native_executors::ShellStreamContext;
        use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;

        let controls = RunControls::default();
        *controls
            .shell_stream_ctx
            .lock()
            .expect("shell context lock") = Some(ShellStreamContext {
            broadcaster: Arc::new(RuntimeTransportBroadcaster::new(8)),
            execution_id: "execution-1".to_owned(),
            principal: Some("principal".to_owned()),
            workspace: Some("workspace".to_owned()),
            base_step_id: "plan-iter-section".to_owned(),
            step_id: "plan-iter-section".to_owned(),
            step_index: 0,
            scrub: None,
        });

        controls.set_shell_stream_iteration(7);
        controls.set_shell_stream_iteration(7);
        let guard = controls
            .shell_stream_ctx
            .lock()
            .expect("shell context lock");
        let context = guard.as_ref().expect("shell context");
        assert_eq!(context.step_index, 7);
        assert_eq!(context.step_id, "plan-iter-section-iter-7");
    }

    #[test]
    fn nested_shell_attribution_is_restored_exactly() {
        use crate::magician_v2::execution::native_executors::ShellStreamContext;
        use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;

        let controls = RunControls::default();
        *controls
            .shell_stream_ctx
            .lock()
            .expect("shell context lock") = Some(ShellStreamContext {
            broadcaster: Arc::new(RuntimeTransportBroadcaster::new(8)),
            execution_id: "execution-1".to_owned(),
            principal: Some("principal".to_owned()),
            workspace: Some("workspace".to_owned()),
            base_step_id: "parent-step".to_owned(),
            step_id: "parent-step-iter-4".to_owned(),
            step_index: 4,
            scrub: None,
        });

        let restore = controls.preserve_shell_stream_context();
        {
            let mut context = controls
                .shell_stream_ctx
                .lock()
                .expect("shell context lock");
            let context = context.as_mut().expect("shell context");
            context.base_step_id = "nested-step".to_owned();
            context.step_id = "nested-step-iter-2".to_owned();
            context.step_index = 2;
        }
        drop(restore);

        let context = controls
            .shell_stream_ctx
            .lock()
            .expect("shell context lock");
        let context = context.as_ref().expect("shell context");
        assert_eq!(context.base_step_id, "parent-step");
        assert_eq!(context.step_id, "parent-step-iter-4");
        assert_eq!(context.step_index, 4);
    }
}
