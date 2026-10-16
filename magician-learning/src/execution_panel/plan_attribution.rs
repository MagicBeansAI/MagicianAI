//! Off-plan attribution: which approved-plan step each run activity row
//! belongs to, or that it was outside the plan.
//!
//! The run's own durable event log carries its step boundaries:
//! `step.started` opens a step's window (the agent's `step_started`, or the
//! runtime opening the step an action completes), and that step's
//! `step.completed` / `step.failed` / `step.skipped` closes it. An activity row
//! recorded inside a window is that step's work; one recorded outside every
//! window (before the first step, between steps, after the last, after a skip)
//! is outside the plan. See `docs/archive/plans/2026-10-01-plan-step-progress.md`.
//!
//! Read back here rather than stamped on each event at emit time: activity is
//! emitted from several layers (the loop, the LLM dispatch seam, operation
//! telemetry) that do not hold the loop's step state, and canonical events are
//! persisted by an outbox projector that may run in another process or later.
//! The boundaries are in the same seq-ordered log as the activity, so the
//! answer is the same, is deterministic, and holds for every recorded run.

use std::collections::{HashMap, HashSet};

use magician::magician_v2::{
    artifact_v2::models::CanonicalEvent,
    progress_channel_seam::{ProgressMessage, ProgressMessageKind},
};

/// Where one activity row belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attribution {
    /// Inside this plan step's window.
    Step(String),
    /// Outside every plan step's window.
    OffPlan,
}

/// One plan step's window on the run's clock, `[start_ms, end_ms)`; an open
/// window (`end_ms: None`) is the step the run is on now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepWindow {
    pub step_id: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// The selected run's attribution: one answer per event of the merged log
/// (`None` for a row that is not activity, such as a step boundary or a
/// lifecycle row), plus the windows for rows keyed by time alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunAttribution {
    pub per_event: Vec<Option<Attribution>>,
    pub windows: Vec<StepWindow>,
}

impl RunAttribution {
    /// Where a row recorded at `timestamp_ms` belongs, by the windows.
    pub fn at(&self, timestamp_ms: i64) -> Attribution {
        self.windows
            .iter()
            .rev()
            .find(|window| {
                window.start_ms <= timestamp_ms
                    && window.end_ms.map_or(true, |end_ms| timestamp_ms < end_ms)
            })
            .map(|window| Attribution::Step(window.step_id.clone()))
            .unwrap_or(Attribution::OffPlan)
    }
}

/// Count each step's rows and the rows outside the plan.
pub(crate) fn attribution_counts<'a>(
    attributions: impl IntoIterator<Item = &'a Attribution>,
) -> (HashMap<String, u32>, u32) {
    let mut per_step: HashMap<String, u32> = HashMap::new();
    let mut off_plan = 0u32;
    for attribution in attributions {
        match attribution {
            Attribution::Step(step_id) => *per_step.entry(step_id.clone()).or_insert(0) += 1,
            Attribution::OffPlan => off_plan += 1,
        }
    }
    (per_step, off_plan)
}

/// A row of the run's own that frames the work rather than being part of it:
/// step boundaries, run lifecycle, planning. Everything else — model calls,
/// tool calls, decisions, delegations, questions, recipe replays — is activity.
fn is_lifecycle_event(event_type: &str) -> bool {
    // The agent's own progress narration is part of the work.
    if event_type == "execution.progress" {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "step.",
        "plan.",
        "execution.",
        "execution_finalizer.",
        "task_agent_finalizer.",
        "agentic.execution_",
        "agentic.iteration_",
        "agentic.resumed",
        "message.",
    ];
    PREFIXES.iter().any(|prefix| event_type.starts_with(prefix))
}

fn step_id_of(event: &CanonicalEvent) -> Option<&str> {
    event
        .payload
        .get("step_id")
        .and_then(|value| value.as_str())
        .or(event.step_id.as_deref())
        .map(str::trim)
        .filter(|step_id| !step_id.is_empty())
}

fn timestamp_ms_of(event: &CanonicalEvent) -> i64 {
    event
        .payload
        .get("timestamp_ms")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&event.timestamp)
                .ok()
                .map(|value| value.timestamp_millis())
        })
        .unwrap_or(0)
}

