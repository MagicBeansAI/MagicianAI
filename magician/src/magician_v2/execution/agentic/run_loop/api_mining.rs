//! What this run observed while mining APIs.
//!
//! Six fields off `ActionExecutors`. Like [`super::browser::BrowserRunState`],
//! these keep their individual `Arc`s: **four** of them are handed by reference
//! into `PrimitiveExecCtx::with_api_mining_runtime` — `replay_last_outcome`,
//! `last_page_url`, `action_events`, `sequence_recorder` — and the mining stages
//! write through that reference as a browser session produces traffic. (The
//! other two arguments that call takes, `api_router` and `api_mining_base_path`,
//! are not members of this group.) The group is a
//! naming move so `ActionExecutors` ends up holding rebuildable service handles
//! and nothing per-execution.
//!
//! # Two members are currently dead, and that is recorded rather than hidden
//!
//! `last_browser_state_hash` and `xhr_validation_metrics` are declared and
//! constructed and then **never read or written** anywhere in the workspace. The
//! field audit classified both as per-execution state that must move into
//! `LoopState`, which is right about what they are for and wrong about what they
//! currently do.
//!
//! They are grouped here anyway rather than deleted: they belong with their
//! siblings if anything ever wires them up, and deleting live-looking fields on a
//! branch several sessions are working in is how a half-built feature loses the
//! half that was already there. But a reader should not have to grep to discover
//! that two of these six have no consumer.

use std::sync::{Arc, Mutex};

/// The mining state of one execution.
#[derive(Clone, Default)]
pub struct ApiMiningRunState {
    /// Outcome of the most recent replay attempt this run made.
    pub replay_last_outcome:
        Arc<Mutex<Option<crate::magician_v2::execution::agentic::executor::ApiReplayMeta>>>,

    /// The last page URL this run saw, used to attribute captured traffic.
    pub last_page_url: Arc<Mutex<Option<String>>>,

    /// Action events accumulated for this execution's mining trace.
    pub action_events: Arc<Mutex<Vec<crate::magician_v2::api_mining::correlator::ActionEvent>>>,

    /// The sequence recorder capturing this execution's request order.
    pub sequence_recorder:
        Arc<Mutex<Option<crate::magician_v2::api_mining::sequence_recorder::SequenceRecorder>>>,

    /// The last browser-state hash this run observed.
    ///
    /// **No consumer today** — see the module docs.
    pub last_browser_state_hash: Arc<Mutex<Option<u64>>>,

    /// XHR validation metrics for this execution.
    ///
    /// **No consumer today** — see the module docs.
    pub xhr_validation_metrics:
        Arc<Mutex<crate::magician_v2::api_mining::types::XhrValidationMetrics>>,
}

impl std::fmt::Debug for ApiMiningRunState {
    /// Hand-written because the recorder and metrics types are not `Debug`, and
    /// because printing captured traffic into a log line is not something this
    /// type should make easy.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiMiningRunState")
            .field(
                "action_events",
                &self
                    .action_events
                    .lock()
                    .map(|guard| guard.len())
                    .unwrap_or_else(|poisoned| poisoned.into_inner().len()),
            )
            .finish_non_exhaustive()
    }
}

impl ApiMiningRunState {
    /// How many action events this run has captured.
    ///
    /// Used by the `Debug` impl above, which is its only caller — the
    /// orchestrator locks `action_events` directly. Kept rather than inlined
    /// because the `Debug` impl must not be the place a lock policy is decided.
    pub fn action_event_count(&self) -> usize {
        self.action_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_captured_count_tracks_the_shared_vector() {
        // The earlier version asserted only that a default group counts zero,
        // which `Vec::len` guarantees. What is worth pinning is that the count
        // reads the SHARED vector — the one the mining stages fill through the
        // `Arc` handed to `PrimitiveExecCtx` — rather than some copy.
        let state = ApiMiningRunState::default();
        assert_eq!(state.action_event_count(), 0);

        // A real event, built from JSON. `ActionEvent` has no `Default`, and
        // reaching for one is how this branch already shipped a test that did
        // not compile — the same mistake, in the same shape, twice.
        let held_by_exec_ctx = Arc::clone(&state.action_events);
        held_by_exec_ctx.lock().unwrap().push(
            serde_json::from_str(
                r#"{"action_id":"action-1","action_type":"Click","timestamp_ms":0}"#,
            )
            .expect("a minimal action event"),
        );

        assert_eq!(
            state.action_event_count(),
            1,
            "the count must observe writes made through the shared handle"
        );
    }

    #[test]
    fn the_group_shares_by_reference_not_by_copy() {
        // REGRESSION GUARD, same as the browser group. These are handed into
        // `PrimitiveExecCtx::with_api_mining_runtime` and the mining stages write
        // through them as traffic arrives. A group that copied would leave the
        // loop reading an empty trace while the stages filled an orphan.
        let state = ApiMiningRunState::default();
        let held_by_exec_ctx = Arc::clone(&state.last_page_url);

        *state.last_page_url.lock().unwrap() = Some("https://example.test".to_string());

        assert_eq!(
            held_by_exec_ctx.lock().unwrap().as_deref(),
            Some("https://example.test"),
            "a reference taken before the write must observe it"
        );
    }

    #[test]
    fn the_debug_rendering_names_only_the_field_it_declares() {
        // Captured traffic can carry anything a page sent — an observed URL with
        // a token in it, a replayed request body. A derived `Debug` would put all
        // of it in any log line that formats the executors.
        //
        // Asserted as an ALLOWLIST, matching `controls.rs`. The earlier form
        // inserted a URL and grepped the render for it, which covered 1 of 6
        // fields: adding `.field("replay_last_outcome", ..)` — the most likely
        // future edit, and `ApiReplayMeta` carries `replay_url`/`replay_body` —
        // would have leaked traffic into every such log line with this test
        // still green.
        let state = ApiMiningRunState::default();
        *state.last_page_url.lock().unwrap() = Some("https://secret.test/token=abc".to_string());

        let rendered = format!("{state:?}");
        assert!(
            rendered.contains("action_events"),
            "the count this type does render must still be there; got {rendered}"
        );
        for withheld in [
            "last_page_url",
            "replay_last_outcome",
            "sequence_recorder",
            "last_browser_state_hash",
            "xhr_validation_metrics",
            "secret.test",
        ] {
            assert!(
                !rendered.contains(withheld),
                "`{withheld}` may carry observed traffic and must not be \
                 rendered; got {rendered}"
            );
        }
    }
}
