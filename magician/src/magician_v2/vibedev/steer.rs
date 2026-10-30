//! Steering a live VibeDev run by name, from chat or voice.
//!
//! `@vibedev #steer API: use Postgres` (or `#steer the API lane to use
//! Postgres`), or spoken "tell vibedev the API lane to use Postgres", reach the
//! rail with `steer` set (`chat::invoke_grammar`). The target is matched
//! against the scope's live runs (`vibedev::live_runs`): a crew lane's label,
//! the run's title, or its project's name. Exactly one match is steered
//! through the coding control registry; none or several get a sentence that
//! says which runs there are. Nothing is ever started here.

use std::sync::Arc;

use super::live_runs::{live_runs, project_task_ids, LiveRun, LiveRunState, OwnerRequests};
use super::projects::read_vibedev_projects;
use crate::magician_v2::archived_filter::ArchivedFilter;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef};

/// A steer as the owner said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerRequest {
    /// Which run: a lane label, a run title or a project name.
    pub target: String,
    pub message: String,
}

/// Parse a steer prompt: `[tell] <target> to <message>` or
/// `<target>: <message>`. A leading `the` and a trailing `lane` or `run` are
/// dropped from the target.
pub fn parse_steer_prompt(prompt: &str) -> Option<SteerRequest> {
    let mut text = prompt.trim();
    // `get` rather than slicing: byte 5 can fall inside a curly quote or an
    // em dash.
    if text
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tell "))
    {
        text = text[5..].trim_start();
    }
    let (target, message) = match text.split_once(':') {
        Some((target, message)) if !target.trim().is_empty() && !target.contains(" to ") => {
            (target, message)
        },
        _ => {
            let lower = text.to_ascii_lowercase();
            let at = lower.find(" to ")?;
            (&text[..at], &text[at + 4..])
        },
    };
    let mut target = target.trim();
    for prefix in ["the ", "The "] {
        if let Some(rest) = target.strip_prefix(prefix) {
            target = rest.trim_start();
        }
    }
    let lower = target.to_ascii_lowercase();
    for suffix in [" lane", " run"] {
        if lower.ends_with(suffix) {
            target = target[..target.len() - suffix.len()].trim_end();
            break;
        }
    }
    let message = message.trim();
    if target.is_empty() || message.is_empty() {
        return None;
    }
    Some(SteerRequest {
        target: target.to_string(),
        message: message.to_string(),
    })
}

/// How a target matched the live runs.
#[derive(Debug, Clone, PartialEq)]
pub enum SteerMatch {
    One(LiveRun),
    Several(Vec<LiveRun>),
    None,
}

fn steerable(run: &LiveRun) -> bool {
    matches!(
        run.state,
        LiveRunState::Working | LiveRunState::NeedsYou | LiveRunState::Paused
    )
}

/// Match `target` against the steerable runs: an exact lane, title or
/// project name wins; otherwise every run whose lane, title or project
/// contains it (case-insensitive).
pub fn match_live_run(target: &str, runs: &[LiveRun]) -> SteerMatch {
    let needle = target.trim().to_lowercase();
    if needle.is_empty() {
        return SteerMatch::None;
    }
    let names = |run: &LiveRun| -> Vec<String> {
        [
            run.lane.clone(),
            Some(run.title.clone()),
            run.latest_turn_title.clone(),
            Some(run.project_name.clone()),
        ]
        .into_iter()
        .flatten()
        .map(|name| name.to_lowercase())
        .collect()
    };
    let candidates: Vec<&LiveRun> = runs.iter().filter(|run| steerable(run)).collect();
    let exact: Vec<&LiveRun> = candidates
        .iter()
        .copied()
        .filter(|run| names(run).iter().any(|name| *name == needle))
        .collect();
    let found = if exact.is_empty() {
        candidates
            .into_iter()
            .filter(|run| names(run).iter().any(|name| name.contains(&needle)))
            .collect()
    } else {
        exact
    };
    match found.as_slice() {
        [] => SteerMatch::None,
        [one] => SteerMatch::One((*one).clone()),
        many => SteerMatch::Several(many.iter().map(|run| (*run).clone()).collect()),
    }
}

fn run_name(run: &LiveRun) -> String {
    match run.lane.as_deref() {
        Some(lane) => format!("the {lane} lane ({})", run.project_name),
        None => format!("\"{}\" ({})", run.title, run.project_name),
    }
}