/// Attribute the merged log (`events`, already in the order the panel shows
/// it: timestamp, then seq) of the run `selected_execution_id` and its
/// delegated children.
///
/// `None` — no attribution at all, every row unchanged — when:
/// - the run has no approved plan (`plan_step_ids` empty);
/// - the run's log predates durable step starts: a step outcome with no
///   `step.started` before it;
/// - the run has ended without reporting any step at all. Nothing then says
///   the work was outside the plan rather than simply unreported, and a run
///   recorded before step starts were durable looks exactly like this.
///
/// A started id that is not one of the plan's steps opens no window.
pub(crate) fn attribute_run(
    selected_execution_id: &str,
    plan_step_ids: &HashSet<String>,
    run_is_terminal: bool,
    events: &[CanonicalEvent],
) -> Option<RunAttribution> {
    if plan_step_ids.is_empty() {
        return None;
    }
    let mut attribution = RunAttribution {
        per_event: Vec::with_capacity(events.len()),
        windows: Vec::new(),
    };
    let mut started: HashSet<String> = HashSet::new();
    let mut any_boundary = false;
    let mut current: Option<String> = None;
    for event in events {
        let own = event.execution_id == selected_execution_id;
        let event_type = event.event_type.as_str();
        if own
            && matches!(
                event_type,
                "step.started" | "step.completed" | "step.failed" | "step.skipped"
            )
        {
            any_boundary = true;
            let step_id = step_id_of(event).map(str::to_string);
            let at_ms = timestamp_ms_of(event);
            match (event_type, step_id) {
                ("step.started", Some(step_id)) => {
                    started.insert(step_id.clone());
                    close_window(&mut attribution.windows, &mut current, at_ms);
                    if plan_step_ids.contains(&step_id) {
                        attribution.windows.push(StepWindow {
                            step_id: step_id.clone(),
                            start_ms: at_ms,
                            end_ms: None,
                        });
                        current = Some(step_id);
                    }
                },
                ("step.completed" | "step.failed", Some(step_id)) => {
                    if !started.contains(&step_id) {
                        // Recorded before step starts were durable.
                        return None;
                    }
                    if current.as_deref() == Some(step_id.as_str()) {
                        close_window(&mut attribution.windows, &mut current, at_ms);
                    }
                },
                ("step.skipped", Some(step_id)) => {
                    if current.as_deref() == Some(step_id.as_str()) {
                        close_window(&mut attribution.windows, &mut current, at_ms);
                    }
                },
                _ => {},
            }
            attribution.per_event.push(None);
            continue;
        }
        if own && is_lifecycle_event(event_type) {
            attribution.per_event.push(None);
            continue;
        }
        // The run's own activity, or any row of a delegated child: the
        // child's whole run is work the parent delegated from its step.
        attribution.per_event.push(Some(match &current {
            Some(step_id) => Attribution::Step(step_id.clone()),
            None => Attribution::OffPlan,
        }));
    }
    if run_is_terminal && !any_boundary {
        return None;
    }
    Some(attribution)
}

fn close_window(windows: &mut [StepWindow], current: &mut Option<String>, at_ms: i64) {
    if current.take().is_some() {
        if let Some(window) = windows.last_mut().filter(|window| window.end_ms.is_none()) {
            window.end_ms = Some(at_ms);
        }
    }
}

