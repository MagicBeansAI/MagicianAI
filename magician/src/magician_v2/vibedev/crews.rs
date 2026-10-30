//! VibeDev crews: one goal worked by several coding runs at once.
//!
//! A crew splits a goal into lanes. Each lane is an ordinary trusted VibeDev
//! run (its own clone and branch) started through [`VibeDevRunService`] with
//! the crew's id and the lane's label on its typed project link, so the rail,
//! the live-runs list and the notch already know which runs belong together.
//!
//! The lifecycle is server-owned and lives on the project record
//! (`VibeDevProjectRecord::crews`), so a restart resumes it:
//!
//! 1. **Proposed.** The lanes come from the caller or from the planner
//!    (`vibedev_crew_plan` operation). The owner is asked to start it — an
//!    Attention question the notch can answer — unless the caller started it.
//! 2. **Running.** Each lane is a run. [`spawn_vibedev_crew_supervisor`]
//!    starts lanes when the question is answered and re-reads lane runs when a
//!    run ends and on a slow sweep, so a missed event or a restart costs
//!    latency, never a stuck crew.
//! 3. **Ready / Failed / Cancelled.** When every lane has ended the crew
//!    settles and the owner gets one notice instead of one per lane.
//!
//! Lanes run in parallel only in trusted projects; a sandboxed project shares
//! one shadow copy and serializes its runs, so a crew there is refused.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::dispatch_intent::{DispatchIntentState, DispatchMode, VibeDevCodingChoice};
use super::projects::{
    mutate_project_store_at, read_vibedev_projects, vibedev_project_store_path, ProjectStoreEdit,
    VibeDevProjectRecord, VibeDevProjectTrust, VIBEDEV_THREAD_ID,
};
use super::run_service::{
    vibedev_run_task_tags, StartVibeDevBuild, VibeDevCockpitRun, VibeDevCodingCatalog,
    VibeDevRunService,
};
use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::user_requests::{RequestOption, UserRequest, UserRequestService};

/// Most lanes one crew may have. More lanes than the trusted-run concurrency
/// cap simply queue (`coding.trusted_max_concurrent_runs`).
pub const MAX_CREW_LANES: usize = 8;
/// Most integrator lanes one crew may run. After that a conflict is the
/// owner's to resolve, so a merge that keeps conflicting cannot loop. Only an
/// integrator that got a run counts as a try; one the budget stopped before it
/// started does not, but at most twice this many integrator lanes are ever
/// added. With at most one review lane, a crew never holds more than
/// `MAX_CREW_LANES + 2 * MAX_INTEGRATOR_LANES + 1` lanes.
pub const MAX_INTEGRATOR_LANES: usize = 3;
/// Smallest per-lane cap a crew budget hands out. The cap line prints whole
/// cents, and a cap that prints as `$0.00` reads back as no cap at all.
const MIN_LANE_BUDGET_USD: f64 = 0.01;
/// Longest goal a crew may carry.
pub const MAX_CREW_GOAL_CHARS: usize = 8_000;
/// Longest brief one lane may carry.
pub const MAX_LANE_BRIEF_CHARS: usize = 4_000;
/// Most paths one lane may claim.
pub const MAX_LANE_OWNS: usize = 32;
/// The start question's type; its id is [`crew_start_request_id`].
pub const CREW_START_REQUEST_TYPE: &str = "vibedev.crew_start";
const START: &str = "start";
const CANCEL: &str = "cancel";
/// The pull-request question's type; its id is [`crew_pull_request_request_id`].
pub const CREW_PULL_REQUEST_TYPE: &str = "vibedev.crew_pull_request";
const OPEN_PULL_REQUEST: &str = "open_pr";
const LATER: &str = "later";
/// Unanswered for a week, the start question closes as not started.
const START_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;
/// How often the supervisor re-reads running crews when no event nudged it.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// `created_by` on a lane's task.
pub const CREW_LANE_CREATED_BY: &str = "vibedev_crew";
/// How long a lane whose run is admitted but not yet created (its dispatch
/// intent still `pending`) may stay so before the crew gives up on it. A
/// claimed intent is waited for: its holder, or the next restart's recovery,
/// creates the task.
const LANE_ADMISSION_GRACE_SECS: i64 = 30 * 60;
/// Failed merges of a ready crew in a row before the owner hears about it.
/// From the first failure the retries back off ([`merge_retry_delay`]).
const MERGE_FAILURES_BEFORE_NOTICE: u32 = 3;
/// Longest wait between two merge retries of a ready crew.
const MAX_MERGE_RETRY_DELAY: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewStatus {
    /// Planned; waiting for the owner (or a caller) to start it.
    #[default]
    Proposed,
    /// Lanes are running.
    Running,
    /// A race finished: waiting for the owner to pick the lane to keep.
    Choosing,
    /// Every lane finished well; their branches are being merged.
    Ready,
    /// The lanes are merged into one published branch (`branch`).
    Integrated,
    /// A lane's branch conflicts with the lanes merged before it
    /// (`conflict`). Resolve it on the branches, then retry.
    Conflict,
    /// Every lane has ended and at least one did not finish well.
    Failed,
    /// Stopped, or never started.
    Cancelled,
}

impl CrewStatus {
    /// Nothing more happens without the owner.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::Conflict | Self::Failed | Self::Cancelled
        )
    }
}

/// The lane whose branch would not merge, and the files it conflicts in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewConflict {
    pub lane: String,
    pub files: Vec<String>,
}

/// How a crew uses its lanes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewMode {
    /// The goal is split; every lane's branch is merged.
    #[default]
    Split,
    /// Every lane attempts the whole goal (usually on different engines); the
    /// owner keeps one.
    Race,
}

/// What a lane is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneRole {
    /// Part of the goal (or, in a race, an attempt at all of it).
    #[default]
    Work,
    /// Merges the work lanes and resolves the conflict the server could not.
    Integrate,
    /// Reviews the merged branch and fixes what it finds.
    Review,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Stopped,
}

impl LaneStatus {
    pub fn is_ended(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Stopped)
    }

    /// A lane's status from its run task's status.
    pub fn from_task_status(status: &str) -> Self {
        match status {
            "completed" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" | "archived" => Self::Stopped,
            _ => Self::Running,
        }
    }
}

/// One lane as a caller or the planner describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewLanePlan {
    pub lane: String,
    pub brief: String,
    /// Repo-relative paths (files or folders) this lane changes; other lanes
    /// are told to leave them alone.
    #[serde(default)]
    pub owns: Vec<String>,
    /// A coding profile for this lane; `None` uses the project's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_profile_id: Option<String>,
}

/// One lane of a crew.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewLane {
    pub lane: String,
    pub brief: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_profile_id: Option<String>,
    /// The lane's run, once started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_task_id: Option<String>,
    #[serde(default)]
    pub status: LaneStatus,
    #[serde(default)]
    pub role: LaneRole,
}

/// A crew, stored on its project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VibeDevCrew {
    pub crew_id: String,
    pub goal: String,
    #[serde(default)]
    pub status: CrewStatus,
    #[serde(default)]
    pub mode: CrewMode,
    pub lanes: Vec<CrewLane>,
    /// After the merge, a review lane checks the merged branch and fixes what
    /// it finds before the pull-request question.
    #[serde(default)]
    pub review: bool,
    /// In a race, the lane the owner kept (its index in `lanes`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<usize>,
    /// The race's "which lane do you keep?" question, while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choose_request_id: Option<String>,
    /// The "let an agent resolve the conflict?" question, while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_request_id: Option<String>,
    /// Spend cap for all lanes together, in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// What the lanes have spent together, in USD, as last read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_usd: Option<f64>,
    /// The start question, while one is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_request_id: Option<String>,
    /// The integration branch the lanes are merged into, once published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<CrewConflict>,
    /// The pull-request question, while one is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    /// The last thing that happened, in one line, for surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl VibeDevCrew {
    pub fn lane_task_ids(&self) -> Vec<String> {
        self.lanes
            .iter()
            .filter_map(|lane| lane.run_task_id.clone())
            .collect()
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Validate and tidy lanes: 1 to [`MAX_CREW_LANES`], distinct one-line labels,
/// a brief each, repo-relative `owns` paths that do not climb out of the repo.
pub fn normalize_crew_lanes(lanes: Vec<CrewLanePlan>) -> Result<Vec<CrewLane>, String> {
    if lanes.is_empty() {
        return Err("a crew needs at least one lane".to_string());
    }
    if lanes.len() > MAX_CREW_LANES {
        return Err(format!("a crew has at most {MAX_CREW_LANES} lanes"));
    }
    let mut labels = HashSet::new();
    let mut normalized = Vec::with_capacity(lanes.len());
    for plan in lanes {
        let lane = one_line(&plan.lane);
        if lane.is_empty() {
            return Err("every lane needs a label".to_string());
        }
        if lane.chars().count() > super::projects::MAX_LANE_CHARS {
            return Err(format!(
                "lane label is longer than {} characters",
                super::projects::MAX_LANE_CHARS
            ));
        }
        if !labels.insert(lane.to_lowercase()) {
            return Err(format!("two lanes are both called `{lane}`"));
        }
        let brief = plan.brief.trim().to_string();
        if brief.is_empty() {
            return Err(format!("lane `{lane}` needs a brief"));
        }
        if brief.chars().count() > MAX_LANE_BRIEF_CHARS {
            return Err(format!(
                "lane `{lane}`'s brief is longer than {MAX_LANE_BRIEF_CHARS} characters"
            ));
        }
        if plan.owns.len() > MAX_LANE_OWNS {
            return Err(format!(
                "lane `{lane}` claims more than {MAX_LANE_OWNS} paths"
            ));
        }
        let mut owns = Vec::new();
        for path in plan.owns {
            let path = path.trim().trim_start_matches("./").to_string();
            if path.is_empty() {
                continue;
            }
            if path.starts_with('/')
                || path.contains('\\')
                || path.split('/').any(|part| part == "..")
                || path.chars().any(char::is_control)
            {
                return Err(format!(
                    "lane `{lane}` path `{path}` must be relative to the repo and stay inside it"
                ));
            }
            if !owns.contains(&path) {
                owns.push(path);
            }
        }
        let coding_profile_id = plan
            .coding_profile_id
            .map(|id| one_line(&id))
            .filter(|id| !id.is_empty());
        normalized.push(CrewLane {
            lane,
            brief,
            owns,
            coding_profile_id,
            run_task_id: None,
            status: LaneStatus::Pending,
            role: LaneRole::Work,
        });
    }
    Ok(normalized)
}

/// Validate a crew goal.
pub fn normalize_crew_goal(goal: &str) -> Result<String, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("a crew needs a goal".to_string());
    }
    if goal.chars().count() > MAX_CREW_GOAL_CHARS {
        return Err(format!(
            "the goal is longer than {MAX_CREW_GOAL_CHARS} characters"
        ));
    }
    Ok(goal.to_string())
}

/// Validate a crew budget: at least a cent, the smallest cap a lane can get.
pub fn normalize_crew_budget(budget_usd: Option<f64>) -> Result<Option<f64>, String> {
    match budget_usd {
        None => Ok(None),
        Some(value) if value.is_finite() && value >= MIN_LANE_BUDGET_USD => Ok(Some(value)),
        Some(_) => Err(format!(
            "budget_usd must be a number of at least ${MIN_LANE_BUDGET_USD:.2}"
        )),
    }
}

/// What a lane's request needs beyond the crew: the work lanes' published
/// branches and the crew's merged branch with its base.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneContext {
    /// `(lane label, branch)` for every work lane that has a branch, in order.
    pub branches: Vec<(String, String)>,
    /// The crew's integration branch, once it exists.
    pub crew_branch: Option<String>,
    /// The commit the crew's integration branch started from.
    pub base: Option<String>,
}

/// The request a lane's run is started with.
///
/// A work lane gets the shared goal, its brief and claim, and every other work
/// lane's claim, so parallel runs stay out of each other's way. A race lane
/// gets the whole goal. An integrate lane gets the branches to merge and the
/// conflict to resolve; a review lane gets the merged branch to check.
///
/// The first line is the lane's task on its own: the run's title is taken
/// from it (`vibedev_run_task_title`), so it must say what the lane does
/// rather than open with the crew preamble.
pub fn lane_request(crew: &VibeDevCrew, index: usize, context: &LaneContext) -> String {
    let lane = &crew.lanes[index];
    let (task, body) = match (lane.role, crew.mode) {
        (LaneRole::Integrate, _) => (
            "Merge the crew's lane branches and resolve their conflicts".to_string(),
            integrate_lane_request(crew, context),
        ),
        (LaneRole::Review, _) => (
            "Review the crew's merged branch before its pull request".to_string(),
            review_lane_request(crew, context),
        ),
        (LaneRole::Work, CrewMode::Race) => (one_line(&crew.goal), race_lane_request(crew, index)),
        (LaneRole::Work, CrewMode::Split) => {
            (one_line(&lane.brief), work_lane_request(crew, index))
        },
    };
    format!("{task}\n\n{body}")
}

fn work_lanes(crew: &VibeDevCrew) -> impl Iterator<Item = (usize, &CrewLane)> {
    crew.lanes
        .iter()
        .enumerate()
        .filter(|(_, lane)| lane.role == LaneRole::Work)
}

/// How many integrator lanes the crew has run (or is running). An integrator
/// that never got a run (the budget stopped it, or it could not start) is not
/// a try.
pub fn integrator_attempts(crew: &VibeDevCrew) -> usize {
    crew.lanes
        .iter()
        .filter(|lane| lane.role == LaneRole::Integrate && lane.run_task_id.is_some())
        .count()
}

/// Whether the owner may still hand a conflict to an integrator lane: a split
/// crew under [`MAX_INTEGRATOR_LANES`] tries, and under twice that many
/// integrator lanes in all, so lanes that never started cannot pile up.
pub fn can_add_integrator(crew: &VibeDevCrew) -> bool {
    let added = crew
        .lanes
        .iter()
        .filter(|lane| lane.role == LaneRole::Integrate)
        .count();
    crew.mode == CrewMode::Split
        && integrator_attempts(crew) < MAX_INTEGRATOR_LANES
        && added < 2 * MAX_INTEGRATOR_LANES
}

/// Whether the crew's budget, as last read (`spent_usd`), still has a cent
/// for another lane. Always true without a budget.
pub fn crew_budget_left(crew: &VibeDevCrew) -> bool {
    remaining_lane_budget(crew.budget_usd, crew.spent_usd.unwrap_or(0.0)).is_ok()
}

/// How a crew budget is shared by lanes about to start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneBudgetShare {
    /// Each starting lane's own cap; `None` without a crew budget.
    pub per_lane_usd: Option<f64>,
    /// How many of the waiting lanes start now (the first ones, in order).
    pub starting: usize,
}

/// Share what the budget has left among the `pending` lanes about to start
/// and the `running` lanes that may still spend.
///
/// Each lane's cap is its own ledger, so every lane gets `left / (pending +
/// running)` rounded down to whole cents, and the lanes together stay within
/// the budget. When that is under a cent, fewer lanes start (at least one
/// when none is running) and the rest wait for running lanes to end. `Err`
/// says the budget is spent.
pub fn share_lane_budget(
    budget_usd: Option<f64>,
    spent_usd: f64,
    pending: usize,
    running: usize,
) -> Result<LaneBudgetShare, String> {
    let Some(budget) = budget_usd else {
        return Ok(LaneBudgetShare {
            per_lane_usd: None,
            starting: pending,
        });
    };
    let left = ((budget - spent_usd) * 100.0).floor();
    if !left.is_finite() || left < 1.0 {
        return Err(format!("the crew's ${budget:.2} budget is spent"));
    }
    // A finite value past `u64::MAX` saturates; no budget gets near it.
    let left_cents = left as u64;
    let (pending_n, running_n) = (pending as u64, running as u64);
    let starting = if left_cents >= pending_n + running_n {
        pending_n
    } else if running_n == 0 {
        left_cents.min(pending_n)
    } else {
        // The running lanes' shares come first; the rest start as they end.
        left_cents.saturating_sub(running_n).min(pending_n)
    };
    let per_lane_cents = (left_cents / (starting + running_n).max(1)).max(1);
    Ok(LaneBudgetShare {
        per_lane_usd: Some(per_lane_cents as f64 / 100.0),
        starting: starting as usize,
    })
}

/// What a new lane may spend: `Ok(None)` without a crew budget, else the
/// budget left after `spent_usd`, rounded down to whole cents. `Err` says the
/// budget is spent.
pub fn remaining_lane_budget(
    budget_usd: Option<f64>,
    spent_usd: f64,
) -> Result<Option<f64>, String> {
    let Some(budget) = budget_usd else {
        return Ok(None);
    };
    let left = ((budget - spent_usd) * 100.0).floor() / 100.0;
    if left.is_finite() && left >= MIN_LANE_BUDGET_USD {
        Ok(Some(left))
    } else {
        Err(format!("the crew's ${budget:.2} budget is spent"))
    }
}

