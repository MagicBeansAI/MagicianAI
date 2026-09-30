//! Agent lifecycle event emission helpers.
//!
//! These functions emit `RuntimeTransportEvent` variants for agent lifecycle transitions.
//!
//! ## Production wiring (Phase 0)
//!
//! **`emit_agent_cycle_started` / `emit_agent_cycle_completed`** are called from
//! production code via [`execute_agent_cycle`](crate::magician_v2::execution::agentic::execute_agent_cycle),
//! which wraps `execute_agentically()` and emits start/complete events when the
//! `AgenticContext` has agent routing. The orchestrator calls `execute_agent_cycle`
//! at its main execution site — any context with agent routing will emit events.
//!
//! **`emit_agent_triggered`** is NOT yet called from production code. It requires
//! the trigger dispatch layer (Phase 3's `AgentRuntime::dispatch_trigger()`), which
//! doesn't exist yet. Phase 0 provides the helper and tests so the full pipeline is
//! ready when Phase 3 adds the trigger system.
//!
//! The receiving side is fully wired: `v2-websocket.ts` dispatches these to
//! `agentStore.handleAgentEvent()` which updates the Svelte store.

use std::sync::Arc;

use super::agent_update::{AgentUpdate, AgentUpdateKind, CycleOutcome};
use super::agent_update_emitter::publish_agent_update;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

/// Map the stringly-typed cycle outcome (from `execute_agent_cycle`) to the
/// typed [`CycleOutcome`] used in the operator-facing feed.
fn map_cycle_outcome(outcome: &str) -> CycleOutcome {
    match outcome {
        "success" | "goal_achieved" => CycleOutcome::Succeeded,
        "failure" | "failed" => CycleOutcome::Failed,
        "paused" | "waiting_for_user" | "waiting_for_confirmation" | "max_iterations_reached" => {
            CycleOutcome::Paused
        },
        _ => CycleOutcome::PartiallySucceeded,
    }
}

/// Emit an `AgentTriggered` event when an agent is about to start working on a goal.
///
/// **Phase 3 call site**: `AgentRuntime::dispatch_trigger()` after matching a trigger
/// to an agent definition and before entering the execution cycle.
pub fn emit_agent_triggered(
    broadcaster: &Arc<RuntimeTransportBroadcaster>,
    principal: Option<&str>,
    workspace: Option<&str>,
    agent_id: &str,
    goal_id: &str,
    trigger: &str,
) {
    broadcaster.emit(RuntimeTransportEvent::AgentTriggered {
        principal: principal.map(str::to_string),
        workspace: workspace.map(str::to_string),
        agent_id: agent_id.to_string(),
        goal_id: goal_id.to_string(),
        trigger: trigger.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    });
}

/// Emit an `AgentCycleStarted` event when an agent begins an observe-decide-execute cycle.
///
/// **Phase 0 call site**: [`execute_agent_cycle`](crate::magician_v2::execution::agentic::execute_agent_cycle)
/// emits this before calling `execute_agentically()` when the context has agent routing.
/// Phase 3 will also call this from `AgentRuntime::run_cycle()`.
pub fn emit_agent_cycle_started(
    broadcaster: &Arc<RuntimeTransportBroadcaster>,
    principal: Option<&str>,
    workspace: Option<&str>,
    agent_id: &str,
    goal_id: &str,
    cycle_id: &str,
    execution_id: Option<&str>,
    goal: &str,
) {
    broadcaster.emit(RuntimeTransportEvent::AgentCycleStarted {
        principal: principal.map(str::to_string),
        workspace: workspace.map(str::to_string),
        agent_id: agent_id.to_string(),
        goal_id: goal_id.to_string(),
        cycle_id: cycle_id.to_string(),
        execution_id: execution_id.map(str::to_string),
        goal: goal.to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
    });

    // Parallel emission for the operator-facing feed. Additive alongside the
    // existing typed event — does not alter any consumer of the original.
    let update = AgentUpdate::new(
        workspace.unwrap_or_default().to_string(),
        agent_id.to_string(),
        AgentUpdateKind::CycleStarted {
            focus_area: None,
            trigger: "agent_execution".to_string(),
        },
    )
    .with_cycle(cycle_id.to_string());
    publish_agent_update(broadcaster, principal, workspace, update);
}

