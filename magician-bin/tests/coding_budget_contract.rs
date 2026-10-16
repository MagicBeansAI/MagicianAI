//! Production watchdog tests with injected monotonic instants: days of work
//! are verified in milliseconds, without the monolith's full fixture graph.
pub use magician::magician_v2;
pub use magician_v2::execution::coding_engine::{
    coding_event_from_raw, CodingEngineEvent, CodingEngineEventKind,
};

mod config {
    pub use magician::config::*;
}

#[path = "../../magician/src/magician_v2/execution/coding_engine/budgets.rs"]
mod budgets;

#[path = "../../magician/src/magician_v2/execution/coding_engine/cost_budget.rs"]
mod cost_budget;
#[path = "../../magician/src/magician_v2/execution/coding_engine/task_budget.rs"]
mod task_budget;

#[path = "../../magician/src/magician_v2/execution/agentic/run_budget.rs"]
mod run_budget;

// The budget source's `super::types` is the canonical runtime type.
pub use magician_v2::execution::agentic::types;

#[test]
fn long_run_codex_protocol_tools_own_the_clock_until_completion() {
    use magician_v2::execution::coding_engine::codex::project_notification;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    for kind in [
        "mcpToolCall",
        "dynamicToolCall",
        "webSearch",
        "commandExecution",
        "fileChange",
    ] {
        let params =
            serde_json::json!({"item":{"id":"operation", "type":kind, "tool":"arbitrary_tool"}});
        let mut watchdog =
            budgets::ProgressWatchdog::new(budgets::ResolvedCodingBudgets::default(), start);
        let event = project_notification(1, "item/started", &params);
        assert_eq!(
            event.kind,
            CodingEngineEventKind::ToolExecutionStart,
            "{kind}"
        );
        watchdog.observe(&event, start);
        assert!(
            watchdog
                .expired(start + Duration::from_secs(86401))
                .is_none(),
            "{kind}"
        );
        let event = project_notification(2, "item/completed", &params);
        assert_eq!(
            event.kind,
            CodingEngineEventKind::ToolExecutionEnd,
            "{kind}"
        );
        watchdog.observe(&event, start + Duration::from_secs(86401));
        assert!(
            watchdog
                .expired(start + Duration::from_secs(86401 + 900))
                .is_some(),
            "model watchdog returns after {kind}"
        );
    }
}
