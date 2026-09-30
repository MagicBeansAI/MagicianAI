//! Regression test for preplan resume resilience (audit #6 / #20 / #21).
//!
//! Guards the fix in `ask_loop/triggers.rs::on_clarification_received`: when the
//! in-memory pause queue has no live entry for a clarification `question_id`
//! (double-submit after the queue entry was dropped by `resume()`, a
//! genuinely-unknown id), the trigger surfaces a dismissable
//! `TriggerError::AlreadyResolved` only when no durable workflow identity is
//! available. When the caller supplies one, persisted session authority must
//! prove that the question is absent; unavailable authority stays retryable so
//! a valid post-restart answer is never silently discarded.
//!
//! This exercises the `None`-arm of `find_by_question` directly against a
//! minimal `PauseResumeManager` backed by an `InMemoryQueueRepository`, mirroring
//! the `build_service()` setup used by the in-crate tests in
//! `src/magician_v2/ask_loop/triggers.rs`.
//!
//! Run with: cargo test --test preplan_resume_resilience_tests

use std::sync::Arc;

use magician::magician_v2::{
    ask_loop::{
        clarifier::{ClarifierLibrary, DeterministicClarifier},
        history::ClarificationHistory,
        pause::{InMemoryQueueRepository, PauseResumeManager, QueueRepository, WaitingQueue},
        triggers::{ResumeTriggerService, TriggerError},
    },
    confidence::ConfidenceService,
    state_tracker::StateTracker,
    storage::{FileV2Store, V2ConversationStore},
};
use tempfile::TempDir;

/// Build a `ResumeTriggerService` over an in-memory pause queue with no wired
/// session manager (so a requested persisted-session fallback fails closed). Mirrors the
/// `build_service()` helper in the in-crate trigger tests.
fn build_service() -> (Arc<ResumeTriggerService>, TempDir) {
    let temp_dir = TempDir::new().expect("temp dir");
    let store = Arc::new(FileV2Store::new(temp_dir.path())) as Arc<dyn V2ConversationStore>;
    let confidence_service = Arc::new(ConfidenceService::default());
    let state_tracker = Arc::new(StateTracker::with_confidence_service(
        Arc::clone(&store),
        Arc::clone(&confidence_service),
    ));
    let repo: Arc<dyn QueueRepository> = Arc::new(InMemoryQueueRepository::default());
    let waiting_queue = Arc::new(WaitingQueue::new(repo));
    let pause_manager = Arc::new(PauseResumeManager::new(
        Arc::clone(&state_tracker),
        waiting_queue,
    ));
    let clarifier = Arc::new(ClarifierLibrary::with_default_templates(Arc::new(
        DeterministicClarifier,
    )));
    let history = Arc::new(ClarificationHistory::new());
    let resume_service = Arc::new(ResumeTriggerService::new(
        pause_manager,
        clarifier,
        None,
        None,
        history,
    ));
    (resume_service, temp_dir)
}

/// A clarification answer for an id that has no live pause-queue entry and no
/// caller-supplied workflow must resolve to `AlreadyResolved`, not
/// `InvalidResume`. This is the exact double-submit / already-answered shape:
/// once `resume()` clears the last queue entry, `find_by_question` returns
/// `None` and — with no `caller_workflow_id` to rehydrate from — the trigger
/// must treat the missing entry as a benign already-resolved dismissal.
#[tokio::test]
async fn double_submit_unknown_question_is_already_resolved_not_invalid() {
    let (service, _tmp) = build_service();

    let result = service
        .on_clarification_received(
            "question-that-is-not-pending",
            "the user's answer",
            None,
            None, // no caller workflow → cannot rehydrate → benign already-resolved
        )
        .await;

    match result {
        Err(TriggerError::AlreadyResolved(_)) => {
            // expected: dismissable soft-success, mapped to HTTP 409 upstream.
        },
        Err(TriggerError::InvalidResume(msg)) => panic!(
            "regression: already-answered/unknown clarification hard-errored as \
             InvalidResume ({msg}); expected AlreadyResolved (see audit #6/#20/#21)"
        ),
        Err(other) => panic!(
            "expected TriggerError::AlreadyResolved for a non-pending question, got: {other:?}"
        ),
        Ok(slots) => panic!(
            "expected an AlreadyResolved error for a non-pending question, but resume \
             succeeded with {} slot(s)",
            slots.len()
        ),
    }
}

/// Answering the same non-pending question twice must be idempotent from the
/// caller's perspective: every submission yields the same `AlreadyResolved`
/// outcome (never flipping to a hard `InvalidResume` or hanging on a pause).
#[tokio::test]
async fn repeated_answers_to_missing_question_stay_already_resolved() {
    let (service, _tmp) = build_service();

    for _ in 0..3 {
        let result = service
            .on_clarification_received("q-double-submit", "answer", None, None)
            .await;
        assert!(
            matches!(result, Err(TriggerError::AlreadyResolved(_))),
            "each repeated submit of a non-pending question must stay AlreadyResolved, got: {result:?}"
        );
    }
}

/// Supplying a `caller_workflow_id` requests durable recovery. Without a
/// session manager, the service cannot prove whether the question is absent or
/// pending, so it must fail closed and leave the answer retryable.
#[tokio::test]
async fn caller_workflow_without_session_authority_stays_retryable() {
    let (service, _tmp) = build_service();

    let result = service
        .on_clarification_received(
            "q-unknown",
            "answer",
            None,
            Some("wf-with-no-pending-session"),
        )
        .await;

    assert!(matches!(
        result,
        Err(TriggerError::RecoveryUnavailable(message))
            if message == "clarification session manager is unavailable"
    ));
}