fn work_lane_request(crew: &VibeDevCrew, index: usize) -> String {
    let lane = &crew.lanes[index];
    let count = work_lanes(crew).count();
    let mut text = format!(
        "You are one lane of a VibeDev crew: {count} runs working one goal in parallel, each on \
         its own branch. The branches are merged afterwards, so keep to your lane.\n\n\
         Crew goal:\n{}\n\nYour lane: {}\n{}\n",
        crew.goal, lane.lane, lane.brief
    );
    if lane.owns.is_empty() {
        text.push_str("\nYou claimed no specific paths; keep your changes focused on your lane.\n");
    } else {
        text.push_str("\nPaths you own (change these):\n");
        for path in &lane.owns {
            text.push_str(&format!("- {path}\n"));
        }
    }
    let others: Vec<&CrewLane> = work_lanes(crew)
        .filter(|(other, _)| *other != index)
        .map(|(_, lane)| lane)
        .collect();
    if !others.is_empty() {
        text.push_str("\nOther lanes (do not change what they own):\n");
        for other in others {
            if other.owns.is_empty() {
                text.push_str(&format!("- {}\n", other.lane));
            } else {
                text.push_str(&format!("- {}: {}\n", other.lane, other.owns.join(", ")));
            }
        }
        text.push_str(
            "\nIf your lane cannot be done without touching another lane's paths, make the \
             smallest change needed and say so in your summary.\n",
        );
    }
    text
}

fn race_lane_request(crew: &VibeDevCrew, index: usize) -> String {
    let count = work_lanes(crew).count();
    format!(
        "You are one of {count} coding agents attempting the same goal independently, each on \
         its own branch. The owner keeps one result, so deliver a complete, working change on \
         your own; do not split the work.\n\nGoal:\n{}\n\nYour attempt: {}\n{}\n",
        crew.goal, crew.lanes[index].lane, crew.lanes[index].brief
    )
}

fn integrate_lane_request(crew: &VibeDevCrew, context: &LaneContext) -> String {
    let mut text = format!(
        "You integrate a VibeDev crew. Its lanes worked one goal in parallel on separate \
         branches, and merging them conflicted. Merge every lane branch into your branch, in \
         this order, resolving each conflict so that every lane's intent survives:\n\n"
    );
    for (lane, branch) in &context.branches {
        text.push_str(&format!(
            "- {lane}: `git fetch origin {branch} && git merge --no-ff FETCH_HEAD`\n"
        ));
    }
    if let Some(conflict) = crew.conflict.as_ref() {
        text.push_str(&format!(
            "\nThe automatic merge stopped at lane {} in: {}.\n",
            conflict.lane,
            conflict.files.join(", ")
        ));
    }
    text.push_str(&format!(
        "\nCrew goal:\n{}\n\nAfter merging, make the project build and its checks pass. Do not \
         add features; change only what integration needs, and summarize each conflict and how \
         you resolved it.\n",
        crew.goal
    ));
    text
}

fn review_lane_request(crew: &VibeDevCrew, context: &LaneContext) -> String {
    let branch = context.crew_branch.as_deref().unwrap_or("the crew branch");
    let base = context.base.as_deref().unwrap_or("the base branch");
    let lanes: Vec<&str> = work_lanes(crew)
        .map(|(_, lane)| lane.lane.as_str())
        .collect();
    format!(
        "You review a VibeDev crew's merged work before it goes to a pull request. First put \
         your branch on the merged result: `git fetch origin {branch} && git reset --hard \
         FETCH_HEAD`. Then review everything the crew changed (`git diff {base}...HEAD`).\n\n\
         Crew goal:\n{}\n\nLanes merged: {}\n\nLook for bugs, places where the lanes \
         disagree (names, interfaces, duplicated code), parts of the goal nobody built, and \
         failing builds or checks. Fix what you find with small, focused commits. Do not \
         rewrite working code for style. End with a short review: what you checked, what you \
         fixed, and anything left for the owner.\n",
        crew.goal,
        lanes.join(", ")
    )
}

/// The lanes of a race: one per given profile, else `racers` lanes on
/// distinct eligible engines first, then cycling through them.
pub fn race_lanes(
    goal: &str,
    profiles: Option<Vec<String>>,
    racers: Option<usize>,
    catalog: &VibeDevCodingCatalog,
) -> Result<Vec<CrewLane>, String> {
    let chosen: Vec<String> = match profiles {
        Some(profiles) => {
            let profiles: Vec<String> = profiles
                .into_iter()
                .map(|id| one_line(&id))
                .filter(|id| !id.is_empty())
                .collect();
            if profiles.len() < 2 {
                return Err("a race needs at least two profiles".to_string());
            }
            profiles
        },
        None => {
            let count = racers.unwrap_or(3);
            if count < 2 {
                return Err("a race needs at least two racers".to_string());
            }
            // Checked before the list is built: the count is the caller's.
            if count > MAX_CREW_LANES {
                return Err(format!("a crew has at most {MAX_CREW_LANES} lanes"));
            }
            let eligible: Vec<_> = catalog
                .entries
                .iter()
                .filter(|entry| entry.eligible)
                .map(|entry| &entry.definition)
                .collect();
            // Different engines first: that is what makes a race worth running.
            let mut seen = Vec::new();
            let mut ordered: Vec<String> = Vec::new();
            for definition in &eligible {
                let engine = format!("{:?}", definition.engine());
                if !seen.contains(&engine) {
                    seen.push(engine);
                    ordered.push(definition.id().to_string());
                }
            }
            for definition in &eligible {
                let id = definition.id().to_string();
                if !ordered.contains(&id) {
                    ordered.push(id);
                }
            }
            if ordered.is_empty() {
                ordered.push(catalog.default_profile_id.clone());
            }
            (0..count)
                .map(|i| ordered[i % ordered.len()].clone())
                .collect()
        },
    };
    if chosen.len() > MAX_CREW_LANES {
        return Err(format!("a crew has at most {MAX_CREW_LANES} lanes"));
    }
    let plans = chosen
        .iter()
        .enumerate()
        .map(|(i, profile)| CrewLanePlan {
            lane: format!("Attempt {} · {profile}", i + 1),
            brief: format!(
                "Attempt the whole goal on the `{profile}` coding profile: {}",
                one_line(goal)
            ),
            owns: Vec::new(),
            coding_profile_id: Some(profile.clone()),
        })
        .collect();
    normalize_crew_lanes(plans)
}

/// Still in play: running, waiting on the owner, or merged with no pull
/// request yet. The rail shows these; the rest are earlier crews, paged
/// (`page_crews`). The cockpit's `isCrewOpen` is the same rule.
pub fn crew_is_open(crew: &VibeDevCrew) -> bool {
    match crew.status {
        CrewStatus::Proposed
        | CrewStatus::Running
        | CrewStatus::Choosing
        | CrewStatus::Ready
        | CrewStatus::Conflict => true,
        CrewStatus::Integrated => crew.pull_request_url.is_none(),
        CrewStatus::Failed | CrewStatus::Cancelled => false,
    }
}

/// Which crews a list read asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrewListState {
    /// Every crew, newest first (the original whole list).
    All,
    /// Crews still in play, newest first.
    Open,
    /// The other crews, most recently changed first, a page at a time.
    Settled,
}

impl CrewListState {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("all") => Ok(Self::All),
            Some("open") => Ok(Self::Open),
            Some("settled") => Ok(Self::Settled),
            Some(other) => Err(format!(
                "unknown crew state `{other}`; use open, settled or all"
            )),
        }
    }
}

/// Default and largest page of settled crews.
pub const CREW_PAGE_DEFAULT: usize = 5;
pub const CREW_PAGE_MAX: usize = 50;

/// One read of a project's crews.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewPage {
    pub crews: Vec<VibeDevCrew>,
    /// Crews matching the state, before paging.
    pub total: usize,
    /// Settled crews in the project (what the rail's "earlier crews" counts).
    pub settled_total: usize,
}

/// Filter, order and page a project's crews. Only `Settled` pages: open crews
/// are few and all shown.
pub fn page_crews(
    crews: Vec<VibeDevCrew>,
    state: CrewListState,
    offset: usize,
    limit: Option<usize>,
) -> CrewPage {
    let settled_total = crews.iter().filter(|crew| !crew_is_open(crew)).count();
    let mut chosen: Vec<VibeDevCrew> = match state {
        CrewListState::All => crews,
        CrewListState::Open => crews.into_iter().filter(crew_is_open).collect(),
        CrewListState::Settled => crews
            .into_iter()
            .filter(|crew| !crew_is_open(crew))
            .collect(),
    };
    if state == CrewListState::Settled {
        chosen.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
    } else {
        chosen.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    }
    let total = chosen.len();
    if state == CrewListState::Settled {
        let limit = limit.unwrap_or(CREW_PAGE_DEFAULT).clamp(1, CREW_PAGE_MAX);
        chosen = chosen.into_iter().skip(offset).take(limit).collect();
    }
    CrewPage {
        crews: chosen,
        total,
        settled_total,
    }
}

/// A crew's status once its lanes are re-read.
pub fn crew_status_after_lanes(crew: &VibeDevCrew) -> CrewStatus {
    if crew.lanes.iter().any(|lane| !lane.status.is_ended()) {
        return CrewStatus::Running;
    }
    let work: Vec<&CrewLane> = work_lanes(crew).map(|(_, lane)| lane).collect();
    let none_done = |lanes: &[&CrewLane]| {
        if lanes.iter().all(|lane| lane.status == LaneStatus::Stopped) {
            CrewStatus::Cancelled
        } else {
            CrewStatus::Failed
        }
    };
    match crew.mode {
        CrewMode::Race => {
            if !work.iter().any(|lane| lane.status == LaneStatus::Done) {
                return none_done(&work);
            }
            if crew.winner.is_none() {
                return CrewStatus::Choosing;
            }
        },
        CrewMode::Split => {
            if !work.iter().all(|lane| lane.status == LaneStatus::Done) {
                return none_done(&work);
            }
            // Only while the conflict the integrator was for is still open: a
            // merge (or the owner's retry) clears it, and an old failed
            // integrator must not turn a later merge and review back into a
            // conflict.
            let integrator = crew
                .lanes
                .iter()
                .rev()
                .find(|lane| lane.role == LaneRole::Integrate);
            if crew.conflict.is_some()
                && integrator.is_some_and(|lane| lane.status != LaneStatus::Done)
            {
                return CrewStatus::Conflict;
            }
        },
    }
    CrewStatus::Ready
}

/// [`crew_status_after_lanes`] for a split crew of `lanes`.
pub fn crew_status_from_lanes(lanes: &[CrewLane]) -> CrewStatus {
    let crew = VibeDevCrew {
        crew_id: String::new(),
        goal: String::new(),
        status: CrewStatus::Running,
        mode: CrewMode::Split,
        lanes: lanes.to_vec(),
        review: false,
        winner: None,
        choose_request_id: None,
        resolve_request_id: None,
        budget_usd: None,
        spent_usd: None,
        start_request_id: None,
        branch: None,
        conflict: None,
        pull_request_request_id: None,
        pull_request_url: None,
        note: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    crew_status_after_lanes(&crew)
}

/// One line describing where the lanes are.
pub fn lanes_summary(lanes: &[CrewLane]) -> String {
    let count = |status: LaneStatus| lanes.iter().filter(|lane| lane.status == status).count();
    let mut parts = Vec::new();
    for (status, word) in [
        (LaneStatus::Done, "done"),
        (LaneStatus::Running, "running"),
        (LaneStatus::Pending, "waiting"),
        (LaneStatus::Failed, "failed"),
        (LaneStatus::Stopped, "stopped"),
    ] {
        let n = count(status);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    format!(
        "{} of {} lanes: {}",
        count(LaneStatus::Done),
        lanes.len(),
        parts.join(", ")
    )
}

// ─── Planning ───────────────────────────────────────────────────────────────

/// The operation the planner runs as (`llm-router.yaml` → `operation_mapping`).
pub const CREW_PLAN_OPERATION: &str = "vibedev_crew_plan";
const CREW_PLAN_PROMPT: &str = "vibedev_crew_plan";
const CREW_PLAN_PROMPT_VERSION: &str = "1.1.0";
const CREW_PLAN_FALLBACK: &str = "You split one software goal into parallel lanes for a crew of coding agents. \
Each lane runs at the same time as the others on its own git branch of the same repository, and the branches are merged afterwards.\n\n\
Project: {project_name}\nGoal:\n{goal}\n\n\
Repository files on the branch the lanes start from:\n{repo_files}\n\n\
Rules:\n- Use between 2 and {max_lanes} lanes; use fewer when the goal is small. One lane is allowed only when the goal cannot be split.\n\
- Lanes must be independent: no lane may need another lane's result to start.\n\
- Give each lane the repo-relative paths (files or folders) it will change in `owns`, and keep those sets disjoint so the merge is clean. \
Take the paths from the repository files above; put a new file in the existing folder that fits it, unless the goal asks for a new folder. \
Leave `owns` empty when the files are not listed and you cannot tell.\n\
- `lane` is a short label (one to three words). `brief` says exactly what to build or change and how to tell it is done.\n\n\
Output ONLY a JSON object: {\"lanes\": [{\"lane\": \"...\", \"brief\": \"...\", \"owns\": [\"...\"]}]}\nNo preamble, no code fences.";

#[derive(Deserialize)]
struct CrewPlanWrapper {
    #[serde(default)]
    lanes: Vec<CrewLanePlan>,
}

fn strip_fences(content: &str) -> &str {
    let mut text = content.trim();
    for fence in ["```json", "```"] {
        if let Some(rest) = text.strip_prefix(fence) {
            text = rest.trim();
            break;
        }
    }
    text.strip_suffix("```").map(str::trim).unwrap_or(text)
}

/// Parse the planner's answer: `{"lanes": [...]}` or a bare array, optionally
/// fenced. Unknown fields are ignored.
pub fn parse_crew_plan(content: &str) -> Result<Vec<CrewLanePlan>, String> {
    let text = strip_fences(content);
    if let Ok(wrapper) = serde_json::from_str::<CrewPlanWrapper>(text) {
        if !wrapper.lanes.is_empty() {
            return Ok(wrapper.lanes);
        }
    }
    serde_json::from_str::<Vec<CrewLanePlan>>(text)
        .map_err(|error| format!("the planner's answer is not a lane list: {error}"))
}

/// Most lines of repository layout the planner is shown.
const CREW_PLAN_MAX_LAYOUT_LINES: usize = 400;

/// The repository layout the planner is shown: every path when they fit,
/// else folders with their file counts (two levels deep, then one), cut at
/// `max_lines`.
pub fn crew_plan_repo_layout(files: &[String], max_lines: usize) -> String {
    if files.is_empty() {
        return "(no committed files yet)".to_string();
    }
    if files.len() <= max_lines {
        return files.join("\n");
    }
    let folders = |depth: usize| {
        let mut entries = std::collections::BTreeMap::<String, usize>::new();
        for path in files {
            let parts: Vec<&str> = path.split('/').collect();
            let key = if parts.len() == 1 {
                path.clone()
            } else {
                format!("{}/", parts[..depth.min(parts.len() - 1)].join("/"))
            };
            *entries.entry(key).or_default() += 1;
        }
        entries
    };
    let mut entries = folders(2);
    if entries.len() > max_lines {
        entries = folders(1);
    }
    let shown = entries.len().min(max_lines);
    let mut lines: Vec<String> = entries
        .iter()
        .take(shown)
        .map(|(key, count)| match (key.ends_with('/'), count) {
            (false, _) => key.clone(),
            (true, 1) => format!("{key} (1 file)"),
            (true, count) => format!("{key} ({count} files)"),
        })
        .collect();
    if entries.len() > shown {
        lines.push(format!("… and {} more", entries.len() - shown));
    }
    format!(
        "{} files; folders show how many files they hold.\n{}",
        files.len(),
        lines.join("\n")
    )
}

/// The layout of the project's repository on the branch its lanes start from,
/// or a note saying why it is not available.
async fn crew_plan_repo_files(project: &VibeDevProjectRecord) -> String {
    let Some(repo) = project
        .repo_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return "(the project has no repository)".to_string();
    };
    let repo = std::path::PathBuf::from(repo);
    let base_branch = project.base_branch.clone();
    let listed = tokio::task::spawn_blocking(move || {
        super::trusted_workspace::base_branch_files(&repo, base_branch.as_deref())
    })
    .await;
    match listed {
        Ok(Ok(files)) => crew_plan_repo_layout(&files, CREW_PLAN_MAX_LAYOUT_LINES),
        Ok(Err(error)) => format!("(the files could not be listed: {error})"),
        Err(error) => format!("(the files could not be listed: {error})"),
    }
}

/// Ask the planner to split `goal` into lanes.
pub async fn plan_crew_lanes(
    resources: &AgentResources,
    scope: &ScopeRef,
    project: &VibeDevProjectRecord,
    goal: &str,
    max_lanes: usize,
) -> Result<Vec<CrewLane>, String> {
    let router = resources
        .operation_llm_router
        .as_ref()
        .ok_or_else(|| "no LLM router is configured to plan a crew".to_string())?;
    let max_lanes = max_lanes.clamp(1, MAX_CREW_LANES);
    let repo_files = crew_plan_repo_files(project).await;
    let variables = std::collections::HashMap::from([
        ("project_name".to_string(), project.name.clone()),
        ("goal".to_string(), goal.to_string()),
        ("repo_files".to_string(), repo_files.clone()),
        ("max_lanes".to_string(), max_lanes.to_string()),
    ]);
    let fallback = CREW_PLAN_FALLBACK
        .replace("{project_name}", &project.name)
        .replace("{max_lanes}", &max_lanes.to_string())
        .replace("{repo_files}", &repo_files)
        .replace("{goal}", goal);
    let prompt = magician_core::prompts::rendered_prompt_or(
        CREW_PLAN_PROMPT,
        CREW_PLAN_PROMPT_VERSION,
        variables,
        &fallback,
    )
    .await;
    let scoped = router.with_scope_context(Some(magicllm::LlmScope::new(
        scope.principal(),
        scope.workspace(),
    )));
    let operation = crate::magician_v2::query_analysis::operation_llm_router::LLMOperation::Other(
        CREW_PLAN_OPERATION.to_string(),
    );
    let response = scoped
        .generate_for_operation_with_system(&operation, None, &prompt)
        .await
        .map_err(|error| format!("the planner failed: {error}"))?;
    let mut lanes = parse_crew_plan(&response.content)?;
    lanes.truncate(max_lanes);
    normalize_crew_lanes(lanes)
}

// ─── Store ──────────────────────────────────────────────────────────────────

fn scope_root(service: &ArtifactV2Service, scope: &ScopeRef) -> std::path::PathBuf {
    service
        .workspace()
        .scope_root(&scope.principal(), &scope.workspace())
}

/// The project and crew, as stored.
pub fn read_crew(
    scope_root: &Path,
    project_id: &str,
    crew_id: &str,
) -> Option<(VibeDevProjectRecord, VibeDevCrew)> {
    let project = read_vibedev_projects(scope_root)
        .into_iter()
        .find(|project| project.project_id == project_id)?;
    let crew = project
        .crews
        .iter()
        .find(|crew| crew.crew_id == crew_id)?
        .clone();
    Some((project, crew))
}

async fn insert_crew(scope_root: &Path, project_id: &str, crew: VibeDevCrew) -> Result<(), String> {
    mutate_project_store_at(&vibedev_project_store_path(scope_root), |store| {
        let project = store
            .projects
            .iter_mut()
            .find(|project| project.project_id == project_id)
            .ok_or_else(|| format!("no VibeDev project {project_id} in this scope"))?;
        project.crews.push(crew);
        project.updated_at_ms = chrono::Utc::now().timestamp_millis();
        Ok(ProjectStoreEdit::Write)
    })
    .await
}

/// Change one crew. `edit` returns whether it changed anything; the crew's
/// `updated_at_ms` moves only when it did. Returns the crew as stored after.
async fn update_crew(
    scope_root: &Path,
    project_id: &str,
    crew_id: &str,
    edit: impl FnOnce(&mut VibeDevCrew) -> Result<bool, String>,
) -> Result<VibeDevCrew, String> {
    let mut after = None;
    mutate_project_store_at(&vibedev_project_store_path(scope_root), |store| {
        let crew = store
            .projects
            .iter_mut()
            .find(|project| project.project_id == project_id)
            .ok_or_else(|| format!("no VibeDev project {project_id} in this scope"))?
            .crews
            .iter_mut()
            .find(|crew| crew.crew_id == crew_id)
            .ok_or_else(|| format!("no crew {crew_id} in project {project_id}"))?;
        let changed = edit(crew)?;
        if changed {
            crew.updated_at_ms = chrono::Utc::now().timestamp_millis();
        }
        after = Some(crew.clone());
        Ok(if changed {
            ProjectStoreEdit::Write
        } else {
            ProjectStoreEdit::Skip
        })
    })
    .await?;
    after.ok_or_else(|| "the crew was not read back".to_string())
}

// ─── Owner questions ────────────────────────────────────────────────────────

/// The start question's id: one per crew, so asking twice is one question.
pub fn crew_start_request_id(crew_id: &str) -> String {
    CrewQuestion::Start.request_id(crew_id)
}

/// The pull-request question's id: one per crew.
pub fn crew_pull_request_request_id(crew_id: &str) -> String {
    CrewQuestion::PullRequest.request_id(crew_id)
}

/// The conflict question's id: one per conflict. A merge can conflict again
/// after an integrator or the owner tried, and a reused id would be a replay
/// of the old question: no new question for the owner, and the old answer
/// read back from history as if it were new.
pub fn crew_resolve_request_id(crew_id: &str, occurrence: i64) -> String {
    format!("{}-{occurrence}", CrewQuestion::Resolve.request_id(crew_id))
}

/// The question that asks the owner to start a proposed crew.
pub fn crew_start_request(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let mut lanes = String::new();
    for lane in &crew.lanes {
        let brief = one_line(&lane.brief);
        let brief: String = brief.chars().take(240).collect();
        lanes.push_str(&format!("- **{}**: {brief}", lane.lane));
        if !lane.owns.is_empty() {
            lanes.push_str(&format!(" _(owns {})_", lane.owns.join(", ")));
        }
        lanes.push('\n');
    }
    let budget = crew
        .budget_usd
        .map(|usd| format!("\nBudget for all lanes: ${usd:.2}.\n"))
        .unwrap_or_default();
    UserRequest {
        id: crew_start_request_id(&crew.crew_id),
        request_type: CREW_START_REQUEST_TYPE.to_string(),
        question: format!(
            "**Start a VibeDev crew on {}?**\n\n{}\n\n{} lanes, each its own run and branch:\n\n{lanes}{budget}",
            project.name,
            crew.goal,
            crew.lanes.len()
        ),
        options: vec![
            RequestOption {
                id: START.to_string(),
                label: "Start the crew".to_string(),
                requires_input: false,
            },
            RequestOption {
                id: CANCEL.to_string(),
                label: "Don't start".to_string(),
                requires_input: false,
            },
        ],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "project_id": project.project_id,
            "crew_id": crew.crew_id,
        }),
        source: "vibedev".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: START_TIMEOUT_SECS,
        default_on_timeout: CANCEL.to_string(),
        created_at: 0,
        sensitive: None,
    }
}

