use std::sync::Arc;
use std::thread;

use magician::magician_v2::ask_loop::{
    ClarificationMetrics, ClarificationSessionState, SessionSnapshotStats,
};

#[test]
fn clarification_metrics_handles_concurrent_sessions() {
    let metrics = Arc::new(ClarificationMetrics::new());
    let workflow_ids: Vec<String> = (0..32).map(|i| format!("workflow-{}", i)).collect();

    thread::scope(|scope| {
        for workflow_id in workflow_ids.iter() {
            let id = workflow_id.clone();
            let metrics = metrics.clone();
            scope.spawn(move || {
                for round in 0..5 {
                    let stats = SessionSnapshotStats {
                        state: if round < 4 {
                            ClarificationSessionState::CollectingAnswers
                        } else {
                            ClarificationSessionState::ReadyToPlan
                        },
                        total_questions: (round + 1) as u64,
                        waiting_on_user: if round < 4 { 1 } else { 0 },
                        queued: if round < 2 { 1 } else { 0 },
                    };
                    metrics.record_session_snapshot(&id, &stats);
                }
            });
        }
    });

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.total_sessions_started, workflow_ids.len() as u64);
    assert_eq!(snapshot.total_sessions_completed, workflow_ids.len() as u64);
    assert_eq!(snapshot.active_sessions, 0);
    assert!(snapshot.avg_session_duration_ms.is_some());
    assert!(snapshot.avg_questions_per_session.is_some());
}
