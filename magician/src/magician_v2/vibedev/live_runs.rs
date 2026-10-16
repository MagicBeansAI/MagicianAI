//! Every project's in-flight and just-ended runs, one state each.
//!
//! The cockpit pages one project's history; an at-a-glance surface (the
//! desktop notch, a phone widget) needs the opposite cut: across every open
//! project, which runs are working, which wait on the owner, and which just
//! ended. A run is a root task plus its folded threaded turns
//! ([`run_pages::fold_into_runs`]); its state is derived from the stored task
//! rows and the scope's pending user requests on every read, never from
//! events, so a restart reports exactly what the previous process did.

use std::collections::{HashMap, HashSet};

use chrono::DateTime;
use serde::Serialize;

use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::vibedev::projects::{parse_vibedev_project_id, VibeDevProjectRecord};
use crate::magician_v2::vibedev::run_pages::fold_into_runs;

/// How long an ended run stays on the list when the caller does not say.
pub const DEFAULT_RECENT_WINDOW_MS: i64 = 30 * 60 * 1_000;
/// Most rows one read returns.
pub const MAX_LIVE_RUNS: usize = 50;

/// What the owner needs to know about a run at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveRunState {
    /// A question, an approval or a diff review is waiting on the owner.
    NeedsYou,
    /// The run is going.
    Working,
    /// The owner paused it.
    Paused,
    /// Ended badly.
    Failed,
    /// The owner (or a budget) stopped it.
    Stopped,
    /// Ended well.
    Done,
}

impl LiveRunState {
    pub fn is_ended(self) -> bool {
        matches!(self, Self::Failed | Self::Stopped | Self::Done)
    }
}

/// One run on the at-a-glance list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveRun {
    /// The run's root task.
    pub run_id: String,
    /// The task a steer or stop must name: the newest turn still going, else
    /// the newest turn.
    pub live_task_id: String,
    pub project_id: String,
    pub project_name: String,
    /// The root's title, which names the run in the cockpit rail.
    pub title: String,
    /// The newest turn's title when it is not the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_turn_title: Option<String>,
    pub state: LiveRunState,
    /// The raw task status of `live_task_id`.
    pub status: String,
    /// What the live turn is doing now, when the runtime said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// How the run ended, when it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Turns in the run, root included.
    pub turns: usize,
    /// When the run's root was created, for "running for 12 min".
    pub started_at: String,
    /// Coding spend of the root and live turn, in USD, when any was metered
    /// (filled by [`fill_costs`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// A run waiting on a diff approval: the approval's id (the proposal id),
    /// answered with `POST /hitl/{approval_id}/respond` (`apply` / `reject`).
    /// Filled by [`fill_approvals`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// The newest turn that waits on a diff approval (not settled, with a
    /// pending proposal): where [`fill_approvals`] looks. Server-side only.
    #[serde(skip)]
    pub approval_task_id: Option<String>,
    /// The crew this run is a lane of, and the lane's label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
}

const SETTLED: [&str; 4] = ["completed", "failed", "cancelled", "archived"];

fn is_settled(item: &TaskListItemV3) -> bool {
    SETTLED.contains(&item.status.as_str())
}

fn updated_ms(item: &TaskListItemV3) -> i64 {
    DateTime::parse_from_rfc3339(&item.updated_at)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}

fn waits_on_owner(item: &TaskListItemV3, waiting_task_ids: &HashSet<String>) -> bool {
    if item.awaiting_diff_approval || waiting_task_ids.contains(&item.id) {
        return true;
    }
    !is_settled(item) && (item.pending_question.is_some() || !item.pending_questions.is_empty())
}