fn crew_settled_notice(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let (title, severity) = match crew.status {
        CrewStatus::Ready | CrewStatus::Integrated => ("VibeDev crew finished", "info"),
        CrewStatus::Conflict => ("VibeDev crew lanes conflict", "warning"),
        CrewStatus::Cancelled => ("VibeDev crew stopped", "info"),
        _ => ("VibeDev crew ended with failed lanes", "warning"),
    };
    let goal: String = one_line(&crew.goal).chars().take(200).collect();
    let mut message = format!("{} · {goal}\n{}", project.name, lanes_summary(&crew.lanes));
    if let Some(note) = crew
        .note
        .as_deref()
        .filter(|note| note.starts_with("Stopped at"))
    {
        message.push_str(&format!("\n{note}"));
    }
    if let Some(conflict) = crew.conflict.as_ref() {
        message.push_str(&format!(
            "\nLane {} conflicts with the lanes merged before it in: {}. Resolve it on its branch, then retry the merge.",
            conflict.lane,
            conflict.files.join(", ")
        ));
    }
    for lane in &crew.lanes {
        message.push_str(&format!("\n- {}: {:?}", lane.lane, lane.status).to_lowercase());
    }
    UserRequest {
        id: format!("vibedev-crew-{}-settled", crew.crew_id),
        request_type: "harness.notify_owner.briefing".to_string(),
        question: format!("Owner briefing: {title}\n\n{message}"),
        options: vec![RequestOption {
            id: "acknowledge".to_string(),
            label: "Acknowledge".to_string(),
            requires_input: false,
        }],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "owner_agent_id": "vibedev",
            "title": title,
            "message": message,
            "kind": "briefing",
            "severity": severity,
            "source": "vibedev_crew_settled",
            "project_id": project.project_id,
            "crew_id": crew.crew_id,
        }),
        source: "harness".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: 86_400,
        default_on_timeout: "acknowledge".to_string(),
        created_at: 0,
        sensitive: None,
    }
}

/// The notice that a finished crew's merge keeps failing: retries go on, more
/// slowly, and the owner can fix the cause and merge again. One per crew.
fn crew_merge_stuck_notice(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    error: &str,
    failures: u32,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let title = "VibeDev crew cannot merge its lanes";
    let goal: String = one_line(&crew.goal).chars().take(200).collect();
    let error: String = one_line(error).chars().take(400).collect();
    let message = format!(
        "{} · {goal}\nEvery lane finished, but merging their branches failed {failures} times in \
         a row: {error}\nIt is retried less often now. Fix the cause, then merge again from the \
         crew in VibeDev.",
        project.name
    );
    UserRequest {
        id: format!("vibedev-crew-{}-merge-stuck", crew.crew_id),
        request_type: "harness.notify_owner.briefing".to_string(),
        question: format!("Owner briefing: {title}\n\n{message}"),
        options: vec![RequestOption {
            id: "acknowledge".to_string(),
            label: "Acknowledge".to_string(),
            requires_input: false,
        }],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "owner_agent_id": "vibedev",
            "title": title,
            "message": message,
            "kind": "briefing",
            "severity": "warning",
            "source": "vibedev_crew_merge_stuck",
            "project_id": project.project_id,
            "crew_id": crew.crew_id,
        }),
        source: "harness".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: 86_400,
        default_on_timeout: "acknowledge".to_string(),
        created_at: 0,
        sensitive: None,
    }
}

/// The owner questions a crew asks. Start, pull request and choose are asked
/// once per crew and have one id each; resolve can be asked again, so its ids
/// carry the occurrence ([`crew_resolve_request_id`]). A crew acts only on an
/// answer to the id it currently waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrewQuestion {
    Start,
    PullRequest,
    /// A race finished: which lane to keep.
    Choose,
    /// The merge conflicted: let an agent resolve it?
    Resolve,
}

impl CrewQuestion {
    fn suffix(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::PullRequest => "pr",
            Self::Choose => "choose",
            Self::Resolve => "resolve",
        }
    }

    pub fn request_type(self) -> &'static str {
        match self {
            Self::Start => CREW_START_REQUEST_TYPE,
            Self::PullRequest => CREW_PULL_REQUEST_TYPE,
            Self::Choose => "vibedev.crew_choose",
            Self::Resolve => "vibedev.crew_resolve",
        }
    }

    pub fn request_id(self, crew_id: &str) -> String {
        format!("vibedev-crew-{crew_id}-{}", self.suffix())
    }

    /// The crew and question a request id names, with or without an
    /// occurrence (`vibedev-crew-<crew>-resolve-<n>`).
    pub fn parse(request_id: &str) -> Option<(&str, Self)> {
        let rest = request_id.strip_prefix("vibedev-crew-")?;
        let base = match rest.rsplit_once('-') {
            Some((head, occurrence))
                if !occurrence.is_empty() && occurrence.bytes().all(|b| b.is_ascii_digit()) =>
            {
                head
            },
            _ => rest,
        };
        for question in [Self::Start, Self::PullRequest, Self::Choose, Self::Resolve] {
            if let Some(crew_id) = base.strip_suffix(&format!("-{}", question.suffix())) {
                if !crew_id.is_empty() {
                    return Some((crew_id, question));
                }
            }
        }
        None
    }
}

const RESOLVE: &str = "resolve";
const MANUAL: &str = "manual";
const KEEP_NONE: &str = "none";

/// The race's question: which finished attempt to keep.
pub fn crew_choose_question(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let goal: String = one_line(&crew.goal).chars().take(200).collect();
    let mut options: Vec<RequestOption> = work_lanes(crew)
        .filter(|(_, lane)| lane.status == LaneStatus::Done)
        .map(|(index, lane)| RequestOption {
            id: format!("lane:{index}"),
            label: format!("Keep {}", lane.lane),
            requires_input: false,
        })
        .collect();
    options.push(RequestOption {
        id: KEEP_NONE.to_string(),
        label: "Keep none".to_string(),
        requires_input: false,
    });
    UserRequest {
        id: CrewQuestion::Choose.request_id(&crew.crew_id),
        request_type: CrewQuestion::Choose.request_type().to_string(),
        question: format!(
            "**VibeDev race on {} finished: which attempt do you keep?**\n\n{goal}\n\n{}\n\nOpen \
             each attempt's run to compare. The one you keep is merged; the others stay on their \
             branches.",
            project.name,
            lanes_summary(&crew.lanes)
        ),
        options,
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({ "project_id": project.project_id, "crew_id": crew.crew_id }),
        source: "vibedev".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: START_TIMEOUT_SECS,
        default_on_timeout: KEEP_NONE.to_string(),
        created_at: 0,
        sensitive: None,
    }
}

/// The conflict question: let an integrator lane resolve it, or do it by hand.
pub fn crew_resolve_question(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let goal: String = one_line(&crew.goal).chars().take(200).collect();
    let detail = crew
        .conflict
        .as_ref()
        .map(|conflict| {
            format!(
                "Lane {} conflicts with the lanes merged before it in: {}.",
                conflict.lane,
                conflict.files.join(", ")
            )
        })
        .unwrap_or_default();
    let mut options = Vec::new();
    let advice = if !can_add_integrator(crew) {
        format!(
            "Integrator runs tried {} times. Resolve it on the lane branches yourself and merge \
             again.",
            integrator_attempts(crew)
        )
    } else if !crew_budget_left(crew) {
        let budget = crew.budget_usd.unwrap_or_default();
        format!(
            "The crew's ${budget:.2} budget is spent, so no integrator run can start. Resolve it \
             on the lane branches yourself and merge again."
        )
    } else {
        options.push(RequestOption {
            id: RESOLVE.to_string(),
            label: "Let an agent resolve it".to_string(),
            requires_input: false,
        });
        "An integrator run can merge every lane and resolve the conflict, or you can resolve it \
         on the lane branches yourself and merge again."
            .to_string()
    };
    options.push(RequestOption {
        id: MANUAL.to_string(),
        label: "I'll resolve it".to_string(),
        requires_input: false,
    });
    UserRequest {
        // The id the crew stored when it entered the conflict; a bare id only
        // for a crew stored before ids carried their occurrence.
        id: crew
            .resolve_request_id
            .clone()
            .unwrap_or_else(|| CrewQuestion::Resolve.request_id(&crew.crew_id)),
        request_type: CrewQuestion::Resolve.request_type().to_string(),
        question: format!(
            "**VibeDev crew on {}: the lanes conflict.**\n\n{goal}\n\n{detail}\n\n{advice}",
            project.name
        ),
        options,
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({ "project_id": project.project_id, "crew_id": crew.crew_id }),
        source: "vibedev".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: START_TIMEOUT_SECS,
        default_on_timeout: MANUAL.to_string(),
        created_at: 0,
        sensitive: None,
    }
}

/// The question offering a pull request for a crew's integration branch.
///
/// An answer here only records what the owner wants: opening the request
/// pushes with the owner's own credentials, and Attention can be answered by
/// surfaces that are not the owner (a bot, a plane grant), so the push itself
/// is the owner-only `pull-request` action on the crew.
pub fn crew_pull_request_question(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    principal: &str,
    workspace: &str,
) -> UserRequest {
    let goal: String = one_line(&crew.goal).chars().take(200).collect();
    let branch = crew.branch.clone().unwrap_or_default();
    let spent = crew
        .spent_usd
        .map(|usd| format!(" Spent ${usd:.2}."))
        .unwrap_or_default();
    UserRequest {
        id: crew_pull_request_request_id(&crew.crew_id),
        request_type: CREW_PULL_REQUEST_TYPE.to_string(),
        question: format!(
            "**VibeDev crew finished on {}: open a pull request?**\n\n{goal}\n\nAll {} lanes \
             are merged into `{branch}`.{spent} Review it there. A pull request pushes with your \
             own credentials, so you open it from the crew in VibeDev.",
            project.name,
            crew.lanes.len()
        ),
        options: vec![
            RequestOption {
                id: OPEN_PULL_REQUEST.to_string(),
                label: "I'll open it in VibeDev".to_string(),
                requires_input: false,
            },
            RequestOption {
                id: LATER.to_string(),
                label: "Not now".to_string(),
                requires_input: false,
            },
        ],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "project_id": project.project_id,
            "crew_id": crew.crew_id,
            "branch": branch,
        }),
        source: "vibedev".to_string(),
        execution_id: None,
        task_id: None,
        timeout_secs: START_TIMEOUT_SECS,
        default_on_timeout: LATER.to_string(),
        created_at: 0,
        sensitive: None,
    }
}

// ─── Lifecycle ──────────────────────────────────────────────────────────────