/// Emit an `AgentCycleCompleted` event when an agent finishes a cycle.
///
/// **Phase 0 call site**: [`execute_agent_cycle`](crate::magician_v2::execution::agentic::execute_agent_cycle)
/// emits this after `execute_agentically()` returns. Outcome mapping: `Success` → "success",
/// `Failed`/`CannotProceed`/`LoopDetected`/`BudgetExhausted` → "failure",
/// `WaitingForUser`/`WaitingForConfirmation`/`MaxIterationsReached` → "paused".
pub fn emit_agent_cycle_completed(
    broadcaster: &Arc<RuntimeTransportBroadcaster>,
    principal: Option<&str>,
    workspace: Option<&str>,
    agent_id: &str,
    goal_id: &str,
    cycle_id: &str,
    execution_id: Option<&str>,
    outcome: &str,
    iterations_used: usize,
    duration_ms: u64,
) {
    broadcaster.emit(RuntimeTransportEvent::AgentCycleCompleted {
        principal: principal.map(str::to_string),
        workspace: workspace.map(str::to_string),
        agent_id: agent_id.to_string(),
        goal_id: goal_id.to_string(),
        cycle_id: cycle_id.to_string(),
        execution_id: execution_id.map(str::to_string),
        outcome: outcome.to_string(),
        iterations_used,
        timestamp: chrono::Utc::now().timestamp_millis(),
    });

    // Parallel emission for the operator-facing feed.
    let update = AgentUpdate::new(
        workspace.unwrap_or_default().to_string(),
        agent_id.to_string(),
        AgentUpdateKind::CycleCompleted {
            outcome: map_cycle_outcome(outcome),
            duration_ms,
        },
    )
    .with_cycle(cycle_id.to_string());
    publish_agent_update(broadcaster, principal, workspace, update);
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn emit_agent_triggered_sends_event() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        emit_agent_triggered(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "agent-1",
            "goal-1",
            "schedule",
        );

        let event = rx.try_recv().unwrap();
        match event {
            RuntimeTransportEvent::AgentTriggered {
                principal,
                workspace,
                agent_id,
                goal_id,
                trigger,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
                assert_eq!(agent_id, "agent-1");
                assert_eq!(goal_id, "goal-1");
                assert_eq!(trigger, "schedule");
            },
            other => panic!("Expected AgentTriggered, got {:?}", other),
        }
    }

    #[test]
    fn emit_agent_cycle_started_sends_event() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        emit_agent_cycle_started(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "a1",
            "g1",
            "c1",
            Some("exec-1"),
            "Do the thing",
        );

        let event = rx.try_recv().unwrap();
        match event {
            RuntimeTransportEvent::AgentCycleStarted {
                principal,
                workspace,
                agent_id,
                goal_id,
                cycle_id,
                execution_id,
                goal,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
                assert_eq!(agent_id, "a1");
                assert_eq!(goal_id, "g1");
                assert_eq!(cycle_id, "c1");
                assert_eq!(execution_id.as_deref(), Some("exec-1"));
                assert_eq!(goal, "Do the thing");
            },
            other => panic!("Expected AgentCycleStarted, got {:?}", other),
        }
    }

    #[test]
    fn emit_agent_cycle_completed_sends_event() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        emit_agent_cycle_completed(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "a1",
            "g1",
            "c1",
            Some("exec-1"),
            "goal_achieved",
            5,
            1234,
        );

        let event = rx.try_recv().unwrap();
        match event {
            RuntimeTransportEvent::AgentCycleCompleted {
                principal,
                workspace,
                agent_id,
                execution_id,
                outcome,
                iterations_used,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
                assert_eq!(agent_id, "a1");
                assert_eq!(execution_id.as_deref(), Some("exec-1"));
                assert_eq!(outcome, "goal_achieved");
                assert_eq!(iterations_used, 5);
            },
            other => panic!("Expected AgentCycleCompleted, got {:?}", other),
        }
    }

    /// Integration test: simulates a full agent lifecycle (triggered → cycle started →
    /// cycle completed) on a single broadcast channel. Proves the pipeline delivers
    /// all three event types in order — the same pipeline Phase 3's AgentRuntime will use.
    #[test]
    fn full_agent_lifecycle_pipeline() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        // 1. Agent triggered
        emit_agent_triggered(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "agent-x",
            "goal-1",
            "manual",
        );
        // 2. Cycle started
        emit_agent_cycle_started(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "agent-x",
            "goal-1",
            "cycle-1",
            Some("thread-x"),
            "Navigate to page",
        );
        // 3. Cycle completed
        emit_agent_cycle_completed(
            &broadcaster,
            Some("alpha"),
            Some("prod"),
            "agent-x",
            "goal-1",
            "cycle-1",
            Some("thread-x"),
            "success",
            3,
            500,
        );

        // Expected event order on the shared channel:
        //   1. AgentTriggered                        (from emit_agent_triggered)
        //   2. AgentCycleStarted                     (typed lifecycle event)
        //   3. AgentEvent{event_type=agent.update}   (parallel operator-feed emission)
        //   4. AgentCycleCompleted                   (typed lifecycle event)
        //   5. AgentEvent{event_type=agent.update}   (parallel operator-feed emission)
        let e1 = rx.try_recv().unwrap();
        assert!(matches!(e1, RuntimeTransportEvent::AgentTriggered { .. }));

        let e2 = rx.try_recv().unwrap();
        assert!(matches!(
            e2,
            RuntimeTransportEvent::AgentCycleStarted { .. }
        ));

        let e3 = rx.try_recv().unwrap();
        match e3 {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, "agent.update");
                assert_eq!(
                    event.payload.get("kind").and_then(|v| v.as_str()),
                    Some("cycle_started")
                );
            },
            other => panic!("expected AgentEvent for cycle_started, got {:?}", other),
        }

        let e4 = rx.try_recv().unwrap();
        match e4 {
            RuntimeTransportEvent::AgentCycleCompleted {
                outcome,
                iterations_used,
                ..
            } => {
                assert_eq!(outcome, "success");
                assert_eq!(iterations_used, 3);
            },
            other => panic!("Expected AgentCycleCompleted, got {:?}", other),
        }

        let e5 = rx.try_recv().unwrap();
        match e5 {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, "agent.update");
                assert_eq!(
                    event.payload.get("kind").and_then(|v| v.as_str()),
                    Some("cycle_completed")
                );
                assert_eq!(
                    event.payload.get("outcome").and_then(|v| v.as_str()),
                    Some("succeeded"),
                );
            },
            other => panic!("expected AgentEvent for cycle_completed, got {:?}", other),
        }

        assert!(rx.try_recv().is_err(), "no more events expected");
    }

    #[test]
    fn cycle_started_also_emits_agent_update_envelope() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        emit_agent_cycle_started(
            &broadcaster,
            Some("alpha"),
            Some("ws_a"),
            "agent-1",
            "goal-1",
            "cycle-1",
            None,
            "do it",
        );

        // First event: typed RuntimeTransportEvent::AgentCycleStarted (existing).
        let first = rx.try_recv().unwrap();
        assert!(matches!(
            first,
            RuntimeTransportEvent::AgentCycleStarted { .. }
        ));

        // Second event: AgentEventEnvelope with event_type = "agent.update".
        let second = rx.try_recv().unwrap();
        match second {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, "agent.update");
                assert_eq!(event.agent_id, "agent-1");
                assert_eq!(event.principal.as_deref(), Some("alpha"));
                assert_eq!(event.workspace.as_deref(), Some("ws_a"));
                let kind = event
                    .payload
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .expect("payload has kind");
                assert_eq!(kind, "cycle_started");
                let cycle_id = event
                    .payload
                    .get("cycle_id")
                    .and_then(|v| v.as_str())
                    .expect("payload has cycle_id");
                assert_eq!(cycle_id, "cycle-1");
            },
            other => panic!("expected AgentEvent, got {:?}", other),
        }
    }

    #[test]
    fn cycle_completed_also_emits_agent_update_envelope_with_mapped_outcome() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        emit_agent_cycle_completed(
            &broadcaster,
            Some("alpha"),
            Some("ws_a"),
            "agent-1",
            "goal-1",
            "cycle-1",
            None,
            "failure",
            3,
            250,
        );

        // Drain the typed event first.
        let _ = rx.try_recv().unwrap();
        // Then the envelope.
        let envelope_event = rx.try_recv().unwrap();
        match envelope_event {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, "agent.update");
                let kind = event.payload.get("kind").and_then(|v| v.as_str()).unwrap();
                assert_eq!(kind, "cycle_completed");
                let outcome = event
                    .payload
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .unwrap();
                assert_eq!(outcome, "failed");
            },
            other => panic!("expected AgentEvent, got {:?}", other),
        }
    }

    #[test]
    fn map_cycle_outcome_handles_known_variants() {
        assert_eq!(map_cycle_outcome("success"), CycleOutcome::Succeeded);
        assert_eq!(map_cycle_outcome("goal_achieved"), CycleOutcome::Succeeded);
        assert_eq!(map_cycle_outcome("failure"), CycleOutcome::Failed);
        assert_eq!(map_cycle_outcome("failed"), CycleOutcome::Failed);
        assert_eq!(map_cycle_outcome("paused"), CycleOutcome::Paused);
        assert_eq!(map_cycle_outcome("waiting_for_user"), CycleOutcome::Paused);
        assert_eq!(
            map_cycle_outcome("max_iterations_reached"),
            CycleOutcome::Paused
        );
        assert_eq!(
            map_cycle_outcome("mystery"),
            CycleOutcome::PartiallySucceeded
        );
    }
}
