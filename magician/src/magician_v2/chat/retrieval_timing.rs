//! Timing helpers for the memory/procedure portion of chat prompt assembly.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::context_retrieval::{
    retrieve_bound_staged_triple, retrieve_staged_pair, ContextRetrievalCheckpoint,
    ContextRetrievalObservation, ContextRetrievalOutcome, ContextRetrievalPolicy,
    ContextRetrievalRequest, ContextStageDescriptor, ContextStageKind, ContextStageState,
    IntoStagedValue,
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ChatContextRetrievalTiming {
    pub memory_ms: f64,
    pub procedures_ms: f64,
    pub concurrent_wall_ms: f64,
    pub overlap_saved_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BoundedChatContextRetrieval<M, P> {
    Completed {
        memory: M,
        procedures: P,
        timing: ChatContextRetrievalTiming,
    },
    TimedOut {
        budget_ms: u64,
        elapsed_ms: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StagedChatContextRetrieval<M, P> {
    pub memory: Option<M>,
    pub procedures: Option<P>,
    pub memory_status: ContextStageState,
    pub procedures_status: ContextStageState,
    pub timing: ChatContextRetrievalTiming,
    pub deadline_reached: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StagedChatContextRetrievalWithFast<M, P> {
    pub fast_memory: Option<M>,
    pub hybrid_memory: Option<M>,
    pub procedures: Option<P>,
    pub fast_memory_status: ContextStageState,
    pub hybrid_memory_status: ContextStageState,
    pub procedures_status: ContextStageState,
    pub fast_memory_ms: f64,
    pub timing: ChatContextRetrievalTiming,
    pub deadline_reached: bool,
    pub cancelled: bool,
    pub canonical: ContextRetrievalOutcome,
}

/// Shared partial-result path used by Chat and realtime voice. Both branches
/// begin together under one absolute deadline; whichever completes remains
/// usable even when its sibling times out.
pub async fn measure_staged_chat_context_retrieval<MF, PF, MO, PO>(
    memory: MF,
    procedures: PF,
    deadline: tokio::time::Instant,
    cancellation: CancellationToken,
) -> StagedChatContextRetrieval<MO::Value, PO::Value>
where
    MF: Future<Output = MO>,
    PF: Future<Output = PO>,
    MO: IntoStagedValue,
    PO: IntoStagedValue,
{
    let outcome = retrieve_staged_pair(memory, procedures, deadline, cancellation).await;
    let memory_duration = Duration::from_secs_f64(outcome.first_elapsed_ms / 1_000.0);
    let procedure_duration = Duration::from_secs_f64(outcome.second_elapsed_ms / 1_000.0);
    let wall = Duration::from_secs_f64(outcome.elapsed_ms / 1_000.0);
    StagedChatContextRetrieval {
        memory: outcome.first,
        procedures: outcome.second,
        memory_status: outcome.first_status,
        procedures_status: outcome.second_status,
        timing: ChatContextRetrievalTiming::from_durations(
            memory_duration,
            procedure_duration,
            wall,
        ),
        deadline_reached: outcome.deadline_reached,
        cancelled: outcome.cancelled,
    }
}

/// Three-stage production path. Fast immutable memory is independent of the
/// hybrid index, so an embedding/index delay cannot erase an exact lexical or
/// hot-snapshot match that is already ready for the same turn.
pub async fn measure_staged_chat_context_retrieval_with_fast<FF, HF, PF, FO, HO, PO>(
    fast_memory: FF,
    hybrid_memory: HF,
    procedures: PF,
    checkpoint: ContextRetrievalCheckpoint,
    configured_budget_ms: u64,
    request: ContextRetrievalRequest,
    policy: ContextRetrievalPolicy,
    cancellation: CancellationToken,
) -> Result<
    StagedChatContextRetrievalWithFast<FO::Value, PO::Value>,
    crate::magician_v2::context_retrieval::ContextCoordinatorError,
>
where
    FF: Future<Output = FO>,
    HF: Future<Output = HO>,
    PF: Future<Output = PO>,
    FO: IntoStagedValue,
    HO: IntoStagedValue<Value = FO::Value>,
    PO: IntoStagedValue,
    FO::Value: crate::magician_v2::context_retrieval::ContextContributionSource,
    PO::Value: crate::magician_v2::context_retrieval::ContextContributionSource,
{
    let outcome = retrieve_bound_staged_triple(
        request,
        policy,
        cancellation,
        ContextStageDescriptor::new(ContextStageKind::FastMemory, 0, "fast_memory"),
        fast_memory,
        ContextStageDescriptor::new(ContextStageKind::HybridMemory, 0, "hybrid_memory"),
        hybrid_memory,
        ContextStageDescriptor::new(
            ContextStageKind::ReusableProcedures,
            0,
            "reusable_procedures",
        ),
        procedures,
    )
    .await?;
    ContextRetrievalObservation::from_outcome(checkpoint, configured_budget_ms, &outcome.canonical)
        .emit();
    let hybrid_duration = Duration::from_secs_f64(outcome.second_elapsed_ms / 1_000.0);
    let procedure_duration = Duration::from_secs_f64(outcome.third_elapsed_ms / 1_000.0);
    let wall = Duration::from_secs_f64(outcome.elapsed_ms / 1_000.0);
    Ok(StagedChatContextRetrievalWithFast {
        fast_memory: outcome.first,
        hybrid_memory: outcome.second,
        procedures: outcome.third,
        fast_memory_status: outcome.first_status,
        hybrid_memory_status: outcome.second_status,
        procedures_status: outcome.third_status,
        fast_memory_ms: outcome.first_elapsed_ms,
        timing: ChatContextRetrievalTiming::from_durations(
            hybrid_duration,
            procedure_duration,
            wall,
        ),
        deadline_reached: outcome.deadline_reached,
        cancelled: outcome.cancelled,
        canonical: outcome.canonical,
    })
}

impl ChatContextRetrievalTiming {
    fn from_durations(memory: Duration, procedures: Duration, wall: Duration) -> Self {
        let memory_ms = duration_ms(memory);
        let procedures_ms = duration_ms(procedures);
        let concurrent_wall_ms = duration_ms(wall);
        Self {
            memory_ms,
            procedures_ms,
            concurrent_wall_ms,
            overlap_saved_ms: (memory_ms + procedures_ms - concurrent_wall_ms).max(0.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatencySummary {
    pub count: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

pub fn summarize_latencies(values_ms: &[f64]) -> Option<LatencySummary> {
    if values_ms.is_empty() {
        return None;
    }
    let mut sorted = values_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mean_ms = sorted.iter().sum::<f64>() / sorted.len() as f64;
    Some(LatencySummary {
        count: sorted.len(),
        min_ms: sorted[0],
        mean_ms,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        max_ms: *sorted.last().expect("non-empty latency sample"),
    })
}

/// Run memory and procedure retrieval exactly as chat does: concurrently,
/// while retaining each branch's elapsed time and the user-visible wall time.
pub async fn measure_chat_context_retrieval<MF, PF, M, P>(
    memory: MF,
    procedures: PF,
) -> ((M, P), ChatContextRetrievalTiming)
where
    MF: Future<Output = M>,
    PF: Future<Output = P>,
{
    let wall_started = Instant::now();
    let (memory, procedures) = tokio::join!(measure_future(memory), measure_future(procedures));
    let timing =
        ChatContextRetrievalTiming::from_durations(memory.1, procedures.1, wall_started.elapsed());
    ((memory.0, procedures.0), timing)
}

/// Run the same concurrent memory/procedure retrieval as Chat, but enforce a
/// hard response-start budget for realtime voice. Dropping the timed-out join
/// future cancels both branches before the caller creates the provider
/// response, so late context can never leak into a subsequent utterance.
pub async fn measure_bounded_chat_context_retrieval<MF, PF, M, P>(
    memory: MF,
    procedures: PF,
    budget: Duration,
) -> BoundedChatContextRetrieval<M, P>
where
    MF: Future<Output = M>,
    PF: Future<Output = P>,
{
    let started = Instant::now();
    match tokio::time::timeout(budget, measure_chat_context_retrieval(memory, procedures)).await {
        Ok(((memory, procedures), timing)) => BoundedChatContextRetrieval::Completed {
            memory,
            procedures,
            timing,
        },
        Err(_) => BoundedChatContextRetrieval::TimedOut {
            budget_ms: budget.as_millis().min(u64::MAX as u128) as u64,
            elapsed_ms: duration_ms(started.elapsed()),
        },
    }
}

async fn measure_future<F, T>(future: F) -> (T, Duration)
where
    F: Future<Output = T>,
{
    let started = Instant::now();
    let output = future.await;
    (output, started.elapsed())
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use tokio::sync::Barrier;

    use super::*;

    fn staged_request(query: &str) -> ContextRetrievalRequest {
        ContextRetrievalRequest::new(
            "owner",
            "default",
            "personal-assistant",
            crate::magician_v2::agents::InvocationSurface::Chat,
            crate::magician_v2::agents::FeatureMode::None,
            "session-1",
            "turn-1",
            1,
            query,
            "authority-r1",
            BTreeMap::from([("live_read".to_string(), "turn-1".to_string())]),
        )
    }

    #[test]
    fn chat_context_retrieval_latency_summary_is_stable() {
        let summary = summarize_latencies(&[50.0, 10.0, 40.0, 20.0, 30.0]).unwrap();
        assert_eq!(summary.count, 5);
        assert_eq!(summary.min_ms, 10.0);
        assert_eq!(summary.mean_ms, 30.0);
        assert_eq!(summary.p50_ms, 30.0);
        assert_eq!(summary.p95_ms, 50.0);
        assert_eq!(summary.max_ms, 50.0);
        assert!(summarize_latencies(&[]).is_none());
    }

    #[tokio::test]
    async fn chat_context_retrieval_starts_memory_and_procedures_concurrently() {
        let barrier = Arc::new(Barrier::new(3));
        let memory_barrier = barrier.clone();
        let procedure_barrier = barrier.clone();
        let measurement = tokio::spawn(async move {
            measure_chat_context_retrieval(
                async move {
                    memory_barrier.wait().await;
                    "memory"
                },
                async move {
                    procedure_barrier.wait().await;
                    "procedures"
                },
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), barrier.wait())
            .await
            .expect("both retrieval branches must reach the barrier");
        let ((memory, procedures), timing) = measurement.await.unwrap();

        assert_eq!(memory, "memory");
        assert_eq!(procedures, "procedures");
        assert!(timing.memory_ms >= 0.0);
        assert!(timing.procedures_ms >= 0.0);
        assert!(timing.concurrent_wall_ms >= 0.0);
        assert!(timing.overlap_saved_ms >= 0.0);
    }

    #[tokio::test]
    async fn agent_surface_runtime_realtime_context_budget_cancels_late_branches() {
        let outcome = measure_bounded_chat_context_retrieval(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "memory"
            },
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "procedures"
            },
            Duration::from_millis(5),
        )
        .await;

        assert!(matches!(
            outcome,
            BoundedChatContextRetrieval::TimedOut { budget_ms: 5, .. }
        ));
    }

    #[tokio::test]
    async fn agent_surface_runtime_realtime_context_budget_preserves_concurrent_results() {
        let outcome = measure_bounded_chat_context_retrieval(
            async { "memory" },
            async { "procedures" },
            Duration::from_millis(50),
        )
        .await;

        match outcome {
            BoundedChatContextRetrieval::Completed {
                memory,
                procedures,
                timing,
            } => {
                assert_eq!(memory, "memory");
                assert_eq!(procedures, "procedures");
                assert!(timing.concurrent_wall_ms < 50.0);
            },
            BoundedChatContextRetrieval::TimedOut { .. } => panic!("unexpected timeout"),
        }
    }

    #[tokio::test]
    async fn staged_fast_helper_reports_empty_independently_without_nested_options() {
        let outcome = measure_staged_chat_context_retrieval_with_fast(
            async { Some("fast memory") },
            async { None::<&'static str> },
            async { Some("procedure") },
            ContextRetrievalCheckpoint::ChatTurn,
            1_000,
            staged_request("empty sibling"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.fast_memory, Some("fast memory"));
        assert!(outcome.hybrid_memory.is_none());
        assert_eq!(outcome.procedures, Some("procedure"));
        assert_eq!(outcome.fast_memory_status, ContextStageState::Completed);
        assert_eq!(outcome.hybrid_memory_status, ContextStageState::Empty);
        assert_eq!(outcome.procedures_status, ContextStageState::Completed);
        assert!(!outcome.deadline_reached);
    }

    #[tokio::test]
    async fn staged_fast_helper_accepts_production_stage_values_without_losing_errors() {
        use crate::magician_v2::context_retrieval::StagedValue;

        let outcome = measure_staged_chat_context_retrieval_with_fast(
            async { StagedValue::Completed("fast memory") },
            async { StagedValue::<&'static str>::error("hybrid_index_unavailable", true) },
            async { StagedValue::<&'static str>::Empty },
            ContextRetrievalCheckpoint::ChatTurn,
            1_000,
            staged_request("stage error"),
            ContextRetrievalPolicy::with_budget(Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.fast_memory, Some("fast memory"));
        assert!(outcome.hybrid_memory.is_none());
        assert!(outcome.procedures.is_none());
        assert_eq!(outcome.fast_memory_status, ContextStageState::Completed);
        assert_eq!(outcome.hybrid_memory_status, ContextStageState::Error);
        assert_eq!(outcome.procedures_status, ContextStageState::Empty);
        assert!(!outcome.deadline_reached);
    }

    #[tokio::test]
    async fn staged_fast_helper_retains_fast_result_when_other_context_is_late() {
        let outcome = measure_staged_chat_context_retrieval_with_fast(
            async { Some("fast memory") },
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Some("late hybrid")
            },
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Some("late procedure")
            },
            ContextRetrievalCheckpoint::RealtimeVoiceTurn,
            250,
            staged_request("partial deadline"),
            ContextRetrievalPolicy::with_budget(Duration::from_millis(250)),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(outcome.fast_memory, Some("fast memory"));
        assert!(outcome.hybrid_memory.is_none());
        assert!(outcome.procedures.is_none());
        assert_eq!(outcome.fast_memory_status, ContextStageState::Completed);
        assert_eq!(outcome.hybrid_memory_status, ContextStageState::TimedOut);
        assert_eq!(outcome.procedures_status, ContextStageState::TimedOut);
        assert!(outcome.deadline_reached);
    }
}