/// Steer the run `request` names, and say what happened.
pub async fn steer_live_run(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    request: &SteerRequest,
) -> String {
    use crate::magician_v2::execution::coding_engine::{
        coding_control_registry, scoped_control_key, CodingControlAction,
    };
    let root = service
        .workspace()
        .scope_root(&scope.principal(), &scope.workspace());
    let projects = read_vibedev_projects(&root);
    let pending = service.pending_user_requests(scope).await;
    let requests = OwnerRequests::from_requests(pending.iter());
    let items = match service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &project_task_ids(&projects, &requests.task_ids()),
            ArchivedFilter::Exclude,
        )
        .await
    {
        Ok(items) => items,
        Err(error) => return format!("I couldn't read the VibeDev runs: {error}"),
    };
    let runs = live_runs(
        &projects,
        &items,
        &requests,
        chrono::Utc::now().timestamp_millis(),
        0,
    );
    let run = match match_live_run(&request.target, &runs) {
        SteerMatch::One(run) => run,
        SteerMatch::None => {
            let live: Vec<String> = runs
                .iter()
                .filter(|run| steerable(run))
                .map(run_name)
                .collect();
            return if live.is_empty() {
                format!("No VibeDev run is going right now, so there is nothing called \"{}\" to steer.", request.target)
            } else {
                format!(
                    "No running VibeDev run matches \"{}\". Running now: {}.",
                    request.target,
                    live.join(", ")
                )
            };
        },
        SteerMatch::Several(runs) => {
            let names: Vec<String> = runs.iter().map(run_name).collect();
            return format!(
                "\"{}\" matches several runs: {}. Say which one.",
                request.target,
                names.join(", ")
            );
        },
    };
    let key = scoped_control_key(&scope.principal(), &scope.workspace(), &run.live_task_id);
    match coding_control_registry()
        .control(&key, CodingControlAction::Steer, Some(&request.message))
        .await
    {
        Ok(true) => format!("Told {}: {}", run_name(&run), request.message),
        Ok(false) => format!(
            "{} is between turns, so the message did not reach it. Open it in VibeDev to send a follow-up.",
            run_name(&run)
        ),
        Err(error) => format!("Steering {} failed: {error}", run_name(&run)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, title: &str, lane: Option<&str>, state: LiveRunState) -> LiveRun {
        LiveRun {
            run_id: id.to_string(),
            live_task_id: id.to_string(),
            project_id: "p".to_string(),
            project_name: "Shop".to_string(),
            title: title.to_string(),
            latest_turn_title: None,
            state,
            status: "running".to_string(),
            step: None,
            summary: None,
            completion_kind: None,
            open_items: Vec::new(),
            turns: 1,
            started_at: "2026-10-13T00:00:00Z".to_string(),
            cost_usd: None,
            approval_id: None,
            approval_respond: None,
            approval_task_id: None,
            waiting_kind: None,
            notice: None,
            crew_id: lane.map(|_| "crew_1".to_string()),
            lane: lane.map(str::to_string),
            updated_at: "2026-10-13T00:00:00Z".to_string(),
            last_progress_at: None,
        }
    }

    #[test]
    fn steer_prompts_parse_both_shapes() {
        assert_eq!(
            parse_steer_prompt("tell the API lane to use Postgres"),
            Some(SteerRequest {
                target: "API".to_string(),
                message: "use Postgres".to_string()
            })
        );
        assert_eq!(
            parse_steer_prompt("API: use Postgres, not SQLite"),
            Some(SteerRequest {
                target: "API".to_string(),
                message: "use Postgres, not SQLite".to_string()
            })
        );
        assert_eq!(
            parse_steer_prompt("Tell the login run to stop at the form"),
            Some(SteerRequest {
                target: "login".to_string(),
                message: "stop at the form".to_string()
            })
        );
        assert_eq!(parse_steer_prompt("tell everyone"), None);
        assert_eq!(parse_steer_prompt(": nothing"), None);
        // Multi-byte text near the front must not panic.
        assert_eq!(
            parse_steer_prompt("the \u{201c}API\u{201d} lane to use Postgres"),
            Some(SteerRequest {
                target: "\u{201c}API\u{201d}".to_string(),
                message: "use Postgres".to_string(),
            })
        );
        assert_eq!(parse_steer_prompt("API\u{2014}use x"), None);
        assert_eq!(parse_steer_prompt("t\u{e9}l\u{e9}"), None);
    }

    #[test]
    fn a_target_matches_one_live_run_or_says_it_is_ambiguous() {
        let runs = vec![
            run("a", "Build the API", Some("API"), LiveRunState::Working),
            run("b", "Build the UI", Some("UI"), LiveRunState::NeedsYou),
            run("c", "Fix API docs", None, LiveRunState::Working),
            run("d", "Old API work", Some("API v1"), LiveRunState::Done),
        ];
        // An exact lane name wins over the run whose title merely contains it.
        assert!(matches!(match_live_run("api", &runs), SteerMatch::One(run) if run.run_id == "a"));
        assert!(matches!(match_live_run("UI", &runs), SteerMatch::One(run) if run.run_id == "b"));
        // A partial name that fits several live runs is ambiguous; ended runs never match.
        assert!(
            matches!(match_live_run("build", &runs), SteerMatch::Several(found) if found.len() == 2)
        );
        assert_eq!(match_live_run("v1", &runs), SteerMatch::None);
        assert_eq!(match_live_run("payments", &runs), SteerMatch::None);
    }
}