/// Answer a crew question the owner settled some other way (a button in the
/// cockpit or the phone, or stopping the crew), so Attention stops asking.
/// Only a question still pending is answered; the resolution's event reaches
/// the supervisor, which finds the crew already moved on and does nothing.
async fn settle_question(
    resources: &AgentResources,
    scope: &ScopeRef,
    request_id: Option<&str>,
    decision: &str,
) {
    let (Some(requests), Some(request_id)) = (resources.user_request_service.as_ref(), request_id)
    else {
        return;
    };
    let pending = requests
        .list_pending_for_scope(&scope.principal(), &scope.workspace())
        .await
        .into_iter()
        .any(|request| request.id == request_id);
    if !pending {
        return;
    }
    let response = crate::magician_v2::user_requests::UserResponse {
        request_id: request_id.to_string(),
        decision: decision.to_string(),
        input: None,
        channel: "vibedev".to_string(),
        sensitive: Vec::new(),
    };
    let outcome = requests
        .respond_scoped(response, Some(&scope.principal()), Some(&scope.workspace()))
        .await;
    if !matches!(
        outcome,
        crate::magician_v2::user_requests::ScopedResponseResult::Accepted
    ) {
        tracing::debug!(%request_id, "a settled crew question was not resolved");
    }
}

/// What a caller asks for when proposing a crew.
#[derive(Debug, Clone, Default)]
pub struct ProposeCrew {
    pub goal: String,
    /// `None`: the planner splits the goal.
    pub lanes: Option<Vec<CrewLanePlan>>,
    pub budget_usd: Option<f64>,
    /// Most lanes the planner may use.
    pub max_lanes: Option<usize>,
    /// Start at once instead of asking the owner.
    pub start: bool,
    pub mode: CrewMode,
    /// Race: how many attempts when `lanes` is not given (default 3).
    pub racers: Option<usize>,
    /// Race: the coding profiles to race, one lane each.
    pub profiles: Option<Vec<String>>,
    /// Review the merged branch before the pull-request question.
    pub review: bool,
}

/// Why a crew call failed: the caller's input, or the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrewError {
    Invalid(String),
    NotFound(String),
    Failed(String),
}

impl std::fmt::Display for CrewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::NotFound(message) | Self::Failed(message) => {
                f.write_str(message)
            },
        }
    }
}

fn service_of(resources: &AgentResources) -> Result<Arc<ArtifactV2Service>, CrewError> {
    resources
        .artifact_v2_service
        .clone()
        .ok_or_else(|| CrewError::Failed("the task service is not available".to_string()))
}

