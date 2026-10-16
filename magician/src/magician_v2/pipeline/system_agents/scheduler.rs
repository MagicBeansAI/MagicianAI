//! system:scheduler PipelineAgent
//!
//! Receives a ScheduleContext, evaluates whether the agent should fire now
//! or sleep until next_fire, and writes a typed artifact.
//! Called by the scoped scheduler wake dispatcher via the WakeUpQueue watcher.

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::magician_v2::agents::runtime::AgentRuntime;
use crate::magician_v2::pipeline::agent::{
    ConcurrentExecutionPolicy, MissedFirePolicy, PipelineAgent, PipelineAgentError,
    PipelineAgentResult, PipelineContext, AGENT_ID_SCHEDULER,
};
use crate::magician_v2::pipeline::artifact::{
    AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION,
};
use crate::magician_v2::pipeline::schedule_utils;

// ---------------------------------------------------------------------------
// Scheduler artifact content types
// ---------------------------------------------------------------------------

/// Written when the scheduler decides the agent should fire now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShouldFireContent {
    pub agent_id: String,
    pub goal_id: String,
    pub fired_at: chrono::DateTime<Utc>,
}

/// Written when the scheduler decides the agent should sleep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepUntilContent {
    pub agent_id: String,
    pub goal_id: String,
    pub wake_at: chrono::DateTime<Utc>,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// SchedulerAgent
// ---------------------------------------------------------------------------

pub struct SchedulerAgent {
    runtime: Arc<AgentRuntime>,
}

impl SchedulerAgent {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self { runtime }
    }
}

/// Fire-window tolerance: if next_fire is within 5s of now, fire.
const FIRE_TOLERANCE_SECS: i64 = 5;