fn ended_state(item: &TaskListItemV3) -> LiveRunState {
    match item.status.as_str() {
        "failed" => LiveRunState::Failed,
        "cancelled" | "archived" => LiveRunState::Stopped,
        _ => LiveRunState::Done,
    }
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The at-a-glance rows for `projects`.
///
/// `items` are the projects' task rows in any order (a row whose
/// `VibeDev project:` line names a project not in `projects` is dropped).
/// `waiting_task_ids` are the tasks a pending user request names. A run that
/// ended more than `recent_window_ms` before `now_ms` is left out. Rows come
/// back needs-you first, then working and paused, then ended, newest first
/// within each.
pub fn live_runs(
    projects: &[VibeDevProjectRecord],
    items: &[TaskListItemV3],
    waiting_task_ids: &HashSet<String>,
    now_ms: i64,
    recent_window_ms: i64,
) -> Vec<LiveRun> {
    let mut rows = Vec::new();
    for project in projects.iter().filter(|project| !project.archived) {
        let mut ids: HashSet<&str> = project.run_task_ids.iter().map(String::as_str).collect();
        if let Some(active) = project.active_root_task_id.as_deref() {
            ids.insert(active);
        }
        let owned: Vec<TaskListItemV3> = items
            .iter()
            .filter(|item| ids.contains(item.id.as_str()))
            .filter(|item| {
                parse_vibedev_project_id(&item.description)
                    .is_none_or(|owner| owner == project.project_id)
            })
            .cloned()
            .collect();
        let (roots, turns) = fold_into_runs(&owned, &project.run_links);
        for root in roots {
            let mut run: Vec<&TaskListItemV3> = vec![root];
            if let Some(folded) = turns.get(&root.id) {
                run.extend(folded.iter().copied());
            }
            if let Some(row) = live_run(
                project,
                root,
                &run,
                waiting_task_ids,
                now_ms,
                recent_window_ms,
            ) {
                rows.push(row);
            }
        }
    }
    rows.sort_by(|a, b| {
        a.state
            .is_ended()
            .cmp(&b.state.is_ended())
            .then_with(|| {
                (a.state != LiveRunState::NeedsYou).cmp(&(b.state != LiveRunState::NeedsYou))
            })
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| b.run_id.cmp(&a.run_id))
    });
    rows.truncate(MAX_LIVE_RUNS);
    rows
}

fn newest<'a>(items: impl Iterator<Item = &'a TaskListItemV3>) -> Option<&'a TaskListItemV3> {
    items.max_by(|a, b| {
        a.updated_at
            .cmp(&b.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    })
}

fn live_run(
    project: &VibeDevProjectRecord,
    root: &TaskListItemV3,
    run: &[&TaskListItemV3],
    waiting_task_ids: &HashSet<String>,
    now_ms: i64,
    recent_window_ms: i64,
) -> Option<LiveRun> {
    let latest = newest(run.iter().copied())?;
    let live = newest(run.iter().copied().filter(|item| !is_settled(item)));
    let focus = live.unwrap_or(latest);

    let state = if run
        .iter()
        .any(|item| waits_on_owner(item, waiting_task_ids))
    {
        LiveRunState::NeedsYou
    } else if let Some(live) = live {
        if live.status == "paused" {
            LiveRunState::Paused
        } else {
            LiveRunState::Working
        }
    } else {
        ended_state(latest)
    };
    if state.is_ended() && now_ms.saturating_sub(updated_ms(latest)) > recent_window_ms {
        return None;
    }

    Some(LiveRun {
        run_id: root.id.clone(),
        live_task_id: focus.id.clone(),
        project_id: project.project_id.clone(),
        project_name: project.name.clone(),
        title: root.title.clone(),
        latest_turn_title: (latest.id != root.id && latest.title != root.title)
            .then(|| latest.title.clone()),
        state,
        status: focus.status.clone(),
        step: if state.is_ended() {
            None
        } else {
            non_empty(focus.current_step_title.as_ref())
        },
        summary: if state.is_ended() {
            non_empty(latest.completion_summary.as_ref())
        } else {
            None
        },
        turns: run.len(),
        started_at: root.created_at.clone(),
        cost_usd: None,
        approval_id: None,
        approval_task_id: newest(
            run.iter()
                .copied()
                .filter(|item| item.awaiting_diff_approval),
        )
        .map(|item| item.id.clone()),
        crew_id: project
            .run_links
            .get(&root.id)
            .and_then(|link| link.crew_id.clone()),
        lane: project
            .run_links
            .get(&root.id)
            .and_then(|link| link.lane.clone()),
        updated_at: latest.updated_at.clone(),
        last_progress_at: focus.last_progress_at.clone(),
    })
}