/// Propose a crew on a trusted project: plan it when no lanes were given,
/// store it, then start it or ask the owner to.
pub async fn propose_crew(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    request: ProposeCrew,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let project = read_vibedev_projects(&root)
        .into_iter()
        .find(|project| project.project_id == project_id && !project.archived)
        .ok_or_else(|| CrewError::NotFound(format!("no VibeDev project {project_id}")))?;
    if project.trust != VibeDevProjectTrust::Trusted {
        return Err(CrewError::Invalid(
            "a crew needs a trusted project: its lanes run at once on their own branches, and a \
             sandboxed project runs one coding run at a time"
                .to_string(),
        ));
    }
    let goal = normalize_crew_goal(&request.goal).map_err(CrewError::Invalid)?;
    let budget_usd = normalize_crew_budget(request.budget_usd).map_err(CrewError::Invalid)?;
    let lanes = match (request.lanes, request.mode) {
        (Some(lanes), _) => normalize_crew_lanes(lanes).map_err(CrewError::Invalid)?,
        (None, CrewMode::Race) => {
            let config = resources.magician_config_snapshot();
            let catalog = VibeDevCodingCatalog::from_config(&config);
            race_lanes(&goal, request.profiles, request.racers, &catalog)
                .map_err(CrewError::Invalid)?
        },
        (None, CrewMode::Split) => plan_crew_lanes(
            resources,
            scope,
            &project,
            &goal,
            request.max_lanes.unwrap_or(4),
        )
        .await
        .map_err(CrewError::Failed)?,
    };
    let now = chrono::Utc::now().timestamp_millis();
    let crew = VibeDevCrew {
        crew_id: format!("crew_{}", uuid::Uuid::new_v4().simple()),
        goal,
        status: CrewStatus::Proposed,
        mode: request.mode,
        lanes,
        review: request.review,
        winner: None,
        choose_request_id: None,
        resolve_request_id: None,
        budget_usd,
        spent_usd: None,
        start_request_id: None,
        branch: None,
        conflict: None,
        pull_request_request_id: None,
        pull_request_url: None,
        note: Some("Planned".to_string()),
        created_at_ms: now,
        updated_at_ms: now,
    };
    insert_crew(&root, project_id, crew.clone())
        .await
        .map_err(CrewError::Failed)?;
    if request.start {
        return start_crew(resources, scope, project_id, &crew.crew_id).await;
    }
    let Some(requests) = resources.user_request_service.as_ref() else {
        return Ok(crew);
    };
    let question = crew_start_request(&project, &crew, &scope.principal(), &scope.workspace());
    let request_id = question.id.clone();
    if let Err(error) = requests.submit_nonblocking(question).await {
        tracing::warn!(crew_id = %crew.crew_id, %error, "could not ask the owner to start a crew");
        return Ok(crew);
    }
    update_crew(&root, project_id, &crew.crew_id, |crew| {
        crew.start_request_id = Some(request_id);
        crew.note = Some("Waiting for you to start it".to_string());
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)
}

/// A status's wire name, for messages.
fn status_name(status: CrewStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Start every lane that has no run yet. Idempotent: each lane's run is keyed
/// by the crew and the lane's position, so a repeat replays the same run.
pub async fn start_crew(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let (_project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    let question = crew.start_request_id.clone();
    // Decided under the store's lock, so a stop that landed first stays.
    let mut refused = None;
    update_crew(&root, project_id, crew_id, |crew| match crew.status {
        CrewStatus::Proposed => {
            crew.status = CrewStatus::Running;
            crew.start_request_id = None;
            crew.note = Some("Starting lanes".to_string());
            Ok(true)
        },
        CrewStatus::Running => Ok(false),
        status => {
            refused = Some(status);
            Ok(false)
        },
    })
    .await
    .map_err(CrewError::Failed)?;
    if let Some(status) = refused {
        return Err(CrewError::Invalid(format!(
            "the crew is {} and cannot be started",
            status_name(status)
        )));
    }
    let started = start_pending_lanes(resources, scope, project_id, crew_id).await?;
    settle_question(resources, scope, question.as_deref(), START).await;
    Ok(started)
}

/// Start every lane of a running crew that has no run yet. Used for the
/// first start, for lanes added later (integrator, reviewer), and by the
/// supervisor for lanes an interrupted start left without a run.
///
/// One starter per crew at a time, and the crew is re-read before each lane:
/// once it is no longer running (stopped), no further lane starts, and a lane
/// admitted while the stop landed is recorded as stopped and its run stopped.
/// With a crew budget, each lane's run is capped at its share of what the
/// budget has left ([`share_lane_budget`]), so its spend is recorded, its
/// engine stops at the cap, and the lanes together stay within the budget.
/// Lanes the share cannot cover a cent for stay waiting until running lanes
/// end.
async fn start_pending_lanes(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    use super::run_service::VibeDevRunStartError;
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let lock = crate::magician_v2::execution::coding_engine::shadow_admission_lock(&format!(
        "crew-lanes:{crew_id}"
    ))
    .await;
    let _guard = lock.lock_owned().await;
    let (project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    if crew.status != CrewStatus::Running {
        return Err(CrewError::Invalid(format!(
            "the crew is {}; its lanes are not started",
            status_name(crew.status)
        )));
    }
    let mut pending: Vec<usize> = crew
        .lanes
        .iter()
        .enumerate()
        .filter(|(_, lane)| lane.run_task_id.is_none() && lane.status == LaneStatus::Pending)
        .map(|(index, _)| index)
        .collect();
    if pending.is_empty() {
        return Ok(crew);
    }
    // Lanes with a run that has not ended may still spend up to their own
    // cap, so they hold a share too.
    let running = crew
        .lanes
        .iter()
        .filter(|lane| lane.run_task_id.is_some() && !lane.status.is_ended())
        .count();
    let share = share_lane_budget(
        crew.budget_usd,
        lanes_spent_usd(&service, scope, &crew),
        pending.len(),
        running,
    );
    let (lane_budget_usd, waiting_for_budget) = match share {
        Ok(share) => {
            let waiting = pending.split_off(share.starting.min(pending.len()));
            (share.per_lane_usd, waiting.len())
        },
        Err(note) => {
            // Nothing starts past the budget: the waiting lanes end as
            // stopped, and the next pass settles the crew.
            return update_crew(&root, project_id, crew_id, |crew| {
                if crew.status != CrewStatus::Running {
                    return Ok(false);
                }
                for index in &pending {
                    if let Some(lane) = crew
                        .lanes
                        .get_mut(*index)
                        .filter(|lane| lane.run_task_id.is_none())
                    {
                        lane.status = LaneStatus::Stopped;
                    }
                }
                crew.note = Some(format!("Not started: {note}"));
                Ok(true)
            })
            .await
            .map_err(CrewError::Failed);
        },
    };
    let context = lane_context(&root, &crew);

    let config = resources.magician_config_snapshot();
    let catalog = VibeDevCodingCatalog::from_config(&config);
    let owner_agent_id =
        crate::magician_v2::agents::task_ownership::resolve_created_task_owner_agent_id(
            resources,
            VIBEDEV_THREAD_ID,
            &vibedev_run_task_tags(false, false),
            None,
        );
    let mut failures: Vec<usize> = Vec::new();
    for index in pending {
        let still_running = matches!(
            read_crew(&root, project_id, crew_id),
            Some((_, now)) if now.status == CrewStatus::Running
        );
        if !still_running {
            break;
        }
        let lane = &crew.lanes[index];
        let input = StartVibeDevBuild {
            scope: scope.clone(),
            chat_session_id: project.chat_session_id.clone(),
            chat_turn_id: format!("{crew_id}-lane-{index}"),
            owner_agent_id: owner_agent_id.clone(),
            project: project.clone(),
            request: lane_request(&crew, index, &context),
            mode: DispatchMode::Build,
            coding_choice: lane
                .coding_profile_id
                .clone()
                .map(|profile_id| VibeDevCodingChoice::Profile { profile_id }),
            coding_catalog: catalog.clone(),
            cockpit: Some(VibeDevCockpitRun {
                created_by: CREW_LANE_CREATED_BY.to_string(),
                // A crew's lanes do not move what the cockpit shows.
                pin_project_pointer: false,
                crew_id: Some(crew_id.to_string()),
                lane: Some(lane.lane.clone()),
                cost_budget_usd: lane_budget_usd,
                ..VibeDevCockpitRun::default()
            }),
        };
        let dispatch_service = Arc::clone(&service);
        let dispatch_scope = scope.clone();
        let started = VibeDevRunService::new(Arc::clone(&service))
            .start_build(input, move |task_id| async move {
                dispatch_service
                    .start_execution(dispatch_scope, task_id, None, false)
                    .await
                    .map(|(_task, execution)| execution.state.execution_id)
                    .map_err(|error| error.to_string())
            })
            .await;
        let task_id = match started {
            Ok(admission) => admission.task_id().to_string(),
            // Admitted; recovery starts it on the next process start.
            Err(VibeDevRunStartError::AdmittedNotStarted { task_id }) => task_id,
            // The lane's key already names a run: an earlier start of this
            // lane that was interrupted before it was recorded.
            Err(VibeDevRunStartError::Conflict { existing_task_id }) => existing_task_id,
            Err(error) => {
                tracing::warn!(%crew_id, lane = %lane.lane, ?error, "a crew lane did not start");
                failures.push(index);
                continue;
            },
        };
        let mut orphaned = false;
        update_crew(&root, project_id, crew_id, |crew| {
            let running = crew.status == CrewStatus::Running;
            let Some(stored) = crew.lanes.get_mut(index) else {
                return Ok(false);
            };
            stored.run_task_id = Some(task_id.clone());
            if running {
                stored.status = LaneStatus::Running;
            } else {
                // Stopped while this lane was admitted; the stop did not see
                // its run.
                stored.status = LaneStatus::Stopped;
                orphaned = true;
            }
            Ok(true)
        })
        .await
        .map_err(CrewError::Failed)?;
        if orphaned {
            stop_task(&service, scope, &task_id).await;
            break;
        }
    }
    update_crew(&root, project_id, crew_id, |crew| {
        let mut changed = false;
        for index in &failures {
            if let Some(lane) = crew
                .lanes
                .get_mut(*index)
                .filter(|lane| lane.run_task_id.is_none())
            {
                lane.status = LaneStatus::Failed;
                changed = true;
            }
        }
        if crew.status != CrewStatus::Running {
            return Ok(changed);
        }
        let failed: Vec<String> = failures
            .iter()
            .filter_map(|index| crew.lanes.get(*index))
            .map(|lane| lane.lane.clone())
            .collect();
        crew.note = Some(match (failed.is_empty(), waiting_for_budget) {
            (true, 0) => lanes_summary(&crew.lanes),
            (true, waiting) => format!(
                "{}; {waiting} wait for running lanes to end, as the budget left covers no more \
                 at once",
                lanes_summary(&crew.lanes)
            ),
            (false, _) => format!("Could not start: {}", failed.join(", ")),
        });
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)
}

/// Stop every running lane and cancel the crew. A proposed crew is simply
/// cancelled.
pub async fn stop_crew(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    if read_crew(&root, project_id, crew_id).is_none() {
        return Err(CrewError::NotFound(format!(
            "no crew {crew_id} in project {project_id}"
        )));
    }
    // Cancelled first, under the store's lock, and the runs stopped after: a
    // lane being admitted meanwhile finds the crew cancelled and stops its own
    // run, and a merge or an answer in flight finds it cancelled and stays out.
    let mut before = None;
    let stopped = update_crew(&root, project_id, crew_id, |crew| {
        if crew.status.is_settled() {
            return Ok(false);
        }
        before = Some(crew.clone());
        for lane in crew.lanes.iter_mut() {
            if !lane.status.is_ended() {
                lane.status = LaneStatus::Stopped;
            }
        }
        crew.status = CrewStatus::Cancelled;
        crew.start_request_id = None;
        crew.choose_request_id = None;
        crew.resolve_request_id = None;
        crew.note = Some("Stopped".to_string());
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)?;
    let Some(before) = before else {
        return Ok(stopped);
    };
    let items = service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &stopped.lane_task_ids(),
            crate::magician_v2::archived_filter::ArchivedFilter::Include,
        )
        .await
        .unwrap_or_default();
    for item in items
        .iter()
        .filter(|item| !LaneStatus::from_task_status(&item.status).is_ended())
    {
        stop_run(&service, scope, item).await;
    }
    for (request_id, decision) in [
        (before.start_request_id.as_deref(), CANCEL),
        (before.choose_request_id.as_deref(), KEEP_NONE),
        (before.resolve_request_id.as_deref(), MANUAL),
    ] {
        settle_question(resources, scope, request_id, decision).await;
    }
    Ok(stopped)
}

/// Stop one lane's run by its task id, when it is still going.
async fn stop_task(service: &Arc<ArtifactV2Service>, scope: &ScopeRef, task_id: &str) {
    let items = service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &[task_id.to_string()],
            crate::magician_v2::archived_filter::ArchivedFilter::Include,
        )
        .await
        .unwrap_or_default();
    for item in items
        .iter()
        .filter(|item| !LaneStatus::from_task_status(&item.status).is_ended())
    {
        stop_run(service, scope, item).await;
    }
}

async fn stop_run(service: &Arc<ArtifactV2Service>, scope: &ScopeRef, item: &TaskListItemV3) {
    use crate::magician_v2::execution::coding_engine::{
        coding_control_registry, scoped_control_key, CodingControlAction,
    };
    let key = scoped_control_key(&scope.principal(), &scope.workspace(), &item.id);
    let delivered = coding_control_registry()
        .control(&key, CodingControlAction::Stop, None)
        .await
        .unwrap_or(false);
    if delivered {
        return;
    }
    let Some(execution_id) = item
        .active_root_execution_id
        .clone()
        .or_else(|| item.latest_root_execution_id.clone())
    else {
        return;
    };
    if let Err(error) = service.cancel_execution_by_id(scope, &execution_id).await {
        tracing::warn!(task_id = %item.id, %error, "could not stop a crew lane");
    }
}

/// What the lanes have spent together, in USD, from their coding cost ledgers.
fn lanes_spent_usd(service: &ArtifactV2Service, scope: &ScopeRef, crew: &VibeDevCrew) -> f64 {
    use crate::magician_v2::execution::coding_engine::cost_budget::CodingCostLedger;
    let micro: u64 = crew
        .lane_task_ids()
        .iter()
        .map(|task_id| {
            let dir = service
                .workspace()
                .task_dir(&scope.principal(), &scope.workspace(), task_id);
            CodingCostLedger::open(&dir).spent_micro_usd()
        })
        .sum();
    micro as f64 / 1_000_000.0
}

/// What a lane whose task has no directory is waiting for, from its run's
/// dispatch intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissingLaneTask {
    /// Never admitted, or its admission ended (dispatched or failed): the
    /// task was deleted, or never will exist.
    Gone,
    /// Admitted and not created yet: the start in flight, or the next
    /// restart's recovery, creates it.
    Waiting,
    /// Admitted, unclaimed, for longer than [`LANE_ADMISSION_GRACE_SECS`].
    /// Fail the intent first, so recovery never starts a run the crew has
    /// stopped watching.
    GiveUp,
}

fn missing_lane_task(
    intent: Option<(DispatchIntentState, chrono::DateTime<chrono::Utc>)>,
    now: chrono::DateTime<chrono::Utc>,
) -> MissingLaneTask {
    match intent {
        None => MissingLaneTask::Gone,
        Some((state, _)) if state.is_terminal() => MissingLaneTask::Gone,
        Some((DispatchIntentState::Pending, updated_at))
            if (now - updated_at).num_seconds() >= LANE_ADMISSION_GRACE_SECS =>
        {
            MissingLaneTask::GiveUp
        },
        Some(_) => MissingLaneTask::Waiting,
    }
}

/// Whether a lane task with no directory has ended for good. A run admitted
/// but not created yet (`AdmittedNotStarted`, or a replay of an admission not
/// yet dispatched) is not deleted: it is waited for, so the crew does not
/// settle while recovery later starts an unsupervised run. A read that fails
/// waits for the next pass too.
fn lane_task_is_gone(service: &Arc<ArtifactV2Service>, scope: &ScopeRef, task_id: &str) -> bool {
    let store = super::run_service::vibedev_run_dispatch_intent_store(service, scope);
    let intent = match store.find_by_task_id(task_id) {
        Ok(intent) => intent,
        Err(error) => {
            tracing::warn!(%task_id, %error, "could not read a crew lane's dispatch intent");
            return false;
        },
    };
    let found = intent
        .as_ref()
        .map(|intent| (intent.state, intent.updated_at));
    match missing_lane_task(found, chrono::Utc::now()) {
        MissingLaneTask::Gone => true,
        MissingLaneTask::Waiting => false,
        MissingLaneTask::GiveUp => {
            let Some(intent) = intent else {
                return true;
            };
            // Fenced on the intent's generation: a claim that landed since
            // the read wins, and the lane waits for it.
            match store.fail(
                &intent,
                "the crew stopped waiting for this lane's run to be created",
            ) {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(%task_id, %error, "could not close a crew lane's stale admission");
                    false
                },
            }
        },
    }
}

async fn file(requests: Option<&UserRequestService>, request: UserRequest, crew_id: &str) {
    let Some(requests) = requests else {
        return;
    };
    if let Err(error) = requests.submit_nonblocking(request).await {
        tracing::warn!(%crew_id, %error, "could not file a crew question for the owner");
    }
}

/// The context lane requests need: work-lane branches and the crew branch.
fn lane_context(scope_root: &Path, crew: &VibeDevCrew) -> LaneContext {
    use super::trusted_workspace::load_run_workspace;
    let branches = work_lanes(crew)
        .filter_map(|(_, lane)| {
            let workspace = load_run_workspace(scope_root, lane.run_task_id.as_deref()?)?;
            Some((lane.lane.clone(), workspace.branch))
        })
        .collect();
    let chain = load_run_workspace(scope_root, &crew.crew_id);
    LaneContext {
        branches,
        crew_branch: crew
            .branch
            .clone()
            .or_else(|| chain.as_ref().map(|ws| ws.branch.clone())),
        base: chain.map(|ws| ws.base_sha),
    }
}

/// Re-read a crew's lanes and move it on: record spend and stop the lanes at
/// the crew's budget; start lanes an interrupted start left without a run;
/// once every lane has ended, ask the owner to pick a race winner, merge a
/// finished crew's branches into one branch, start the review lane when one
/// was asked for, and then offer a pull request. A conflict asks whether an
/// integrator lane should resolve it. The owner hears once per step.
pub async fn reconcile_crew(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, String> {
    let service = resources
        .artifact_v2_service
        .clone()
        .ok_or_else(|| "the task service is not available".to_string())?;
    let requests = resources.user_request_service.as_deref();
    let root = scope_root(&service, scope);
    let (project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| format!("no crew {crew_id} in project {project_id}"))?;
    let crew = match crew.status {
        CrewStatus::Running => {
            refresh_running_crew(resources, &service, requests, scope, &project, crew).await?
        },
        _ => crew,
    };
    if crew.status != CrewStatus::Ready {
        return Ok(crew);
    }
    // A merge that keeps failing is retried less and less often.
    if merge_backing_off(&crew.crew_id) {
        return Ok(crew);
    }
    integrate_crew(resources, &service, requests, scope, &project, &crew).await
}

async fn refresh_running_crew(
    resources: &AgentResources,
    service: &Arc<ArtifactV2Service>,
    requests: Option<&UserRequestService>,
    scope: &ScopeRef,
    project: &VibeDevProjectRecord,
    crew: VibeDevCrew,
) -> Result<VibeDevCrew, String> {
    let root = scope_root(service, scope);
    let queried = crew.lane_task_ids();
    let items = service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &queried,
            crate::magician_v2::archived_filter::ArchivedFilter::Include,
        )
        .await
        .map_err(|error| error.to_string())?;
    // A lane whose task is gone (deleted) has ended. One that is only missing
    // from this listing (a read that failed), or whose run is admitted but
    // not created yet, waits for the next pass.
    let (principal, workspace) = (scope.principal(), scope.workspace());
    let gone: Vec<String> = queried
        .iter()
        .filter(|task_id| !items.iter().any(|item| &item.id == *task_id))
        .filter(|task_id| {
            !service
                .workspace()
                .task_dir(&principal, &workspace, task_id)
                .exists()
        })
        .filter(|task_id| lane_task_is_gone(service, scope, task_id))
        .cloned()
        .collect();
    let spent = lanes_spent_usd(service, scope, &crew);
    let over_budget = crew.budget_usd.is_some_and(|budget| spent >= budget);
    if over_budget {
        for item in items
            .iter()
            .filter(|item| !LaneStatus::from_task_status(&item.status).is_ended())
        {
            stop_run(service, scope, item).await;
        }
    }
    let mut entered = None;
    let mut unstarted = false;
    let after = update_crew(&root, &project.project_id, &crew.crew_id, |crew| {
        // Stopped since it was read: the stop decided, and nothing here may
        // move it back.
        if crew.status != CrewStatus::Running {
            return Ok(false);
        }
        let mut changed = crew.spent_usd != Some(spent);
        crew.spent_usd = Some(spent);
        for lane in crew.lanes.iter_mut() {
            let Some(task_id) = lane.run_task_id.as_deref() else {
                continue;
            };
            let mut status = match items.iter().find(|item| item.id == task_id) {
                Some(item) => LaneStatus::from_task_status(&item.status),
                None if gone.iter().any(|gone| gone == task_id) => LaneStatus::Stopped,
                None => continue,
            };
            if over_budget && !status.is_ended() {
                status = LaneStatus::Stopped;
            }
            if lane.status != status {
                lane.status = status;
                changed = true;
            }
        }
        unstarted = crew
            .lanes
            .iter()
            .any(|lane| lane.run_task_id.is_none() && !lane.status.is_ended());
        if !unstarted {
            let status = crew_status_after_lanes(crew);
            if status != crew.status {
                entered = Some(status);
                crew.status = status;
                changed = true;
                match status {
                    CrewStatus::Choosing => {
                        crew.choose_request_id =
                            Some(CrewQuestion::Choose.request_id(&crew.crew_id));
                    },
                    CrewStatus::Conflict => {
                        crew.resolve_request_id = Some(crew_resolve_request_id(
                            &crew.crew_id,
                            chrono::Utc::now().timestamp_millis(),
                        ));
                    },
                    _ => {},
                }
            }
        }
        if changed {
            crew.note = Some(match (over_budget, crew.budget_usd, crew.status) {
                (true, Some(budget), _) => format!("Stopped at the ${budget:.2} budget"),
                (_, _, CrewStatus::Choosing) => "Pick the attempt to keep".to_string(),
                (_, Some(budget), CrewStatus::Conflict) if !crew_budget_left(crew) => format!(
                    "The ${budget:.2} budget is spent, so no integrator can run; resolve the \
                     conflict by hand, then merge again"
                ),
                (_, _, CrewStatus::Conflict) if can_add_integrator(crew) => {
                    "The integrator did not finish; resolve the conflict by hand or try again"
                        .to_string()
                },
                (_, _, CrewStatus::Conflict) => format!(
                    "The integrator did not finish after {} tries; resolve the conflict by hand, \
                     then merge again",
                    integrator_attempts(crew)
                ),
                _ => lanes_summary(&crew.lanes),
            });
        }
        Ok(changed)
    })
    .await?;
    if unstarted && after.status == CrewStatus::Running {
        // An interrupted start (a restart mid-loop, a failed write) left lanes
        // without a run. Starting them again replays a run already admitted
        // under the lane's key.
        return start_pending_lanes(resources, scope, &project.project_id, &after.crew_id)
            .await
            .map_err(|error| error.to_string());
    }
    match entered {
        Some(CrewStatus::Choosing) => {
            let question = crew_choose_question(project, &after, &principal, &workspace);
            file(requests, question, &after.crew_id).await;
        },
        Some(CrewStatus::Conflict) => {
            let question = crew_resolve_question(project, &after, &principal, &workspace);
            file(requests, question, &after.crew_id).await;
        },
        Some(CrewStatus::Failed | CrewStatus::Cancelled) => {
            let notice = crew_settled_notice(project, &after, &principal, &workspace);
            file(requests, notice, &after.crew_id).await;
        },
        _ => {},
    }
    Ok(after)
}

/// The lane branches a ready crew merges, by label, in order: the review
/// lane's (when it finished), the race winner's, the integrator's (which
/// already holds every lane), or every work lane's.
fn branches_to_merge(
    scope_root: &Path,
    crew: &VibeDevCrew,
) -> Result<Vec<(String, super::trusted_workspace::TrustedRunWorkspace)>, String> {
    use super::trusted_workspace::load_run_workspace;
    let last = |role: LaneRole| crew.lanes.iter().rev().find(|lane| lane.role == role);
    let lanes: Vec<&CrewLane> = if let Some(review) = last(LaneRole::Review) {
        if review.status == LaneStatus::Done {
            vec![review]
        } else {
            Vec::new()
        }
    } else if crew.mode == CrewMode::Race {
        let winner = crew
            .winner
            .and_then(|index| crew.lanes.get(index))
            .ok_or_else(|| "the race has no winner yet".to_string())?;
        vec![winner]
    } else if let Some(integrator) =
        last(LaneRole::Integrate).filter(|lane| lane.status == LaneStatus::Done)
    {
        vec![integrator]
    } else {
        work_lanes(crew).map(|(_, lane)| lane).collect()
    };
    lanes
        .into_iter()
        .map(|lane| {
            let workspace = lane
                .run_task_id
                .as_deref()
                .and_then(|task_id| load_run_workspace(scope_root, task_id))
                .ok_or_else(|| format!("Lane {} has no branch to merge", lane.lane))?;
            Ok((lane.lane.clone(), workspace))
        })
        .collect()
}

/// Merge a ready crew's branches into its integration branch.
async fn integrate_crew(
    resources: &AgentResources,
    service: &Arc<ArtifactV2Service>,
    requests: Option<&UserRequestService>,
    scope: &ScopeRef,
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
) -> Result<VibeDevCrew, String> {
    use super::trusted_workspace::{
        integrate_branches, load_run_workspace, IntegrationOutcome, TrustedRunRequest,
    };
    let root = scope_root(service, scope);
    let lanes = match branches_to_merge(&root, crew) {
        Ok(lanes) => lanes,
        Err(note) => {
            return merge_failed(&root, requests, scope, project, crew, note).await;
        },
    };
    // Where the chain comes from: an existing crew chain (a merge after a
    // review), else the first lane's repo and base.
    let existing_chain = load_run_workspace(&root, &crew.crew_id);
    let (real_repo, base_branch) = match (existing_chain.as_ref(), lanes.first()) {
        (Some(chain), _) => (chain.real_repo.clone(), chain.base_branch.clone()),
        (None, Some((_, first))) => (first.real_repo.clone(), first.base_branch.clone()),
        (None, None) => return Ok(crew.clone()),
    };
    let branches: Vec<String> = lanes.iter().map(|(_, ws)| ws.branch.clone()).collect();
    let lock = crate::magician_v2::execution::coding_engine::shadow_admission_lock(&format!(
        "trusted:{}",
        crew.crew_id
    ))
    .await;
    let Ok(_guard) = lock.try_lock_owned() else {
        return Ok(crew.clone());
    };
    let (scope_root_owned, crew_id, project_id, project_name) = (
        root.clone(),
        crew.crew_id.clone(),
        project.project_id.clone(),
        project.name.clone(),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        integrate_branches(
            &TrustedRunRequest {
                scope_root: &scope_root_owned,
                root_task_id: &crew_id,
                project_id: &project_id,
                project_name: &project_name,
                real_repo: &real_repo,
                base_branch: base_branch.as_deref(),
            },
            &branches,
        )
    })
    .await
    .map_err(|error| format!("the merge task failed: {error}"))?;
    // Each outcome is recorded only while the crew is still `ready`: a crew
    // stopped during the merge stays stopped.
    if outcome.is_ok() {
        clear_merge_failures(&crew.crew_id);
    }
    match outcome {
        Ok(IntegrationOutcome::Merged { workspace, .. }) => {
            let wants_review =
                crew.review && !crew.lanes.iter().any(|lane| lane.role == LaneRole::Review);
            // The review is one more run; it needs budget left.
            let budget_spent = wants_review
                && remaining_lane_budget(crew.budget_usd, lanes_spent_usd(service, scope, crew))
                    .is_err();
            if wants_review && !budget_spent {
                let review_lane = CrewLane {
                    lane: "Review".to_string(),
                    brief: "Review the merged branch and fix what you find.".to_string(),
                    owns: Vec::new(),
                    coding_profile_id: None,
                    run_task_id: None,
                    status: LaneStatus::Pending,
                    role: LaneRole::Review,
                };
                let mut moved = false;
                let after = update_crew(&root, &project.project_id, &crew.crew_id, |crew| {
                    if crew.status != CrewStatus::Ready
                        || crew.lanes.iter().any(|lane| lane.role == LaneRole::Review)
                    {
                        return Ok(false);
                    }
                    crew.status = CrewStatus::Running;
                    crew.branch = Some(workspace.branch.clone());
                    crew.conflict = None;
                    crew.lanes.push(review_lane);
                    crew.note = Some(format!("Merged into {}; reviewing", workspace.branch));
                    moved = true;
                    Ok(true)
                })
                .await?;
                if !moved {
                    return Ok(after);
                }
                return start_pending_lanes(resources, scope, &project.project_id, &crew.crew_id)
                    .await
                    .map_err(|error| error.to_string());
            }
            let review_note = if budget_spent {
                " (the budget is spent, so there was no review)"
            } else {
                crew.lanes
                    .iter()
                    .rev()
                    .find(|lane| lane.role == LaneRole::Review)
                    .filter(|lane| lane.status != LaneStatus::Done)
                    .map(|_| " (the review did not finish; its changes are not included)")
                    .unwrap_or("")
            };
            let mut moved = false;
            let after = update_crew(&root, &project.project_id, &crew.crew_id, |crew| {
                if crew.status != CrewStatus::Ready {
                    return Ok(false);
                }
                crew.status = CrewStatus::Integrated;
                crew.branch = Some(workspace.branch.clone());
                crew.conflict = None;
                crew.pull_request_request_id = Some(crew_pull_request_request_id(&crew.crew_id));
                crew.note = Some(format!("Merged into {}{review_note}", workspace.branch));
                moved = true;
                Ok(true)
            })
            .await?;
            if moved {
                let question = crew_pull_request_question(
                    project,
                    &after,
                    &scope.principal(),
                    &scope.workspace(),
                );
                file(requests, question, &after.crew_id).await;
            }
            Ok(after)
        },
        Ok(IntegrationOutcome::Conflict {
            workspace,
            branch,
            files,
        }) => {
            let lane = lanes
                .iter()
                .find(|(_, ws)| ws.branch == branch)
                .map(|(lane, _)| lane.clone())
                .unwrap_or(branch);
            let mut moved = false;
            let after = update_crew(&root, &project.project_id, &crew.crew_id, |crew| {
                if crew.status != CrewStatus::Ready {
                    return Ok(false);
                }
                crew.status = CrewStatus::Conflict;
                crew.branch = Some(workspace.branch.clone());
                crew.conflict = Some(CrewConflict {
                    lane: lane.clone(),
                    files,
                });
                crew.resolve_request_id = Some(crew_resolve_request_id(
                    &crew.crew_id,
                    chrono::Utc::now().timestamp_millis(),
                ));
                crew.note = Some(format!(
                    "Lane {lane} conflicts with the lanes merged before it"
                ));
                moved = true;
                Ok(true)
            })
            .await?;
            if moved {
                let question =
                    crew_resolve_question(project, &after, &scope.principal(), &scope.workspace());
                file(requests, question, &after.crew_id).await;
            }
            Ok(after)
        },
        Err(error) => {
            // Left `ready`: the sweep retries, backing off, and the note says
            // why it waits.
            let note = format!("Could not merge the lanes yet: {error}");
            merge_failed(&root, requests, scope, project, crew, note).await
        },
    }
}

/// A ready crew's failed merges in a row, in this process.
#[derive(Debug, Clone, Copy)]
struct MergeRetry {
    failures: u32,
    next_attempt: std::time::Instant,
}

fn merge_retries() -> &'static std::sync::Mutex<std::collections::HashMap<String, MergeRetry>> {
    static RETRIES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, MergeRetry>>,
    > = std::sync::OnceLock::new();
    RETRIES.get_or_init(Default::default)
}

