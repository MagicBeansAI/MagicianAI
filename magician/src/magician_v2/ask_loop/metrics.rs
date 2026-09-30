use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;

use super::session::ClarificationSessionState;

/// Snapshot exposed to dashboards/telemetry consumers.
#[derive(Debug, Clone, Serialize)]
pub struct ClarificationMetricsSnapshot {
    pub total_sessions_started: u64,
    pub total_sessions_completed: u64,
    pub active_sessions: u64,
    pub avg_session_duration_ms: Option<f64>,
    pub avg_questions_per_session: Option<f64>,
    pub guardrail_timeouts: u64,
    pub guardrail_question_caps: u64,
    pub guardrail_round_caps: u64,
    pub timestamp_ms: i64,
}

#[derive(Debug)]
struct SessionHeartbeat {
    started_at: DateTime<Utc>,
    last_snapshot: DateTime<Utc>,
    total_questions: u64,
}

/// Input data derived from the running session.
#[derive(Debug)]
pub struct SessionSnapshotStats {
    pub state: ClarificationSessionState,
    pub total_questions: u64,
    pub waiting_on_user: usize,
    pub queued: usize,
}

/// Thread-safe accumulator for clarification metrics.
pub struct ClarificationMetrics {
    sessions_started: AtomicU64,
    sessions_completed: AtomicU64,
    guardrail_timeouts: AtomicU64,
    guardrail_question_caps: AtomicU64,
    guardrail_round_caps: AtomicU64,
    total_duration_ms: AtomicU64,
    total_questions_completed: AtomicU64,
    active_sessions: DashMap<String, SessionHeartbeat>,
}

impl Default for ClarificationMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ClarificationMetrics {
    pub fn new() -> Self {
        Self {
            sessions_started: AtomicU64::new(0),
            sessions_completed: AtomicU64::new(0),
            guardrail_timeouts: AtomicU64::new(0),
            guardrail_question_caps: AtomicU64::new(0),
            guardrail_round_caps: AtomicU64::new(0),
            total_duration_ms: AtomicU64::new(0),
            total_questions_completed: AtomicU64::new(0),
            active_sessions: DashMap::new(),
        }
    }

    pub fn record_guardrail_timeout(&self) {
        self.guardrail_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_guardrail_question_cap(&self) {
        self.guardrail_question_caps.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_guardrail_round_cap(&self) {
        self.guardrail_round_caps.fetch_add(1, Ordering::Relaxed);
    }

    /// Update metrics using the latest session snapshot and return the aggregate snapshot.
    pub fn record_session_snapshot(
        &self,
        workflow_id: &str,
        stats: &SessionSnapshotStats,
    ) -> ClarificationMetricsSnapshot {
        let now = Utc::now();
        let mut should_complete = false;
        {
            let mut heartbeat = self
                .active_sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| {
                    self.sessions_started.fetch_add(1, Ordering::Relaxed);
                    SessionHeartbeat {
                        started_at: now,
                        last_snapshot: now,
                        total_questions: stats.total_questions,
                    }
                });

            heartbeat.last_snapshot = now;
            heartbeat.total_questions = stats.total_questions;

            let all_questions_answered = stats.waiting_on_user == 0 && stats.queued == 0;
            if stats.state == ClarificationSessionState::ReadyToPlan && all_questions_answered {
                should_complete = true;
            }
        }

        if should_complete {
            if let Some((_, hb)) = self.active_sessions.remove(workflow_id) {
                self.sessions_completed.fetch_add(1, Ordering::Relaxed);
                let duration_ms = (now - hb.started_at).num_milliseconds().max(0) as u64;
                self.total_duration_ms
                    .fetch_add(duration_ms, Ordering::Relaxed);
                self.total_questions_completed
                    .fetch_add(hb.total_questions, Ordering::Relaxed);
            }
        }

        self.snapshot()
    }

    pub fn snapshot(&self) -> ClarificationMetricsSnapshot {
        let completed = self.sessions_completed.load(Ordering::Relaxed);
        let avg_duration = if completed > 0 {
            Some(self.total_duration_ms.load(Ordering::Relaxed) as f64 / completed as f64)
        } else {
            None
        };

        let avg_questions = if completed > 0 {
            Some(self.total_questions_completed.load(Ordering::Relaxed) as f64 / completed as f64)
        } else {
            None
        };

        ClarificationMetricsSnapshot {
            total_sessions_started: self.sessions_started.load(Ordering::Relaxed),
            total_sessions_completed: completed,
            active_sessions: self.active_sessions.len() as u64,
            avg_session_duration_ms: avg_duration,
            avg_questions_per_session: avg_questions,
            guardrail_timeouts: self.guardrail_timeouts.load(Ordering::Relaxed),
            guardrail_question_caps: self.guardrail_question_caps.load(Ordering::Relaxed),
            guardrail_round_caps: self.guardrail_round_caps.load(Ordering::Relaxed),
            timestamp_ms: Utc::now().timestamp_millis(),
        }
    }
}