/// Fill each row's `cost_usd` from the coding cost ledgers of its root and
/// live turn.
pub fn fill_costs(
    rows: &mut [LiveRun],
    service: &crate::magician_v2::artifact_v2::ArtifactV2Service,
    scope: &crate::magician_v2::artifact_v2::ScopeRef,
) {
    use crate::magician_v2::execution::coding_engine::cost_budget::CodingCostLedger;
    for row in rows.iter_mut() {
        let mut ids = vec![row.run_id.as_str()];
        if row.live_task_id != row.run_id {
            ids.push(row.live_task_id.as_str());
        }
        let micro: u64 = ids
            .into_iter()
            .map(|task_id| {
                let dir =
                    service
                        .workspace()
                        .task_dir(&scope.principal(), &scope.workspace(), task_id);
                CodingCostLedger::open(&dir).spent_micro_usd()
            })
            .sum();
        row.cost_usd = (micro > 0).then(|| micro as f64 / 1_000_000.0);
    }
}

/// Fill each waiting row's `approval_id` from the scope's pending diff
/// proposals: an unclaimed one of the turn that waits on a diff approval.
/// A proposal left by a settled turn is not offered; approving it would apply
/// an old diff over newer work.
pub fn fill_approvals(
    rows: &mut [LiveRun],
    approvals: &crate::magician_v2::artifact_v2::service::PendingDiffApprovals,
) {
    let unclaimed: HashSet<String> = approvals
        .unclaimed_pending()
        .iter()
        .map(|proposal| proposal.id.to_string())
        .collect();
    for row in rows
        .iter_mut()
        .filter(|row| row.state == LiveRunState::NeedsYou)
    {
        row.approval_id = row.approval_task_id.as_deref().and_then(|task_id| {
            approvals
                .proposal_ids_for_task(task_id)
                .iter()
                .find(|id| unclaimed.contains(*id))
                .cloned()
        });
    }
}

/// How many of a project's newest recorded runs [`project_task_ids`] reads.
/// Live and just-ended runs are recent; reading a project's whole history on
/// every poll and push tick grows without bound.
const RECENT_RUN_SCAN: usize = 150;

/// The task ids [`live_runs`] reads in its one batched task read: each
/// project's newest recorded runs, its active run, the lanes of its open
/// crews and its runs in `waiting_task_ids` (a run that waits on the owner
/// stays listed however old it is), plus the turns they continue (by their
/// typed link) so a follow-up still folds into its root.
pub fn project_task_ids(
    projects: &[VibeDevProjectRecord],
    waiting_task_ids: &HashSet<String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for project in projects.iter().filter(|project| !project.archived) {
        let skip = project.run_task_ids.len().saturating_sub(RECENT_RUN_SCAN);
        let waiting = project
            .run_task_ids
            .iter()
            .take(skip)
            .filter(|id| waiting_task_ids.contains(*id));
        let lanes = project
            .crews
            .iter()
            .filter(|crew| !crew.status.is_settled())
            .flat_map(|crew| crew.lanes.iter())
            .filter_map(|lane| lane.run_task_id.as_ref());
        let starts = project
            .run_task_ids
            .iter()
            .skip(skip)
            .chain(project.active_root_task_id.iter())
            .chain(waiting)
            .chain(lanes);
        for id in starts {
            let mut next = Some(id);
            // Bounded: a link cycle cannot loop.
            for _ in 0..RECENT_RUN_SCAN {
                let Some(id) = next else { break };
                if !seen.insert(id.clone()) {
                    break;
                }
                ids.push(id.clone());
                next = project
                    .run_links
                    .get(id)
                    .and_then(|link| link.parent_task_id.as_ref());
            }
        }
    }
    ids
}