/// How long a ready crew waits after `failures` failed merges in a row: one
/// sweep after the first, doubling after each further one, at most
/// [`MAX_MERGE_RETRY_DELAY`].
fn merge_retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    SWEEP_INTERVAL
        .saturating_mul(1u32 << doublings)
        .min(MAX_MERGE_RETRY_DELAY)
}

/// Whether a ready crew's next merge attempt is still backing off.
fn merge_backing_off(crew_id: &str) -> bool {
    let retries = merge_retries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    retries
        .get(crew_id)
        .is_some_and(|retry| std::time::Instant::now() < retry.next_attempt)
}

/// Count one more failed merge; returns how many failed in a row.
fn record_merge_failure(crew_id: &str) -> u32 {
    let mut retries = merge_retries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let failures = retries
        .get(crew_id)
        .map_or(1, |retry| retry.failures.saturating_add(1));
    retries.insert(
        crew_id.to_string(),
        MergeRetry {
            failures,
            next_attempt: std::time::Instant::now() + merge_retry_delay(failures),
        },
    );
    failures
}

/// Forget a crew's failed merges: it merged, conflicted, or the owner asked
/// to merge again.
fn clear_merge_failures(crew_id: &str) {
    merge_retries()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(crew_id);
}

/// Record a failed merge on a ready crew: the note says why it waits, retries
/// back off, and after [`MERGE_FAILURES_BEFORE_NOTICE`] failures in a row the
/// owner gets one notice.
async fn merge_failed(
    root: &Path,
    requests: Option<&UserRequestService>,
    scope: &ScopeRef,
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    note: String,
) -> Result<VibeDevCrew, String> {
    let failures = record_merge_failure(&crew.crew_id);
    let shown = if failures >= MERGE_FAILURES_BEFORE_NOTICE {
        format!("{note} (failed {failures} times in a row; retrying less often)")
    } else {
        note.clone()
    };
    let after = update_crew(root, &project.project_id, &crew.crew_id, |crew| {
        let changed =
            crew.status == CrewStatus::Ready && crew.note.as_deref() != Some(shown.as_str());
        if changed {
            crew.note = Some(shown);
        }
        Ok(changed)
    })
    .await?;
    if failures == MERGE_FAILURES_BEFORE_NOTICE && after.status == CrewStatus::Ready {
        let notice = crew_merge_stuck_notice(
            project,
            &after,
            &note,
            failures,
            &scope.principal(),
            &scope.workspace(),
        );
        file(requests, notice, &after.crew_id).await;
    }
    Ok(after)
}