#[async_trait]
impl PipelineAgent for SchedulerAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_SCHEDULER
    }
    fn required_inputs(&self) -> Vec<ArtifactType> {
        vec![]
    }
    fn output_types(&self) -> Vec<ArtifactType> {
        vec![
            ArtifactType::Custom("scheduler:should_fire".into()),
            ArtifactType::Custom("scheduler:sleep_until".into()),
        ]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        // 1. Require schedule_context
        let sc = context
            .schedule_context
            .as_ref()
            .ok_or_else(|| PipelineAgentError::MissingInput("schedule_context".into()))?;

        // 2. Compute next_fire (pre-jitter value for the fire-window check)
        let mut next_fire_opt = schedule_utils::next_fire(&sc.schedule, sc.last_fire, sc.now);

        // 3. Apply missed fire policy
        if sc.missed_fires > 0 {
            match sc.missed_fire_policy {
                MissedFirePolicy::Skip => {
                    // Recompute next_fire from now (ignore the missed window)
                    next_fire_opt = schedule_utils::next_fire(&sc.schedule, Some(sc.now), sc.now);
                },
                MissedFirePolicy::RunOnce => {
                    // Fire immediately once, then normal schedule resumes next wake
                    next_fire_opt = Some(sc.now);
                },
                MissedFirePolicy::Queue => {
                    // Fire immediately (caller decrements missed_fires on next wake)
                    next_fire_opt = Some(sc.now);
                },
            }
        }

        let Some(pre_jitter_next_fire) = next_fire_opt else {
            // OnEvent or Once-already-fired — nothing to schedule; sleep far future
            tracing::info!(agent_id = %sc.agent_id, "system:scheduler: no next fire (OnEvent or Once exhausted)");
            let sleep_far = sc.now + chrono::Duration::days(365);
            let artifact_id = write_artifact(
                store,
                &sc.agent_id,
                ArtifactType::Custom("scheduler:sleep_until".into()),
                serde_json::to_value(SleepUntilContent {
                    agent_id: sc.agent_id.clone(),
                    goal_id: sc.goal_id.clone(),
                    wake_at: sleep_far,
                    reason: "no_next_fire".into(),
                })
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                context,
            );
            return Ok(PipelineAgentResult::Sleeping {
                wake_at: sleep_far,
                artifact_ids: vec![artifact_id],
            });
        };

        // 4. Fire-window check (use pre-jitter value)
        let should_fire =
            pre_jitter_next_fire <= sc.now + chrono::Duration::seconds(FIRE_TOLERANCE_SECS);

        if should_fire {
            // 4a. Concurrent execution check
            let is_running = self
                .runtime
                .is_goal_running_in_scope(
                    Some(&sc.principal),
                    Some(&sc.workspace),
                    &sc.agent_id,
                    &sc.goal_id,
                )
                .await;
            if is_running {
                match sc.concurrent_execution_policy {
                    ConcurrentExecutionPolicy::Skip => {
                        // Skip this fire — reschedule to next window
                        let next_window =
                            schedule_utils::next_fire(&sc.schedule, Some(sc.now), sc.now)
                                .unwrap_or(sc.now + chrono::Duration::hours(1));
                        tracing::info!(agent_id = %sc.agent_id, "system:scheduler: concurrent skip — rescheduling");
                        let artifact_id = write_artifact(
                            store,
                            &sc.agent_id,
                            ArtifactType::Custom("scheduler:sleep_until".into()),
                            serde_json::to_value(SleepUntilContent {
                                agent_id: sc.agent_id.clone(),
                                goal_id: sc.goal_id.clone(),
                                wake_at: next_window,
                                reason: "concurrent_skip".into(),
                            })
                            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                            context,
                        );
                        return Ok(PipelineAgentResult::Sleeping {
                            wake_at: next_window,
                            artifact_ids: vec![artifact_id],
                        });
                    },
                    ConcurrentExecutionPolicy::Queue => {
                        // Check back in 5s
                        let wake_at = sc.now + chrono::Duration::seconds(5);
                        let artifact_id = write_artifact(
                            store,
                            &sc.agent_id,
                            ArtifactType::Custom("scheduler:sleep_until".into()),
                            serde_json::to_value(SleepUntilContent {
                                agent_id: sc.agent_id.clone(),
                                goal_id: sc.goal_id.clone(),
                                wake_at,
                                reason: "concurrent_queue".into(),
                            })
                            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                            context,
                        );
                        return Ok(PipelineAgentResult::Sleeping {
                            wake_at,
                            artifact_ids: vec![artifact_id],
                        });
                    },
                    ConcurrentExecutionPolicy::CancelPrevious => {
                        // Cancel the currently running pipeline and fire the new scheduled run.
                        //
                        // C-09: Fix TOCTOU race in CancelPrevious.
                        //
                        // The old two-step pattern -- (1) active_cycle() read under a read-lock
                        // followed by (2) complete_active_cycle() under a separate write-lock --
                        // had a window between the two operations where another thread could
                        // complete the old cycle naturally and start a NEW cycle.  In that case
                        // complete_active_cycle(old_cycle_id) would not match the new cycle id
                        // and would return None without clearing the slot, leaving an active
                        // reservation in place.  The scheduler would then fall through to
                        // ShouldFire and admit a second concurrent run.
                        //
                        // Fix: replace the split read+clear with take_active_cycle(), which
                        // atomically acquires a write-lock, removes, and returns the active
                        // reservation in a single operation.  After take_active_cycle() returns
                        // the slot is already empty; there is no window for a concurrent actor to
                        // slip in.  The cancelled task later complete_active_cycle() call is a
                        // safe no-op: the slot is None so it returns None immediately.
                        let old_reservation = self
                            .runtime
                            .take_active_cycle_in_scope(
                                Some(&sc.principal),
                                Some(&sc.workspace),
                                &sc.agent_id,
                                &sc.goal_id,
                            )
                            .await;

                        tracing::info!(
                            agent_id = %sc.agent_id,
                            goal_id = %sc.goal_id,
                            old_cycle_id = ?old_reservation.as_ref().map(|r| &r.cycle_id),
                            "system:scheduler: CancelPrevious -- cancelling running pipeline"
                        );
                        self.runtime
                            .cancel_goal_in_scope(
                                &sc.principal,
                                &sc.workspace,
                                &sc.agent_id,
                                &sc.goal_id,
                            )
                            .await;
                        // The active slot is already cleared by take_active_cycle() above;
                        // no separate complete_active_cycle() call is needed.
                        // Fall through to ShouldFire below.
                    },
                }
            }

            // SHOULD FIRE
            tracing::info!(
                agent_id = %sc.agent_id,
                goal_id = %sc.goal_id,
                "system:scheduler: ShouldFire"
            );
            let artifact_id = write_artifact(
                store,
                &sc.agent_id,
                ArtifactType::Custom("scheduler:should_fire".into()),
                serde_json::to_value(ShouldFireContent {
                    agent_id: sc.agent_id.clone(),
                    goal_id: sc.goal_id.clone(),
                    fired_at: sc.now,
                })
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                context,
            );
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![artifact_id],
            })
        } else {
            // SLEEP UNTIL next fire (apply jitter to sleep_at only, not fire-window check)
            let sleep_at = apply_jitter(pre_jitter_next_fire, &sc.schedule);
            tracing::info!(
                agent_id = %sc.agent_id,
                "system:scheduler: SleepUntil({})",
                sleep_at
            );
            let artifact_id = write_artifact(
                store,
                &sc.agent_id,
                ArtifactType::Custom("scheduler:sleep_until".into()),
                serde_json::to_value(SleepUntilContent {
                    agent_id: sc.agent_id.clone(),
                    goal_id: sc.goal_id.clone(),
                    wake_at: sleep_at,
                    reason: "scheduled".into(),
                })
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                context,
            );
            Ok(PipelineAgentResult::Sleeping {
                wake_at: sleep_at,
                artifact_ids: vec![artifact_id],
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn write_artifact(
    store: &mut ArtifactStore,
    _agent_id: &str,
    artifact_type: ArtifactType,
    content: serde_json::Value,
    context: &PipelineContext,
) -> String {
    let artifact_id = Uuid::new_v4().to_string();
    store.put(AgentArtifact {
        artifact_id: artifact_id.clone(),
        artifact_type,
        producer_agent_id: AGENT_ID_SCHEDULER.to_string(),
        producer_cycle_id: context.cycle_id.clone(),
        content,
        schema_version: ARTIFACT_SCHEMA_VERSION,
        produced_at: Utc::now(),
        render_hints: None,
    });
    artifact_id
}

fn apply_jitter(
    base: chrono::DateTime<Utc>,
    schedule: &crate::magician_v2::pipeline::agent::AgentScheduleKind,
) -> chrono::DateTime<Utc> {
    use crate::magician_v2::pipeline::agent::AgentScheduleKind;
    let jitter_secs = match schedule {
        AgentScheduleKind::Interval { jitter_seconds, .. } => jitter_seconds.unwrap_or(0),
        _ => 0,
    };
    if jitter_secs == 0 {
        return base;
    }
    use rand::Rng;
    let offset_secs = rand::thread_rng().gen_range(0..jitter_secs) as i64;
    base + chrono::Duration::seconds(offset_secs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::pipeline::agent::{
        AgentScheduleKind, ConcurrentExecutionPolicy, MissedFirePolicy, PipelineContext,
        ScheduleContext,
    };
    use crate::magician_v2::pipeline::artifact::ArtifactStore;
    use chrono::Utc;
    use std::sync::Arc;

    fn make_ctx(
        schedule: AgentScheduleKind,
        last_fire: Option<chrono::DateTime<Utc>>,
    ) -> PipelineContext {
        PipelineContext {
            chain_id: "chain-1".into(),
            cycle_id: "cy-1".into(),
            workflow_id: "wf-1".into(),
            query: String::new(),
            iteration: 0,
            schedule_context: Some(ScheduleContext {
                principal: "anonymous".into(),
                workspace: "default".into(),
                agent_id: "test-agent".into(),
                goal_id: "test-goal".into(),
                task_id: None,
                schedule,
                last_fire,
                now: Utc::now(),
                missed_fires: 0,
                missed_fire_policy: MissedFirePolicy::Skip,
                concurrent_execution_policy: ConcurrentExecutionPolicy::Skip,
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn scheduler_fires_when_overdue() {
        let agent = SchedulerAgent::new(Arc::new(AgentRuntime::new()));
        let ctx = make_ctx(
            AgentScheduleKind::Interval {
                seconds: 3600,
                jitter_seconds: None,
            },
            Some(Utc::now() - chrono::Duration::hours(2)), // 2h ago — overdue
        );
        let mut store = ArtifactStore::new("chain-1");
        let result = agent.execute(&mut store, &ctx).await.unwrap();
        assert!(matches!(result, PipelineAgentResult::Completed { .. }));
        let artifact = store.latest_of_type(&ArtifactType::Custom("scheduler:should_fire".into()));
        assert!(artifact.is_some(), "ShouldFire artifact missing");
    }

    #[tokio::test]
    async fn scheduler_sleeps_when_not_due() {
        let agent = SchedulerAgent::new(Arc::new(AgentRuntime::new()));
        let ctx = make_ctx(
            AgentScheduleKind::Interval {
                seconds: 3600,
                jitter_seconds: None,
            },
            Some(Utc::now() - chrono::Duration::minutes(10)), // 10min ago — not due
        );
        let mut store = ArtifactStore::new("chain-1");
        let result = agent.execute(&mut store, &ctx).await.unwrap();
        assert!(matches!(result, PipelineAgentResult::Sleeping { .. }));
        if let PipelineAgentResult::Sleeping { wake_at, .. } = result {
            assert!(wake_at > Utc::now());
        }
    }

    #[tokio::test]
    async fn missing_schedule_context_returns_error() {
        let agent = SchedulerAgent::new(Arc::new(AgentRuntime::new()));
        let ctx = PipelineContext {
            chain_id: "c".into(),
            cycle_id: "cy".into(),
            workflow_id: "w".into(),
            query: String::new(),
            iteration: 0,
            ..Default::default()
        };
        let mut store = ArtifactStore::new("chain-1");
        let result = agent.execute(&mut store, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missed_fire_run_once_fires_immediately() {
        let agent = SchedulerAgent::new(Arc::new(AgentRuntime::new()));
        let mut ctx = make_ctx(
            AgentScheduleKind::Interval {
                seconds: 3600,
                jitter_seconds: None,
            },
            Some(Utc::now() - chrono::Duration::minutes(10)), // normally not due
        );
        // Override: missed fires + RunOnce policy
        if let Some(sc) = &mut ctx.schedule_context {
            sc.missed_fires = 3;
            sc.missed_fire_policy = MissedFirePolicy::RunOnce;
        }
        let mut store = ArtifactStore::new("chain-1");
        let result = agent.execute(&mut store, &ctx).await.unwrap();
        // RunOnce with missed fires → should fire now
        assert!(matches!(result, PipelineAgentResult::Completed { .. }));
    }
}