/// Counts per state, for a badge that does not need the rows.
pub fn state_counts(rows: &[LiveRun]) -> HashMap<LiveRunState, usize> {
    let mut counts = HashMap::new();
    for row in rows {
        *counts.entry(row.state).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::vibedev::run_service::VIBEDEV_RUN_THREADED_TAG;
    use serde_json::json;

    const NOW: &str = "2026-10-12T12:00:00Z";

    fn now_ms() -> i64 {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .timestamp_millis()
    }

    fn project(id: &str, tasks: &[&str]) -> VibeDevProjectRecord {
        serde_json::from_value(json!({
            "project_id": id,
            "name": format!("Project {id}"),
            "chat_thread_id": "thread",
            "chat_session_id": "session",
            "run_task_ids": tasks,
            "created_at_ms": 0,
            "updated_at_ms": 0,
        }))
        .expect("project record")
    }

    fn task(
        id: &str,
        project: &str,
        status: &str,
        updated_at: &str,
        parent: Option<&str>,
    ) -> TaskListItemV3 {
        let mut description = format!("VibeDev coding request:\nVibeDev project: {project}\n");
        if let Some(parent) = parent {
            description.push_str(&format!("Parent task: {parent}\n"));
        }
        let tags = if parent.is_some() {
            json!([{ "id": "t", "name": VIBEDEV_RUN_THREADED_TAG }])
        } else {
            json!([])
        };
        serde_json::from_value(json!({
            "id": id, "title": format!("Title {id}"), "description": description,
            "status": status, "agent_id": "cto", "latest_root_execution_id": null,
            "last_completed_root_execution_id": null, "completion_summary": null,
            "completion_outcome": null, "tags": tags,
            "created_at": updated_at, "updated_at": updated_at,
        }))
        .expect("task list item")
    }

    #[test]
    fn an_old_run_waiting_on_the_owner_is_still_read() {
        let ids: Vec<String> = (0..RECENT_RUN_SCAN + 10)
            .map(|index| format!("t{index}"))
            .collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let projects = vec![project("p", &refs)];
        let read = project_task_ids(&projects, &HashSet::new());
        assert_eq!(read.len(), RECENT_RUN_SCAN);
        assert!(!read.contains(&"t0".to_string()));
        let waiting: HashSet<String> = ["t0".to_string(), "elsewhere".to_string()].into();
        let read = project_task_ids(&projects, &waiting);
        assert!(read.contains(&"t0".to_string()));
        assert!(
            !read.contains(&"elsewhere".to_string()),
            "only the project's own runs"
        );
    }

    fn states(rows: &[LiveRun]) -> Vec<(&str, LiveRunState)> {
        rows.iter()
            .map(|row| (row.run_id.as_str(), row.state))
            .collect()
    }

    #[test]
    fn a_running_turn_makes_its_run_working_and_names_the_turn_to_steer() {
        let projects = vec![project("p", &["a", "a2"])];
        let items = vec![
            task("a", "p", "completed", "2026-10-12T11:00:00Z", None),
            task("a2", "p", "running", "2026-10-12T11:50:00Z", Some("a")),
        ];
        let rows = live_runs(
            &projects,
            &items,
            &HashSet::new(),
            now_ms(),
            DEFAULT_RECENT_WINDOW_MS,
        );
        assert_eq!(states(&rows), vec![("a", LiveRunState::Working)]);
        assert_eq!(rows[0].live_task_id, "a2");
        assert_eq!(rows[0].turns, 2);
        assert_eq!(rows[0].latest_turn_title.as_deref(), Some("Title a2"));
        assert_eq!(rows[0].project_name, "Project p");
    }

    #[test]
    fn owner_waits_rank_first_and_old_ended_runs_drop_off() {
        let projects = vec![
            project("p", &["w", "q", "d", "old", "f"]),
            project("r", &["x"]),
        ];
        let mut asking = task("q", "p", "running", "2026-10-12T11:10:00Z", None);
        asking.awaiting_diff_approval = true;
        let items = vec![
            task("w", "p", "running", "2026-10-12T11:55:00Z", None),
            asking,
            task("d", "p", "completed", "2026-10-12T11:58:00Z", None),
            task("old", "p", "completed", "2026-10-12T10:00:00Z", None),
            task("f", "p", "failed", "2026-10-12T11:40:00Z", None),
            task("x", "r", "running", "2026-10-12T11:30:00Z", None),
        ];
        let waiting: HashSet<String> = ["x".to_string()].into();
        let rows = live_runs(
            &projects,
            &items,
            &waiting,
            now_ms(),
            DEFAULT_RECENT_WINDOW_MS,
        );
        assert_eq!(
            states(&rows),
            vec![
                ("x", LiveRunState::NeedsYou),
                ("q", LiveRunState::NeedsYou),
                ("w", LiveRunState::Working),
                ("d", LiveRunState::Done),
                ("f", LiveRunState::Failed),
            ]
        );
        let counts = state_counts(&rows);
        assert_eq!(counts.get(&LiveRunState::NeedsYou), Some(&2));
        // Only the turn that waits on a diff approval is where its id comes
        // from; a question-only wait has none.
        assert_eq!(rows[1].approval_task_id.as_deref(), Some("q"));
        assert_eq!(rows[0].approval_task_id, None);
        let mut rows = rows;
        fill_approvals(
            &mut rows,
            &crate::magician_v2::artifact_v2::service::PendingDiffApprovals::none(),
        );
        assert!(rows.iter().all(|row| row.approval_id.is_none()));
    }

    #[test]
    fn archived_projects_and_foreign_rows_are_left_out() {
        let mut archived = project("gone", &["g"]);
        archived.archived = true;
        let projects = vec![archived, project("p", &["a", "b"])];
        let items = vec![
            task("g", "gone", "running", "2026-10-12T11:59:00Z", None),
            task("a", "p", "running", "2026-10-12T11:59:00Z", None),
            // Listed by `p` but stamped for another project.
            task("b", "other", "running", "2026-10-12T11:59:00Z", None),
        ];
        let rows = live_runs(
            &projects,
            &items,
            &HashSet::new(),
            now_ms(),
            DEFAULT_RECENT_WINDOW_MS,
        );
        assert_eq!(states(&rows), vec![("a", LiveRunState::Working)]);
        assert_eq!(
            project_task_ids(&projects, &HashSet::new()),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn a_crew_lane_carries_its_crew_and_label() {
        let mut lane_project = project("p", &["a"]);
        lane_project.run_links.insert(
            "a".to_string(),
            crate::magician_v2::vibedev::projects::VibeDevRunLink {
                parent_task_id: None,
                crew_id: Some("crew-1".to_string()),
                lane: Some("API".to_string()),
            },
        );
        let items = vec![task("a", "p", "running", "2026-10-12T11:59:00Z", None)];
        let rows = live_runs(
            &[lane_project],
            &items,
            &HashSet::new(),
            now_ms(),
            DEFAULT_RECENT_WINDOW_MS,
        );
        assert_eq!(rows[0].crew_id.as_deref(), Some("crew-1"));
        assert_eq!(rows[0].lane.as_deref(), Some("API"));
    }

    #[test]
    fn a_stopped_run_reports_how_it_ended() {
        let projects = vec![project("p", &["s"])];
        let mut stopped = task("s", "p", "cancelled", "2026-10-12T11:59:00Z", None);
        stopped.completion_summary = Some("  Stopped by the owner ".to_string());
        stopped.current_step_title = Some("Editing".to_string());
        let rows = live_runs(
            &projects,
            &[stopped],
            &HashSet::new(),
            now_ms(),
            DEFAULT_RECENT_WINDOW_MS,
        );
        assert_eq!(rows[0].state, LiveRunState::Stopped);
        assert_eq!(rows[0].summary.as_deref(), Some("Stopped by the owner"));
        assert_eq!(rows[0].step, None);
    }
}