/// Start an integrator lane on a conflicted crew: it merges every work lane
/// and resolves the conflict; its branch is then merged instead of theirs. A
/// crew runs at most [`MAX_INTEGRATOR_LANES`] of them, and only with budget
/// left.
pub async fn resolve_crew_conflict(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let (_project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    if crew.status != CrewStatus::Conflict || crew.mode != CrewMode::Split {
        return Err(CrewError::Invalid(
            "only a split crew whose lanes conflict can be resolved by an agent".to_string(),
        ));
    }
    let exhausted = |crew: &VibeDevCrew| {
        format!(
            "integrator runs already tried {} times; resolve the conflict on the lane branches, \
             then merge again",
            integrator_attempts(crew)
        )
    };
    if !can_add_integrator(&crew) {
        return Err(CrewError::Invalid(exhausted(&crew)));
    }
    remaining_lane_budget(crew.budget_usd, lanes_spent_usd(&service, scope, &crew))
        .map_err(CrewError::Invalid)?;
    // Re-checked under the store's lock: of two resolves at once, one adds
    // the integrator, and a stop that landed first stays.
    let mut refused = None;
    update_crew(&root, project_id, crew_id, |crew| {
        if crew.status != CrewStatus::Conflict {
            refused = Some(format!(
                "the crew is {}; its conflict is no longer waiting",
                status_name(crew.status)
            ));
            return Ok(false);
        }
        if !can_add_integrator(crew) {
            refused = Some(exhausted(&*crew));
            return Ok(false);
        }
        crew.lanes.push(CrewLane {
            lane: "Integrate".to_string(),
            brief: "Merge every lane and resolve the conflict.".to_string(),
            owns: Vec::new(),
            coding_profile_id: None,
            run_task_id: None,
            status: LaneStatus::Pending,
            role: LaneRole::Integrate,
        });
        crew.status = CrewStatus::Running;
        crew.resolve_request_id = None;
        crew.note = Some("An integrator is resolving the conflict".to_string());
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)?;
    if let Some(message) = refused {
        return Err(CrewError::Invalid(message));
    }
    let started = start_pending_lanes(resources, scope, project_id, crew_id).await?;
    settle_question(
        resources,
        scope,
        crew.resolve_request_id.as_deref(),
        RESOLVE,
    )
    .await;
    Ok(started)
}

/// Keep one race attempt (`None`: keep none, which cancels the crew).
pub async fn choose_crew_winner(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
    lane_index: Option<usize>,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let (_project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    let not_choosing = || CrewError::Invalid("the crew is not waiting for a pick".to_string());
    if crew.status != CrewStatus::Choosing {
        return Err(not_choosing());
    }
    if let Some(index) = lane_index {
        let valid = crew
            .lanes
            .get(index)
            .is_some_and(|lane| lane.role == LaneRole::Work && lane.status == LaneStatus::Done);
        if !valid {
            return Err(CrewError::Invalid(format!(
                "lane {index} is not a finished attempt"
            )));
        }
    }
    let mut moved = false;
    update_crew(&root, project_id, crew_id, |crew| {
        // Re-checked under the store's lock: a stop or another pick first wins.
        if crew.status != CrewStatus::Choosing {
            return Ok(false);
        }
        crew.choose_request_id = None;
        match lane_index {
            Some(index) => {
                crew.winner = Some(index);
                crew.status = CrewStatus::Ready;
                crew.note = Some(format!("Keeping {}", crew.lanes[index].lane));
            },
            None => {
                crew.status = CrewStatus::Cancelled;
                crew.note = Some("No attempt kept".to_string());
            },
        }
        moved = true;
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)?;
    if !moved {
        return Err(not_choosing());
    }
    let decision =
        lane_index.map_or_else(|| KEEP_NONE.to_string(), |index| format!("lane:{index}"));
    settle_question(
        resources,
        scope,
        crew.choose_request_id.as_deref(),
        &decision,
    )
    .await;
    reconcile_crew(resources, scope, project_id, crew_id)
        .await
        .map_err(CrewError::Failed)
}

/// Merge again after the owner resolved a conflict on the lane branches.
pub async fn retry_crew_integration(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let (_project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    let not_mergeable = || {
        CrewError::Invalid(
            "only a crew whose lanes are done and not yet merged can be merged".to_string(),
        )
    };
    if !matches!(crew.status, CrewStatus::Conflict | CrewStatus::Ready) {
        return Err(not_mergeable());
    }
    let mut moved = false;
    update_crew(&root, project_id, crew_id, |crew| {
        // Re-checked under the store's lock: a review or integrator lane
        // starting, or a stop, moved it on.
        if !matches!(crew.status, CrewStatus::Conflict | CrewStatus::Ready) {
            return Ok(false);
        }
        crew.status = CrewStatus::Ready;
        crew.conflict = None;
        crew.resolve_request_id = None;
        crew.note = Some("Merging the lanes".to_string());
        moved = true;
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)?;
    if !moved {
        return Err(not_mergeable());
    }
    settle_question(resources, scope, crew.resolve_request_id.as_deref(), MANUAL).await;
    // The owner asked: merge now, whatever the backoff says.
    clear_merge_failures(crew_id);
    reconcile_crew(resources, scope, project_id, crew_id)
        .await
        .map_err(CrewError::Failed)
}

/// Open a pull request for an integrated crew's branch, with the owner's own
/// credentials (`trusted_workspace::open_pull_request`, which scans the
/// branch for secrets first). Idempotent: an opened request is returned.
pub async fn open_crew_pull_request(
    resources: &AgentResources,
    scope: &ScopeRef,
    project_id: &str,
    crew_id: &str,
) -> Result<VibeDevCrew, CrewError> {
    use super::trusted_workspace::{
        is_pull_request_url, load_run_workspace, open_pull_request, save_run_workspace,
    };
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let (project, crew) = read_crew(&root, project_id, crew_id)
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id} in project {project_id}")))?;
    // A stored value that is not a URL (an empty read saved by an older
    // build) is no pull request: open one.
    if crew
        .pull_request_url
        .as_deref()
        .is_some_and(is_pull_request_url)
    {
        return Ok(crew);
    }
    if crew.status != CrewStatus::Integrated {
        return Err(CrewError::Invalid(
            "a pull request needs the crew's lanes merged first".to_string(),
        ));
    }
    let mut workspace = load_run_workspace(&root, crew_id)
        .ok_or_else(|| CrewError::Failed("the crew's integration branch is missing".to_string()))?;
    let goal_line: String = one_line(&crew.goal).chars().take(72).collect();
    let title = format!("VibeDev crew: {goal_line}");
    let mut body = format!("{}\n\nMerged from {} lanes:\n", crew.goal, crew.lanes.len());
    for lane in &crew.lanes {
        body.push_str(&format!("- **{}**: {}\n", lane.lane, one_line(&lane.brief)));
    }
    body.push_str(&format!("\nProject: {}\n", project.name));
    let lock = crate::magician_v2::execution::coding_engine::shadow_admission_lock(&format!(
        "trusted:{crew_id}"
    ))
    .await;
    let _guard = lock
        .try_lock_owned()
        .map_err(|_| CrewError::Invalid("the crew's branch is busy; try again".to_string()))?;
    let save_root = root.clone();
    let url = tokio::task::spawn_blocking(move || {
        let url = open_pull_request(&mut workspace, &title, &body)?;
        save_run_workspace(&save_root, &workspace)?;
        Ok::<_, String>(url)
    })
    .await
    .map_err(|error| CrewError::Failed(format!("the pull request task failed: {error}")))?
    .map_err(CrewError::Failed)?;
    let opened = update_crew(&root, project_id, crew_id, |crew| {
        crew.pull_request_url = Some(url.clone());
        crew.pull_request_request_id = None;
        crew.note = Some(format!("Pull request opened: {url}"));
        Ok(true)
    })
    .await
    .map_err(CrewError::Failed)?;
    settle_question(
        resources,
        scope,
        crew.pull_request_request_id.as_deref(),
        OPEN_PULL_REQUEST,
    )
    .await;
    Ok(opened)
}

/// The owner answered one of a crew's questions: act on it. Only an answer to
/// the question the crew waits on now (`request_id`) counts; an answer to an
/// earlier one, such as a conflict already handled, is history.
pub async fn answer_crew_question(
    resources: &AgentResources,
    scope: &ScopeRef,
    crew_id: &str,
    question: CrewQuestion,
    request_id: &str,
    decision: &str,
) -> Result<(), CrewError> {
    let service = service_of(resources)?;
    let root = scope_root(&service, scope);
    let Some(project) = read_vibedev_projects(&root)
        .into_iter()
        .find(|project| project.crews.iter().any(|crew| crew.crew_id == crew_id))
    else {
        return Err(CrewError::NotFound(format!("no crew {crew_id}")));
    };
    let project_id = project.project_id.as_str();
    let crew = project
        .crews
        .iter()
        .find(|crew| crew.crew_id == crew_id)
        .cloned()
        .ok_or_else(|| CrewError::NotFound(format!("no crew {crew_id}")))?;
    let waiting = open_questions(&crew)
        .into_iter()
        .any(|(open, id)| open == question && id == request_id);
    if !waiting {
        return Ok(());
    }
    match question {
        CrewQuestion::Start => {
            if crew.status != CrewStatus::Proposed {
                return Ok(());
            }
            if decision == START {
                start_crew(resources, scope, project_id, crew_id).await?;
            } else {
                update_crew(&root, project_id, crew_id, |crew| {
                    if crew.status != CrewStatus::Proposed {
                        return Ok(false);
                    }
                    crew.status = CrewStatus::Cancelled;
                    crew.start_request_id = None;
                    crew.note = Some("Not started".to_string());
                    Ok(true)
                })
                .await
                .map_err(CrewError::Failed)?;
            }
        },
        CrewQuestion::PullRequest => {
            // Recorded only: the push is the owner's own action on the crew
            // (see `crew_pull_request_question`).
            let wants = decision == OPEN_PULL_REQUEST;
            update_crew(&root, project_id, crew_id, |crew| {
                if crew.pull_request_request_id.as_deref() != Some(request_id) {
                    return Ok(false);
                }
                crew.pull_request_request_id = None;
                if wants && crew.pull_request_url.is_none() {
                    crew.note = Some(
                        "Open the pull request from the crew in VibeDev; it pushes with your \
                         credentials"
                            .to_string(),
                    );
                }
                Ok(true)
            })
            .await
            .map_err(CrewError::Failed)?;
        },
        CrewQuestion::Choose => {
            if crew.status != CrewStatus::Choosing {
                return Ok(());
            }
            let index = decision
                .strip_prefix("lane:")
                .and_then(|index| index.parse::<usize>().ok());
            choose_crew_winner(resources, scope, project_id, crew_id, index).await?;
        },
        CrewQuestion::Resolve => {
            if crew.status != CrewStatus::Conflict {
                return Ok(());
            }
            let refused = if decision == RESOLVE {
                match resolve_crew_conflict(resources, scope, project_id, crew_id).await {
                    Ok(_) => return Ok(()),
                    // No tries or budget left: the question closes and the
                    // note says why.
                    Err(CrewError::Invalid(message)) => {
                        Some(format!("No integrator started: {message}"))
                    },
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            update_crew(&root, project_id, crew_id, |crew| {
                if crew.resolve_request_id.as_deref() != Some(request_id) {
                    return Ok(false);
                }
                crew.resolve_request_id = None;
                if let Some(note) = refused {
                    crew.note = Some(note);
                }
                Ok(true)
            })
            .await
            .map_err(CrewError::Failed)?;
        },
    }
    Ok(())
}

/// The questions a crew is waiting on, with their ids.
fn open_questions(crew: &VibeDevCrew) -> Vec<(CrewQuestion, &str)> {
    [
        (CrewQuestion::Start, crew.start_request_id.as_deref()),
        (
            CrewQuestion::PullRequest,
            crew.pull_request_request_id.as_deref(),
        ),
        (CrewQuestion::Choose, crew.choose_request_id.as_deref()),
        (CrewQuestion::Resolve, crew.resolve_request_id.as_deref()),
    ]
    .into_iter()
    .filter_map(|(question, id)| id.map(|id| (question, id)))
    .collect()
}

/// Every crew in `scope` the supervisor still has work for, with its
/// project id: running, merging, or waiting on an owner question.
fn open_crews(scope_root: &Path) -> Vec<(String, VibeDevCrew)> {
    read_vibedev_projects(scope_root)
        .into_iter()
        .flat_map(|project| {
            let project_id = project.project_id.clone();
            project
                .crews
                .into_iter()
                .filter(|crew| {
                    matches!(crew.status, CrewStatus::Running | CrewStatus::Ready)
                        || !open_questions(crew).is_empty()
                })
                .map(move |crew| (project_id.clone(), crew))
        })
        .collect()
}

/// The root executions of a crew's running lanes.
async fn running_lane_executions(
    service: &ArtifactV2Service,
    scope: &ScopeRef,
    crew: &VibeDevCrew,
) -> Vec<String> {
    let task_ids: Vec<String> = crew
        .lanes
        .iter()
        .filter(|lane| lane.status == LaneStatus::Running)
        .filter_map(|lane| lane.run_task_id.clone())
        .collect();
    if task_ids.is_empty() {
        return Vec::new();
    }
    service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &task_ids,
            crate::magician_v2::archived_filter::ArchivedFilter::Include,
        )
        .await
        .unwrap_or_default()
        .into_iter()
        .flat_map(|item| [item.active_root_execution_id, item.latest_root_execution_id])
        .flatten()
        .collect()
}

/// One pass over every scope: apply answered crew questions (a missed event
/// or a restart) and re-read running and merging crews. Returns the root
/// executions of the lanes still running, so the supervisor wakes early only
/// when one of those ends.
pub async fn sweep_crews(resources: &AgentResources) -> HashSet<String> {
    let mut lane_executions = HashSet::new();
    let Some(service) = resources.artifact_v2_service.clone() else {
        return lane_executions;
    };
    let requests = resources.user_request_service.clone();
    for (principal, workspace) in service.workspace().list_tenant_scopes() {
        let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
        let root = scope_root(&service, &scope);
        let crews = open_crews(&root);
        if crews.is_empty() {
            continue;
        }
        let answered: Vec<(String, String)> = requests
            .as_ref()
            .map(|requests| {
                requests
                    .list_history_for_scope(&scope.principal(), &scope.workspace(), None)
                    .into_iter()
                    .filter(|record| record.request.request_type.starts_with("vibedev.crew_"))
                    .filter_map(|record| {
                        let decision = record.response.as_ref()?.decision.clone();
                        Some((record.request.id.clone(), decision))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (project_id, crew) in crews {
            // Only the ids the crew waits on now: an answer to an earlier
            // occurrence of a question is never applied again.
            for (question, request_id) in open_questions(&crew) {
                if let Some((_, decision)) = answered.iter().find(|(id, _)| id == request_id) {
                    if let Err(error) = answer_crew_question(
                        resources,
                        &scope,
                        &crew.crew_id,
                        question,
                        request_id,
                        decision,
                    )
                    .await
                    {
                        tracing::warn!(crew_id = %crew.crew_id, %error, "could not apply a crew answer");
                    }
                }
            }
            if matches!(crew.status, CrewStatus::Running | CrewStatus::Ready) {
                match reconcile_crew(resources, &scope, &project_id, &crew.crew_id).await {
                    Ok(after) if after.status == CrewStatus::Running => {
                        lane_executions
                            .extend(running_lane_executions(&service, &scope, &after).await);
                    },
                    Ok(_) => {},
                    Err(error) => {
                        tracing::warn!(crew_id = %crew.crew_id, %error, "could not re-read a crew");
                    },
                }
            }
        }
        if let Some(requests) = requests.as_deref() {
            refile_lost_questions(requests, &scope, &root).await;
        }
    }
    lane_executions
}

/// The question a crew waits on, built again: `None` once the crew's status
/// no longer fits it.
fn crew_question_for(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    question: CrewQuestion,
    principal: &str,
    workspace: &str,
) -> Option<UserRequest> {
    Some(match (question, crew.status) {
        (CrewQuestion::Start, CrewStatus::Proposed) => {
            crew_start_request(project, crew, principal, workspace)
        },
        (CrewQuestion::Choose, CrewStatus::Choosing) => {
            crew_choose_question(project, crew, principal, workspace)
        },
        (CrewQuestion::Resolve, CrewStatus::Conflict) => {
            crew_resolve_question(project, crew, principal, workspace)
        },
        (CrewQuestion::PullRequest, CrewStatus::Integrated) => {
            crew_pull_request_question(project, crew, principal, workspace)
        },
        _ => return None,
    })
}

/// File again a question a crew waits on that was never filed. The crew
/// stores the id before filing, so a submit that failed left the crew waiting
/// with nobody asked. A question pending, or in history (answered or not), is
/// left alone; filing an id again is idempotent anyway.
async fn refile_lost_questions(requests: &UserRequestService, scope: &ScopeRef, scope_root: &Path) {
    let (principal, workspace) = (scope.principal(), scope.workspace());
    let projects = read_vibedev_projects(scope_root);
    let waiting: Vec<(&VibeDevProjectRecord, &VibeDevCrew, CrewQuestion, &str)> = projects
        .iter()
        .flat_map(|project| {
            project.crews.iter().flat_map(move |crew| {
                open_questions(crew)
                    .into_iter()
                    .map(move |(question, id)| (project, crew, question, id))
            })
        })
        .collect();
    if waiting.is_empty() {
        return;
    }
    let mut known: HashSet<String> = requests
        .list_pending_for_scope(&principal, &workspace)
        .await
        .into_iter()
        .map(|request| request.id)
        .collect();
    known.extend(
        requests
            .list_history_for_scope(&principal, &workspace, None)
            .into_iter()
            .map(|record| record.request.id),
    );
    for (project, crew, question, request_id) in waiting {
        if known.contains(request_id) {
            continue;
        }
        let Some(request) = crew_question_for(project, crew, question, &principal, &workspace)
        else {
            continue;
        };
        if request.id != request_id {
            continue;
        }
        tracing::info!(crew_id = %crew.crew_id, %request_id, "re-filing a crew question nobody was asked");
        file(Some(requests), request, &crew.crew_id).await;
    }
}

/// Run crews in the background: act on crew questions as they are answered,
/// re-read crews when one of their lane runs ends, and sweep every
/// [`SWEEP_INTERVAL`] for anything an event missed (a lane started since the
/// last pass included).
pub fn spawn_vibedev_crew_supervisor(
    resources: Arc<AgentResources>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = resources
            .event_broadcaster
            .as_ref()
            .map(|broadcaster| broadcaster.subscribe());
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Filled by each sweep; the first tick fires at once.
        let mut lane_executions = HashSet::new();
        loop {
            let nudge = match events.as_mut() {
                Some(receiver) => tokio::select! {
                    _ = ticker.tick() => true,
                    event = receiver.recv() => match event {
                        Ok(event) => crew_event_nudges(&resources, event, &lane_executions).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            events = None;
                            false
                        },
                    },
                },
                None => {
                    ticker.tick().await;
                    true
                },
            };
            if nudge {
                lane_executions = sweep_crews(&resources).await;
            }
        }
    })
}

/// Whether `event` is the end of one of the crew lanes' runs. Every other
/// execution in the system ends without waking the supervisor.
fn ends_lane_run(
    event: &crate::magician_v2::realtime_events::RuntimeTransportEvent,
    lane_executions: &HashSet<String>,
) -> bool {
    use crate::magician_v2::realtime_events::RuntimeTransportEvent as Event;
    match event {
        Event::ExecutionCompleted { execution_id, .. }
        | Event::ExecutionFailed { execution_id, .. }
        | Event::ExecutionCancelled { execution_id, .. }
        | Event::AgenticExecutionCompleted { execution_id, .. } => {
            lane_executions.contains(execution_id)
        },
        _ => false,
    }
}

/// Act on one event: a crew answer is applied at once; a lane's run ending
/// asks for a sweep.
async fn crew_event_nudges(
    resources: &Arc<AgentResources>,
    event: crate::magician_v2::realtime_events::RuntimeTransportEvent,
    lane_executions: &HashSet<String>,
) -> bool {
    use crate::magician_v2::realtime_events::RuntimeTransportEvent as Event;
    match event {
        Event::HitlResolved {
            correlation_id,
            decision,
            principal,
            workspace,
            ..
        } => {
            let Some((crew_id, question)) = CrewQuestion::parse(&correlation_id) else {
                return false;
            };
            let (Some(principal), Some(workspace), Some(decision)) =
                (principal, workspace, decision)
            else {
                // Not enough to act on here; the sweep reads the answer.
                return true;
            };
            let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
            if let Err(error) = answer_crew_question(
                resources,
                &scope,
                crew_id,
                question,
                &correlation_id,
                &decision,
            )
            .await
            {
                tracing::warn!(%crew_id, %error, "could not apply a crew answer");
            }
            false
        },
        event => ends_lane_run(&event, lane_executions),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(lane: &str, owns: &[&str]) -> CrewLanePlan {
        CrewLanePlan {
            lane: lane.to_string(),
            brief: format!("Build the {lane}"),
            owns: owns.iter().map(|path| path.to_string()).collect(),
            coding_profile_id: None,
        }
    }

    fn crew(lanes: Vec<CrewLane>, mode: CrewMode) -> VibeDevCrew {
        let mut crew = VibeDevCrew {
            crew_id: "crew_1".to_string(),
            goal: "Ship login".to_string(),
            status: CrewStatus::Running,
            mode,
            lanes,
            review: false,
            winner: None,
            choose_request_id: None,
            resolve_request_id: None,
            budget_usd: None,
            spent_usd: None,
            start_request_id: None,
            branch: None,
            conflict: None,
            pull_request_request_id: None,
            pull_request_url: None,
            note: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        crew.status = CrewStatus::Running;
        crew
    }

    fn with_status(mut lanes: Vec<CrewLane>, statuses: &[LaneStatus]) -> Vec<CrewLane> {
        for (lane, status) in lanes.iter_mut().zip(statuses) {
            lane.status = *status;
        }
        lanes
    }

    #[test]
    fn lanes_are_validated_and_tidied() {
        let lanes = normalize_crew_lanes(vec![
            plan("  API  layer ", &["./src/api", "src/api", " "]),
            plan("UI", &[]),
        ])
        .expect("valid lanes");
        assert_eq!(lanes[0].lane, "API layer");
        assert_eq!(lanes[0].owns, vec!["src/api"]);
        assert_eq!(lanes[1].status, LaneStatus::Pending);
        assert_eq!(lanes[1].role, LaneRole::Work);

        assert!(normalize_crew_lanes(Vec::new()).is_err());
        assert!(normalize_crew_lanes(vec![plan("UI", &[]), plan("ui", &[])]).is_err());
        assert!(normalize_crew_lanes(vec![plan("UI", &["../outside"])]).is_err());
        assert!(normalize_crew_lanes(vec![plan("UI", &["/etc"])]).is_err());
        let mut empty = plan("UI", &[]);
        empty.brief = "  ".to_string();
        assert!(normalize_crew_lanes(vec![empty]).is_err());
        let many = (0..=MAX_CREW_LANES)
            .map(|i| plan(&format!("L{i}"), &[]))
            .collect();
        assert!(normalize_crew_lanes(many).is_err());
        assert!(normalize_crew_budget(Some(0.0)).is_err());
        assert!(normalize_crew_budget(Some(0.009)).is_err());
        assert!(normalize_crew_budget(Some(f64::NAN)).is_err());
        assert_eq!(normalize_crew_budget(Some(0.01)), Ok(Some(0.01)));
        assert_eq!(normalize_crew_budget(Some(2.5)), Ok(Some(2.5)));
        assert!(normalize_crew_goal(" ").is_err());
    }

    #[test]
    fn the_planner_sees_every_path_of_a_small_repo_and_folders_of_a_large_one() {
        let small: Vec<String> = ["index.html", "src/app.js", "test/app.test.js"]
            .iter()
            .map(|path| path.to_string())
            .collect();
        assert_eq!(
            crew_plan_repo_layout(&small, 10),
            "index.html\nsrc/app.js\ntest/app.test.js"
        );
        assert_eq!(crew_plan_repo_layout(&[], 10), "(no committed files yet)");

        let mut large: Vec<String> = vec!["Cargo.toml".to_string()];
        large.extend((0..30).map(|i| format!("crates/api/src/file{i}.rs")));
        large.extend((0..5).map(|i| format!("ui/lib/view{i}.svelte")));
        large.push("docs/README.md".to_string());
        assert_eq!(
            crew_plan_repo_layout(&large, 10),
            "37 files; folders show how many files they hold.\nCargo.toml\ncrates/api/ (30 \
             files)\ndocs/ (1 file)\nui/lib/ (5 files)"
        );
        // Too many folders two levels deep: one level, then cut.
        let wide: Vec<String> = (0..12)
            .flat_map(|i| [format!("pkg{i:02}/a/x.rs"), format!("pkg{i:02}/b/y.rs")])
            .collect();
        let layout = crew_plan_repo_layout(&wide, 10);
        assert!(layout.contains("\npkg00/ (2 files)\n"), "{layout}");
        assert!(layout.ends_with("… and 2 more"), "{layout}");
    }

    #[test]
    fn the_rail_shows_open_crews_and_pages_the_settled_ones() {
        let at = |id: &str, status: CrewStatus, created: i64, updated: i64| {
            let mut c = crew(Vec::new(), CrewMode::Split);
            c.crew_id = id.to_string();
            c.status = status;
            c.created_at_ms = created;
            c.updated_at_ms = updated;
            c
        };
        let mut merged_with_pr = at("pr", CrewStatus::Integrated, 6, 60);
        merged_with_pr.pull_request_url = Some("https://example.com/pr/1".to_string());
        let crews = vec![
            at("run", CrewStatus::Running, 1, 10),
            at("merged", CrewStatus::Integrated, 2, 20),
            at("stop1", CrewStatus::Cancelled, 3, 30),
            at("stop2", CrewStatus::Cancelled, 4, 80),
            at("fail", CrewStatus::Failed, 5, 50),
            merged_with_pr,
        ];
        let ids = |page: &CrewPage| {
            page.crews
                .iter()
                .map(|c| c.crew_id.clone())
                .collect::<Vec<_>>()
        };

        let open = page_crews(crews.clone(), CrewListState::Open, 0, None);
        assert_eq!(ids(&open), vec!["merged", "run"]);
        assert_eq!((open.total, open.settled_total), (2, 4));

        let first = page_crews(crews.clone(), CrewListState::Settled, 0, Some(2));
        assert_eq!(ids(&first), vec!["stop2", "pr"]);
        assert_eq!(first.total, 4);
        let second = page_crews(crews.clone(), CrewListState::Settled, 2, Some(2));
        assert_eq!(ids(&second), vec!["fail", "stop1"]);
        assert!(
            page_crews(crews.clone(), CrewListState::Settled, 4, Some(2))
                .crews
                .is_empty()
        );

        assert_eq!(
            page_crews(crews, CrewListState::All, 0, None).crews.len(),
            6
        );
        assert_eq!(CrewListState::parse(None), Ok(CrewListState::All));
        assert_eq!(
            CrewListState::parse(Some("settled")),
            Ok(CrewListState::Settled)
        );
        assert!(CrewListState::parse(Some("closed")).is_err());
    }

    #[test]
    fn the_planner_answer_parses_fenced_or_bare() {
        let wrapped = "```json\n{\"lanes\": [{\"lane\": \"API\", \"brief\": \"b\", \"owns\": [\"src\"], \"extra\": 1}]}\n```";
        assert_eq!(parse_crew_plan(wrapped).unwrap()[0].owns, vec!["src"]);
        let bare = "[{\"lane\": \"UI\", \"brief\": \"b\"}]";
        assert_eq!(parse_crew_plan(bare).unwrap()[0].lane, "UI");
        assert!(parse_crew_plan("I think you should").is_err());
    }

    #[test]
    fn a_work_lane_request_names_its_claim_and_everyone_elses() {
        let lanes = normalize_crew_lanes(vec![plan("API", &["src/api"]), plan("UI", &["web"])])
            .expect("lanes");
        let crew = crew(lanes, CrewMode::Split);
        let request = lane_request(&crew, 1, &LaneContext::default());
        // The run's title comes from the first line: the lane's own task.
        assert!(request.starts_with("Build the UI\n\nYou are one lane"));
        assert_eq!(
            crate::magician_v2::vibedev::run_service::vibedev_run_task_title(&request, false),
            "VibeDev · Build the UI"
        );
        assert!(request.contains("Crew goal:\nShip login"));
        assert!(request.contains("Your lane: UI\nBuild the UI"));
        assert!(request.contains("Paths you own (change these):\n- web"));
        assert!(request.contains("- API: src/api"));
        assert!(!request.contains("- UI: web"));
    }

    #[test]
    fn integrate_and_review_lanes_get_the_branches_they_work_on() {
        let mut lanes =
            normalize_crew_lanes(vec![plan("API", &[]), plan("UI", &[])]).expect("lanes");
        lanes.push(CrewLane {
            lane: "Integrate".to_string(),
            brief: "b".to_string(),
            owns: Vec::new(),
            coding_profile_id: None,
            run_task_id: None,
            status: LaneStatus::Pending,
            role: LaneRole::Integrate,
        });
        let mut crew = crew(lanes, CrewMode::Split);
        crew.conflict = Some(CrewConflict {
            lane: "UI".to_string(),
            files: vec!["README.md".to_string()],
        });
        let context = LaneContext {
            branches: vec![
                ("API".to_string(), "vibedev/app-aaaa".to_string()),
                ("UI".to_string(), "vibedev/app-bbbb".to_string()),
            ],
            crew_branch: Some("vibedev/app-crew-cccc".to_string()),
            base: Some("abc123".to_string()),
        };
        let integrate = lane_request(&crew, 2, &context);
        assert!(
            integrate.starts_with("Merge the crew's lane branches and resolve their conflicts\n\n")
        );
        assert!(
            integrate.contains("git fetch origin vibedev/app-aaaa && git merge --no-ff FETCH_HEAD")
        );
        assert!(integrate.contains("stopped at lane UI in: README.md"));

        crew.lanes[2].role = LaneRole::Review;
        let review = lane_request(&crew, 2, &context);
        assert!(review.starts_with("Review the crew's merged branch before its pull request\n\n"));
        assert!(review
            .contains("git fetch origin vibedev/app-crew-cccc && git reset --hard FETCH_HEAD"));
        assert!(review.contains("git diff abc123...HEAD"));
        assert!(review.contains("Lanes merged: API, UI"));
    }

    #[test]
    fn a_split_crew_settles_from_its_work_lanes_and_its_integrator() {
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        use LaneStatus::*;
        assert_eq!(
            crew_status_from_lanes(&with_status(lanes.clone(), &[Done, Running])),
            CrewStatus::Running
        );
        assert_eq!(
            crew_status_from_lanes(&with_status(lanes.clone(), &[Done, Done])),
            CrewStatus::Ready
        );
        assert_eq!(
            crew_status_from_lanes(&with_status(lanes.clone(), &[Done, Failed])),
            CrewStatus::Failed
        );
        assert_eq!(
            crew_status_from_lanes(&with_status(lanes.clone(), &[Stopped, Stopped])),
            CrewStatus::Cancelled
        );
        let mut with_integrator = with_status(lanes, &[Done, Done]);
        with_integrator.push(CrewLane {
            lane: "Integrate".to_string(),
            brief: "b".to_string(),
            owns: Vec::new(),
            coding_profile_id: None,
            run_task_id: None,
            status: Failed,
            role: LaneRole::Integrate,
        });
        // While the conflict it was for is open, a failed integrator leaves
        // the crew conflicted.
        let mut conflicted = crew(with_integrator.clone(), CrewMode::Split);
        conflicted.conflict = Some(CrewConflict {
            lane: "B".to_string(),
            files: vec!["README.md".to_string()],
        });
        assert_eq!(crew_status_after_lanes(&conflicted), CrewStatus::Conflict);
        with_integrator[2].status = Done;
        assert_eq!(crew_status_from_lanes(&with_integrator), CrewStatus::Ready);
        assert_eq!(lanes_summary(&with_integrator), "3 of 3 lanes: 3 done");
        assert_eq!(LaneStatus::from_task_status("completed"), LaneStatus::Done);
        assert_eq!(LaneStatus::from_task_status("running"), LaneStatus::Running);
        assert!(CrewStatus::Integrated.is_settled());
        assert!(!CrewStatus::Ready.is_settled());
    }

    #[test]
    fn a_race_waits_for_a_pick_once_any_attempt_finishes() {
        let catalog = VibeDevCodingCatalog {
            default_profile_id: "coding-balanced".to_string(),
            entries: Vec::new(),
        };
        let lanes = race_lanes("Ship login", None, Some(2), &catalog).expect("race lanes");
        assert_eq!(lanes.len(), 2);
        assert_eq!(
            lanes[0].coding_profile_id.as_deref(),
            Some("coding-balanced")
        );
        assert!(lanes[0].lane.starts_with("Attempt 1"));
        assert!(race_lanes("g", None, Some(1), &catalog).is_err());
        assert!(race_lanes("g", Some(vec!["only".to_string()]), None, &catalog).is_err());
        let named = race_lanes(
            "g",
            Some(vec!["a".into(), "b".into(), "c".into()]),
            None,
            &catalog,
        )
        .unwrap();
        assert_eq!(named.len(), 3);

        use LaneStatus::*;
        let mut race = crew(with_status(lanes, &[Failed, Done]), CrewMode::Race);
        assert_eq!(crew_status_after_lanes(&race), CrewStatus::Choosing);
        let race_request = lane_request(&race, 0, &LaneContext::default());
        assert!(race_request.starts_with("Ship login\n\n"));
        assert!(race_request.contains("one of 2 coding agents"));
        race.winner = Some(1);
        assert_eq!(crew_status_after_lanes(&race), CrewStatus::Ready);
        race.lanes[1].status = Failed;
        assert_eq!(crew_status_after_lanes(&race), CrewStatus::Failed);

        let question = crew_choose_question(
            &serde_json::from_value(serde_json::json!({
                "project_id": "p", "name": "Shop", "chat_thread_id": "t",
                "chat_session_id": "s", "created_at_ms": 0, "updated_at_ms": 0
            }))
            .unwrap(),
            &crew(
                with_status(race.lanes.clone(), &[Done, Failed]),
                CrewMode::Race,
            ),
            "owner",
            "default",
        );
        let ids: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(ids, vec!["lane:0", "none"]);
    }

    #[test]
    fn question_ids_round_trip() {
        for question in [
            CrewQuestion::Start,
            CrewQuestion::PullRequest,
            CrewQuestion::Choose,
            CrewQuestion::Resolve,
        ] {
            let id = question.request_id("crew_abc");
            assert_eq!(CrewQuestion::parse(&id), Some(("crew_abc", question)));
            assert!(question.request_type().starts_with("vibedev.crew_"));
        }
        assert_eq!(
            crew_start_request_id("crew_abc"),
            "vibedev-crew-crew_abc-start"
        );
        assert_eq!(CrewQuestion::parse("vibedev-crew--start"), None);
        assert_eq!(CrewQuestion::parse("other"), None);
    }

    fn project() -> VibeDevProjectRecord {
        serde_json::from_value(serde_json::json!({
            "project_id": "p", "name": "Shop", "chat_thread_id": "t",
            "chat_session_id": "s", "created_at_ms": 0, "updated_at_ms": 0
        }))
        .unwrap()
    }

    fn integrator(status: LaneStatus) -> CrewLane {
        CrewLane {
            lane: "Integrate".to_string(),
            brief: "b".to_string(),
            owns: Vec::new(),
            coding_profile_id: None,
            run_task_id: Some("task_int".to_string()),
            status,
            role: LaneRole::Integrate,
        }
    }

    #[test]
    fn each_conflict_is_its_own_question() {
        let first = crew_resolve_request_id("crew_abc", 1_700_000_000_000);
        let second = crew_resolve_request_id("crew_abc", 1_700_000_000_001);
        assert_ne!(first, second);
        assert_eq!(first, "vibedev-crew-crew_abc-resolve-1700000000000");
        assert_eq!(
            CrewQuestion::parse(&first),
            Some(("crew_abc", CrewQuestion::Resolve))
        );
        // Ids stored before occurrences existed still parse.
        assert_eq!(
            CrewQuestion::parse("vibedev-crew-crew_abc-resolve"),
            Some(("crew_abc", CrewQuestion::Resolve))
        );
        assert_eq!(CrewQuestion::parse("vibedev-crew-crew_abc-resolve-"), None);

        // The question carries the id the crew stored, so an answer to an
        // earlier conflict never matches the one the crew waits on now.
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut conflicted = crew(lanes, CrewMode::Split);
        conflicted.status = CrewStatus::Conflict;
        conflicted.resolve_request_id = Some(second.clone());
        let question = crew_resolve_question(&project(), &conflicted, "owner", "default");
        assert_eq!(question.id, second);
        assert_eq!(
            open_questions(&conflicted),
            vec![(CrewQuestion::Resolve, second.as_str())]
        );
    }

    #[test]
    fn integrator_runs_are_capped() {
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut conflicted = crew(lanes, CrewMode::Split);
        conflicted.status = CrewStatus::Conflict;
        conflicted.lanes.push(integrator(LaneStatus::Failed));
        assert!(can_add_integrator(&conflicted));
        let options = |crew: &VibeDevCrew| -> Vec<String> {
            crew_resolve_question(&project(), crew, "owner", "default")
                .options
                .into_iter()
                .map(|option| option.id)
                .collect()
        };
        assert_eq!(options(&conflicted), vec![RESOLVE, MANUAL]);
        while integrator_attempts(&conflicted) < MAX_INTEGRATOR_LANES {
            conflicted.lanes.push(integrator(LaneStatus::Failed));
        }
        assert!(!can_add_integrator(&conflicted));
        assert_eq!(options(&conflicted), vec![MANUAL]);
        assert!(
            crew_resolve_question(&project(), &conflicted, "owner", "default")
                .question
                .contains("tried 3 times")
        );
        // A race never gets an integrator.
        let race = crew(Vec::new(), CrewMode::Race);
        assert!(!can_add_integrator(&race));
    }

    #[test]
    fn a_lane_is_capped_at_what_the_crew_budget_has_left() {
        assert_eq!(remaining_lane_budget(None, 12.0), Ok(None));
        assert_eq!(remaining_lane_budget(Some(5.0), 0.0), Ok(Some(5.0)));
        assert_eq!(remaining_lane_budget(Some(5.0), 1.234), Ok(Some(3.76)));
        // Less than a cent left prints as `$0.00`, which reads back as no cap.
        assert!(remaining_lane_budget(Some(1.0), 0.995).is_err());
        assert!(remaining_lane_budget(Some(1.0), 2.0).is_err());
    }

    #[test]
    fn a_huge_race_is_refused_before_it_is_built() {
        let catalog = VibeDevCodingCatalog {
            default_profile_id: "coding-balanced".to_string(),
            entries: Vec::new(),
        };
        assert!(race_lanes("g", None, Some(usize::MAX), &catalog).is_err());
        assert!(race_lanes("g", None, Some(MAX_CREW_LANES + 1), &catalog).is_err());
        assert_eq!(
            race_lanes("g", None, Some(MAX_CREW_LANES), &catalog)
                .unwrap()
                .len(),
            MAX_CREW_LANES
        );
    }

    #[test]
    fn only_a_lane_run_ending_wakes_the_supervisor() {
        use crate::magician_v2::realtime_events::RuntimeTransportEvent;
        let ended = |execution_id: &str| RuntimeTransportEvent::ExecutionCancelled {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            plan_id: "plan".to_string(),
            step_index: 0,
            timestamp: 0,
        };
        let lanes: HashSet<String> = HashSet::from(["exec_lane".to_string()]);
        assert!(ends_lane_run(&ended("exec_lane"), &lanes));
        assert!(!ends_lane_run(&ended("exec_other"), &lanes));
        assert!(!ends_lane_run(&ended("exec_lane"), &HashSet::new()));
    }

    #[test]
    fn lanes_share_the_crew_budget_instead_of_each_getting_all_of_it() {
        let share = |budget, spent, pending, running| {
            share_lane_budget(budget, spent, pending, running)
                .map(|share| (share.per_lane_usd, share.starting))
        };
        assert_eq!(share(None, 3.0, 4, 0), Ok((None, 4)));
        assert_eq!(share(Some(10.0), 0.0, 4, 0), Ok((Some(2.5), 4)));
        // Running lanes hold a share of what is left too.
        assert_eq!(share(Some(10.0), 1.0, 2, 1), Ok((Some(3.0), 2)));
        // Rounded down to whole cents, so the lanes stay within the budget.
        assert_eq!(share(Some(1.0), 0.0, 3, 0), Ok((Some(0.33), 3)));
        // Under a cent each: fewer lanes start, never none.
        assert_eq!(share(Some(0.02), 0.0, 3, 0), Ok((Some(0.01), 2)));
        // With lanes running, the rest wait for them to end.
        assert_eq!(share(Some(0.02), 0.0, 3, 2), Ok((Some(0.01), 0)));
        assert_eq!(share(Some(0.03), 0.0, 3, 2), Ok((Some(0.01), 1)));
        assert!(share(Some(1.0), 0.995, 2, 0).is_err());
        for budget in [0.01, 0.07, 1.0, 3.33, 25.0] {
            for pending in 1..=MAX_CREW_LANES {
                let (per_lane, starting) = share(Some(budget), 0.0, pending, 0).unwrap();
                let per_lane = per_lane.unwrap();
                assert!((1..=pending).contains(&starting));
                assert!(per_lane >= MIN_LANE_BUDGET_USD);
                assert!(
                    per_lane * starting as f64 <= budget + 1e-9,
                    "{budget} {pending}"
                );
            }
        }
    }

    #[test]
    fn an_admitted_lane_run_is_waited_for_not_taken_as_deleted() {
        let now = chrono::Utc::now();
        let stale = now - chrono::Duration::seconds(LANE_ADMISSION_GRACE_SECS + 1);
        assert_eq!(missing_lane_task(None, now), MissingLaneTask::Gone);
        assert_eq!(
            missing_lane_task(Some((DispatchIntentState::Settled, now)), now),
            MissingLaneTask::Gone
        );
        assert_eq!(
            missing_lane_task(Some((DispatchIntentState::Failed, now)), now),
            MissingLaneTask::Gone
        );
        assert_eq!(
            missing_lane_task(Some((DispatchIntentState::Pending, now)), now),
            MissingLaneTask::Waiting
        );
        assert_eq!(
            missing_lane_task(Some((DispatchIntentState::Pending, stale)), now),
            MissingLaneTask::GiveUp
        );
        // A claim is finished by its holder or by the next restart.
        assert_eq!(
            missing_lane_task(Some((DispatchIntentState::Claimed, stale)), now),
            MissingLaneTask::Waiting
        );
    }

    #[test]
    fn an_old_failed_integrator_does_not_undo_a_later_merge() {
        use LaneStatus::*;
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut merged = crew(with_status(lanes, &[Done, Done]), CrewMode::Split);
        merged.lanes.push(integrator(Failed));
        // The owner resolved it by hand and merged again: the conflict is
        // cleared, the merge started a review, and the review finished.
        merged.conflict = None;
        merged.review = true;
        merged.lanes.push(CrewLane {
            lane: "Review".to_string(),
            brief: "b".to_string(),
            owns: Vec::new(),
            coding_profile_id: None,
            run_task_id: Some("task_review".to_string()),
            status: Done,
            role: LaneRole::Review,
        });
        assert_eq!(crew_status_after_lanes(&merged), CrewStatus::Ready);
        // The same lanes with the conflict still open stay conflicted.
        merged.conflict = Some(CrewConflict {
            lane: "B".to_string(),
            files: Vec::new(),
        });
        assert_eq!(crew_status_after_lanes(&merged), CrewStatus::Conflict);
    }

    #[test]
    fn an_integrator_that_never_started_is_no_try_and_resolve_needs_budget() {
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut conflicted = crew(lanes, CrewMode::Split);
        conflicted.status = CrewStatus::Conflict;
        let mut unstarted = integrator(LaneStatus::Stopped);
        unstarted.run_task_id = None;
        conflicted.lanes.push(unstarted.clone());
        assert_eq!(integrator_attempts(&conflicted), 0);
        assert!(can_add_integrator(&conflicted));
        // Lanes that never started still cannot pile up.
        while conflicted
            .lanes
            .iter()
            .filter(|lane| lane.role == LaneRole::Integrate)
            .count()
            < 2 * MAX_INTEGRATOR_LANES
        {
            conflicted.lanes.push(unstarted.clone());
        }
        assert!(!can_add_integrator(&conflicted));

        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut broke = crew(lanes, CrewMode::Split);
        broke.status = CrewStatus::Conflict;
        broke.budget_usd = Some(1.0);
        broke.spent_usd = Some(1.0);
        assert!(!crew_budget_left(&broke));
        let question = crew_resolve_question(&project(), &broke, "owner", "default");
        let ids: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(ids, vec![MANUAL]);
        assert!(question.question.contains("budget is spent"));
        broke.spent_usd = Some(0.5);
        assert!(crew_budget_left(&broke));
    }

    #[test]
    fn a_merge_that_keeps_failing_backs_off() {
        assert_eq!(merge_retry_delay(1), SWEEP_INTERVAL);
        assert_eq!(merge_retry_delay(2), SWEEP_INTERVAL * 2);
        assert_eq!(merge_retry_delay(3), SWEEP_INTERVAL * 4);
        assert_eq!(merge_retry_delay(20), MAX_MERGE_RETRY_DELAY);
        assert_eq!(merge_retry_delay(u32::MAX), MAX_MERGE_RETRY_DELAY);

        let crew_id = "crew_backoff_test";
        assert!(!merge_backing_off(crew_id));
        assert_eq!(record_merge_failure(crew_id), 1);
        assert!(merge_backing_off(crew_id));
        assert_eq!(record_merge_failure(crew_id), 2);
        clear_merge_failures(crew_id);
        assert!(!merge_backing_off(crew_id));
        assert_eq!(record_merge_failure(crew_id), 1);
        clear_merge_failures(crew_id);
    }

    #[test]
    fn a_lost_question_is_rebuilt_with_the_id_the_crew_waits_on() {
        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut conflicted = crew(lanes, CrewMode::Split);
        conflicted.status = CrewStatus::Conflict;
        let id = crew_resolve_request_id(&conflicted.crew_id, 42);
        conflicted.resolve_request_id = Some(id.clone());
        let rebuilt = crew_question_for(
            &project(),
            &conflicted,
            CrewQuestion::Resolve,
            "owner",
            "default",
        )
        .expect("a conflicted crew asks to resolve");
        assert_eq!(rebuilt.id, id);
        // Moved on: nothing to file.
        conflicted.status = CrewStatus::Running;
        assert!(crew_question_for(
            &project(),
            &conflicted,
            CrewQuestion::Resolve,
            "owner",
            "default"
        )
        .is_none());

        let lanes = normalize_crew_lanes(vec![plan("A", &[]), plan("B", &[])]).expect("lanes");
        let mut race = crew(
            with_status(lanes, &[LaneStatus::Done, LaneStatus::Done]),
            CrewMode::Race,
        );
        race.status = CrewStatus::Choosing;
        let rebuilt =
            crew_question_for(&project(), &race, CrewQuestion::Choose, "owner", "default")
                .expect("a choosing race asks for a pick");
        assert_eq!(rebuilt.id, CrewQuestion::Choose.request_id(&race.crew_id));
    }
}