/// Whether a progress-log row (`debug.timeline`) is activity, by the same
/// rule as [`attribute_run`]: run status changes, hand-overs and plan / run
/// lifecycle notifications frame the work; actions, child progress and the
/// agent's notifications are part of it.
pub(crate) fn progress_message_is_activity(message: &ProgressMessage) -> bool {
    match &message.kind {
        ProgressMessageKind::StatusChanged { .. } | ProgressMessageKind::HandedOver { .. } => false,
        ProgressMessageKind::ActionProgress { .. }
        | ProgressMessageKind::ChildStatusChanged { .. } => true,
        ProgressMessageKind::AgentNotification { event_type, .. } => {
            !is_lifecycle_event(event_type)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        execution_id: &str,
        seq: u64,
        event_type: &str,
        step_id: Option<&str>,
    ) -> CanonicalEvent {
        CanonicalEvent {
            event_id: format!("e{execution_id}{seq}"),
            seq,
            timestamp: format!("2026-10-01T00:00:{seq:02}Z"),
            principal: "alpha".to_string(),
            workspace: "default".to_string(),
            task_id: "task_x".to_string(),
            execution_id: execution_id.to_string(),
            plan_id: None,
            step_id: None,
            event_type: event_type.to_string(),
            ref_ids: Default::default(),
            payload: match step_id {
                Some(step_id) => serde_json::json!({
                    "step_id": step_id,
                    "timestamp_ms": (seq as i64) * 1000,
                }),
                None => serde_json::json!({ "timestamp_ms": (seq as i64) * 1000 }),
            },
        }
    }

    fn plan(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn step(id: &str) -> Option<Attribution> {
        Some(Attribution::Step(id.to_string()))
    }

    const OFF: Option<Attribution> = Some(Attribution::OffPlan);

    #[test]
    fn work_inside_a_step_window_belongs_to_that_step() {
        let events = vec![
            event("run", 1, "step.started", Some("s1")),
            event("run", 2, "llm.succeeded", None),
            event("run", 3, "tool.succeeded", None),
            event("run", 4, "step.completed", Some("s1")),
        ];
        let attribution = attribute_run("run", &plan(&["s1", "s2"]), false, &events).unwrap();
        assert_eq!(
            attribution.per_event,
            vec![None, step("s1"), step("s1"), None]
        );
        assert_eq!(
            attribution.windows,
            vec![StepWindow {
                step_id: "s1".to_string(),
                start_ms: 1000,
                end_ms: Some(4000),
            }]
        );
    }

    #[test]
    fn work_before_the_first_step_between_steps_and_after_the_last_is_off_plan() {
        let events = vec![
            event("run", 1, "llm.succeeded", None),
            event("run", 2, "step.started", Some("s1")),
            event("run", 3, "tool.succeeded", None),
            event("run", 4, "step.completed", Some("s1")),
            event("run", 5, "tool.succeeded", None),
            event("run", 6, "step.started", Some("s2")),
            event("run", 7, "tool.failed", None),
            event("run", 8, "step.failed", Some("s2")),
            event("run", 9, "llm.succeeded", None),
        ];
        let attribution = attribute_run("run", &plan(&["s1", "s2"]), true, &events).unwrap();
        assert_eq!(
            attribution.per_event,
            vec![
                OFF,
                None,
                step("s1"),
                None,
                OFF,
                None,
                step("s2"),
                None,
                OFF
            ]
        );
        assert_eq!(attribution.at(500), Attribution::OffPlan);
        assert_eq!(attribution.at(3000), Attribution::Step("s1".to_string()));
        assert_eq!(attribution.at(5000), Attribution::OffPlan);
        assert_eq!(attribution.at(7000), Attribution::Step("s2".to_string()));
        assert_eq!(attribution.at(9000), Attribution::OffPlan);
    }

    #[test]
    fn work_after_a_skipped_step_is_off_plan() {
        let events = vec![
            event("run", 1, "step.started", Some("s1")),
            event("run", 2, "tool.succeeded", None),
            event("run", 3, "step.skipped", Some("s1")),
            event("run", 4, "tool.succeeded", None),
        ];
        let attribution = attribute_run("run", &plan(&["s1"]), false, &events).unwrap();
        assert_eq!(attribution.per_event, vec![None, step("s1"), None, OFF]);
    }

    #[test]
    fn a_run_without_an_approved_plan_is_not_attributed() {
        let events = vec![event("run", 1, "tool.succeeded", None)];
        assert_eq!(attribute_run("run", &HashSet::new(), false, &events), None);
    }

    #[test]
    fn a_log_recorded_before_durable_step_starts_is_not_attributed() {
        let events = vec![
            event("run", 1, "tool.succeeded", None),
            event("run", 2, "step.completed", Some("s1")),
        ];
        assert_eq!(attribute_run("run", &plan(&["s1"]), true, &events), None);
    }

    #[test]
    fn a_finished_run_that_reported_no_step_is_not_attributed_but_a_live_one_is() {
        let events = vec![event("run", 1, "tool.succeeded", None)];
        assert_eq!(attribute_run("run", &plan(&["s1"]), true, &events), None);
        let live = attribute_run("run", &plan(&["s1"]), false, &events).unwrap();
        assert_eq!(live.per_event, vec![OFF]);
    }

    #[test]
    fn a_started_id_outside_the_plan_opens_no_window() {
        let events = vec![
            event("run", 1, "step.started", Some("made-up")),
            event("run", 2, "tool.succeeded", None),
        ];
        let attribution = attribute_run("run", &plan(&["s1"]), false, &events).unwrap();
        assert_eq!(attribution.per_event, vec![None, OFF]);
        assert!(attribution.windows.is_empty());
    }

    #[test]
    fn lifecycle_rows_are_not_activity_but_a_delegated_childs_rows_are() {
        let events = vec![
            event("run", 1, "execution.started", None),
            event("run", 2, "step.started", Some("s1")),
            event("run", 3, "sub_goal.requested", None),
            event("child", 1, "execution.started", None),
            event("child", 2, "tool.succeeded", None),
            event("run", 4, "step.completed", Some("s1")),
            event("run", 5, "execution.completed", None),
        ];
        let attribution = attribute_run("run", &plan(&["s1"]), true, &events).unwrap();
        assert_eq!(
            attribution.per_event,
            vec![None, None, step("s1"), step("s1"), step("s1"), None, None]
        );
    }

    #[test]
    fn counts_split_rows_by_step_and_off_plan() {
        let rows = [
            Attribution::Step("s1".to_string()),
            Attribution::Step("s1".to_string()),
            Attribution::OffPlan,
            Attribution::Step("s2".to_string()),
        ];
        let (per_step, off_plan) = attribution_counts(rows.iter());
        assert_eq!(per_step.get("s1"), Some(&2));
        assert_eq!(per_step.get("s2"), Some(&1));
        assert_eq!(off_plan, 1);
    }
}
