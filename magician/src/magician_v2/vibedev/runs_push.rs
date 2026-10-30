//! Keep phones' coding-runs surfaces current while the app is closed.
//!
//! Since the "Magician at work" summary (`work_live`), the same routes carry
//! the owner's whole long-running work — coding runs, crews and Work tasks —
//! and each followed item gets its own updates on its `work_item` route.
//!
//! The iPhone coding-runs Live Activity and the Android ongoing notification
//! show the live-runs summary (`vibedev::live_runs`). While the app is open it
//! refreshes them itself; this pusher covers the rest: when a run-state event
//! arrives it marks the summaries dirty, and at most every [`PUSH_TICK`] it
//! recomputes each scope that has a coding-runs route and sends the summary
//! through `MobilePushDispatcher::deliver_coding_runs` — but only when it
//! changed, so a busy run does not stream pushes. An active summary is re-sent
//! every [`HEARTBEAT`] so the lock screen is not marked stale through a long
//! step, a device whose send failed is retried alone with backoff (up to
//! [`MAX_RETRY_BACKOFF`] apart, until it lands, the summary changes or the
//! route is gone), and after a restart every scope gets one summary so a
//! surface left from before is ended.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::live_runs::{LiveRun, LiveRunState};
use crate::magician_v2::artifact_v2::ScopeRef;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::mobile_push::{
    CodingRunsPush, MobilePushDispatcher, PushRegistration, WorkItemPush, WorkSummaryFields,
    WorkSummaryNext,
};
use crate::magician_v2::realtime_events::RuntimeTransportEvent;
use crate::magician_v2::work_live::{self, WorkItem, WorkLive, WorkState};

/// How often dirty summaries are recomputed.
const PUSH_TICK: Duration = Duration::from_secs(4);
/// Recompute even without events this often, so a missed event heals.
const IDLE_REFRESH: Duration = Duration::from_secs(120);
/// Re-send an unchanged active summary this often; the iPhone marks an
/// activity stale 20 minutes after its last update.
const HEARTBEAT: Duration = Duration::from_secs(10 * 60);
/// The longest wait before retrying a device whose send failed. Retries do
/// not give up: a lost `end` would leave "1 working" on a lock screen, and a
/// provider error that never clears costs one request per device this often.
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// What the pusher remembers about one scope.
struct ScopeState {
    /// The last summary sent, and when.
    last: CodingRunsPush,
    sent_at: Instant,
    /// Devices that summary has not reached yet.
    retry: Option<Retry>,
}

struct Retry {
    registration_ids: HashSet<String>,
    attempts: u32,
    at: Instant,
}

fn retry_after(attempts: u32) -> Duration {
    (PUSH_TICK * 2_u32.saturating_pow(attempts.min(8))).min(MAX_RETRY_BACKOFF)
}

/// The retry left after a send to some devices failed on its `attempts`th try.
fn next_retry(failed: HashSet<String>, attempts: u32, now: Instant) -> Option<Retry> {
    (!failed.is_empty()).then(|| Retry {
        registration_ids: failed,
        attempts,
        at: now + retry_after(attempts),
    })
}

/// The retry after a changed summary was sent to every device and `failed`
/// missed. Devices the previous summary still owed carry over only when this
/// send could have skipped them: without `start` it sends no push-to-start,
/// while with it every route was targeted, so an owed one it reached is not
/// sent a second start. A carried retry keeps its deadline, so changes
/// arriving faster than its backoff cannot hold it back.
fn carry_retry(
    previous: Option<&Retry>,
    failed: HashSet<String>,
    start: bool,
    now: Instant,
) -> Option<Retry> {
    let carried = previous.filter(|_| !start);
    let mut owed = failed;
    if let Some(previous) = carried {
        owed.extend(previous.registration_ids.iter().cloned());
    }
    let mut retry = next_retry(owed, carried.map_or(1, |previous| previous.attempts), now)?;
    if let Some(previous) = carried {
        retry.at = retry.at.min(previous.at);
    }
    Some(retry)
}

/// What a retry re-sends: the last summary, still a start while runs are
/// going. The first send's start may be the very thing that failed, and a
/// push-to-start only reaches an iPhone that has no activity yet.
fn retry_summary(last: &CodingRunsPush) -> CodingRunsPush {
    CodingRunsPush {
        start: last.active,
        ..last.clone()
    }
}

/// The summary phones show, from a scope's live runs. `previous` is the last
/// summary delivered for the scope.
pub fn summarize(
    runs: &[LiveRun],
    previous: Option<&CodingRunsPush>,
    now_ms: i64,
) -> CodingRunsPush {
    let count = |states: &[LiveRunState]| {
        runs.iter()
            .filter(|run| states.contains(&run.state))
            .count() as u32
    };
    let working = count(&[LiveRunState::Working, LiveRunState::Paused]);
    let needs_you = count(&[LiveRunState::NeedsYou]);
    let top = runs
        .iter()
        .find(|run| run.state == LiveRunState::NeedsYou)
        .or_else(|| runs.iter().find(|run| run.state == LiveRunState::Working))
        .or_else(|| runs.first());
    let headline = top
        .map(|run| {
            let label = run
                .lane
                .as_ref()
                .map(|lane| format!("{lane} lane"))
                .unwrap_or_else(|| run.title.clone());
            if run.project_name.is_empty() {
                label
            } else {
                format!("{label} · {}", run.project_name)
            }
        })
        .unwrap_or_default();
    let detail = top
        .map(|run| match run.state {
            LiveRunState::NeedsYou => "Needs you".to_string(),
            _ => run
                .step
                .clone()
                .or_else(|| run.summary.clone())
                .unwrap_or_default(),
        })
        .unwrap_or_default();
    let active = working + needs_you > 0;
    let previously_active = previous.is_some_and(|previous| previous.active);
    let start = active && !previously_active;
    CodingRunsPush {
        working,
        needs_you,
        done: count(&[LiveRunState::Done]),
        failed: count(&[LiveRunState::Failed]),
        headline,
        detail,
        active,
        start,
        urgent: start || needs_you > previous.map_or(0, |previous| previous.needs_you),
        event_timestamp: now_ms,
        work: WorkSummaryFields::default(),
    }
}

/// The summary phones show, from a scope's work summary
/// (`work_live::scope_work_live`). `previous` is the last summary delivered.
pub fn summarize_work(
    live: &WorkLive,
    previous: Option<&CodingRunsPush>,
    now_ms: i64,
) -> CodingRunsPush {
    let counts = live.counts;
    let featured = live.featured.as_ref();
    let headline = featured.map(WorkItem::headline).unwrap_or_default();
    let detail = featured
        .map(|item| match item.state {
            WorkState::NeedsYou => "Needs you".to_string(),
            _ => item.step.clone().unwrap_or_default(),
        })
        .unwrap_or_default();
    let active = counts.working + counts.needs_you > 0;
    let previously_active = previous.is_some_and(|previous| previous.active);
    let start = active && !previously_active;
    let work = featured
        .map(|item| WorkSummaryFields {
            featured_id: item.id.clone(),
            featured_kind: item.kind.as_str().to_string(),
            featured_state: item.state.as_str().to_string(),
            featured_title: item.title.clone(),
            featured_context: item.context.clone(),
            featured_step: item.step.clone().unwrap_or_default(),
            action_kind: item
                .action
                .as_ref()
                .map(|action| action.kind.clone())
                .unwrap_or_default(),
            action_id: item
                .action
                .as_ref()
                .map(|action| action.id.clone())
                .unwrap_or_default(),
            deep_link: item.deep_link.clone(),
            more: live.more(),
            next: live
                .next
                .iter()
                .map(|next| WorkSummaryNext {
                    id: next.id.clone(),
                    kind: next.kind.as_str().to_string(),
                    state: next.state.as_str().to_string(),
                    title: next.title.clone(),
                })
                .collect(),
        })
        .unwrap_or_default();
    CodingRunsPush {
        working: counts.working,
        needs_you: counts.needs_you,
        done: counts.done,
        failed: counts.failed,
        headline,
        detail,
        active,
        start,
        urgent: start
            || counts.needs_you > previous.map_or(0, |previous| previous.needs_you)
            || counts.failed > previous.map_or(0, |previous| previous.failed),
        event_timestamp: now_ms,
        work,
    }
}

/// One followed item's update. `item` is `None` once it left the list, which
/// ends it with its last known state.
fn work_item_push(
    item_id: &str,
    item: Option<&WorkItem>,
    previous: Option<&WorkItemPush>,
    now_ms: i64,
) -> WorkItemPush {
    let Some(item) = item else {
        let mut ended = previous.cloned().unwrap_or_else(|| WorkItemPush {
            item_id: item_id.to_string(),
            kind: item_id.split(':').next().unwrap_or_default().to_string(),
            state: WorkState::Stopped.as_str().to_string(),
            title: String::new(),
            context: String::new(),
            step: String::new(),
            spent_usd: None,
            action_kind: String::new(),
            action_id: String::new(),
            deep_link: String::new(),
            ended: true,
            urgent: false,
            event_timestamp: now_ms,
        });
        ended.ended = true;
        ended.urgent = false;
        ended.event_timestamp = now_ms;
        return ended;
    };
    let state = item.state.as_str().to_string();
    let urgent = matches!(item.state, WorkState::NeedsYou | WorkState::Failed)
        && previous.is_none_or(|previous| previous.state != state);
    WorkItemPush {
        item_id: item.id.clone(),
        kind: item.kind.as_str().to_string(),
        state,
        title: item.title.clone(),
        context: item.context.clone(),
        step: item.step.clone().unwrap_or_default(),
        spent_usd: item.spent_usd,
        action_kind: item
            .action
            .as_ref()
            .map(|action| action.kind.clone())
            .unwrap_or_default(),
        action_id: item
            .action
            .as_ref()
            .map(|action| action.id.clone())
            .unwrap_or_default(),
        deep_link: item.deep_link.clone(),
        ended: item.state.is_ended(),
        urgent,
        event_timestamp: now_ms,
    }
}

/// Send each followed item's update to its `work_item` routes when it
/// changed (or its heartbeat is due). A route whose send failed is retried on
/// the next pass; an ended item's routes are dropped by the dispatcher.
async fn push_followed_items(
    dispatcher: &MobilePushDispatcher,
    principal: &str,
    workspace: &str,
    live: &WorkLive,
    sent: &mut HashMap<String, (WorkItemPush, Instant)>,
) {
    let routes = dispatcher
        .work_item_registrations(principal, workspace)
        .await;
    let route_ids: HashSet<String> = routes.iter().map(|route| route.id.clone()).collect();
    sent.retain(|id, _| route_ids.contains(id));
    let items: HashMap<&str, &WorkItem> = live
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut by_item: HashMap<String, Vec<PushRegistration>> = HashMap::new();
    for route in routes {
        if let Some(item_id) = route.task_id.clone() {
            by_item.entry(item_id).or_default().push(route);
        }
    }
    let now = Instant::now();
    let now_ms = chrono::Utc::now().timestamp_millis();
    for (item_id, routes) in by_item {
        let previous = routes
            .iter()
            .find_map(|route| sent.get(&route.id).map(|(push, _)| push.clone()));
        let push = work_item_push(
            &item_id,
            items.get(item_id.as_str()).copied(),
            previous.as_ref(),
            now_ms,
        );
        let due: Vec<PushRegistration> = routes
            .into_iter()
            .filter(|route| {
                sent.get(&route.id).is_none_or(|(last, at)| {
                    !last.same_content(&push)
                        || (!push.ended && now.duration_since(*at) >= HEARTBEAT)
                })
            })
            .collect();
        if due.is_empty() {
            continue;
        }
        let ids: Vec<String> = due.iter().map(|route| route.id.clone()).collect();
        let failed = dispatcher.deliver_work_item(due, &push).await;
        for id in ids {
            if failed.contains(&id) {
                continue;
            }
            if push.ended {
                sent.remove(&id);
            } else {
                sent.insert(id, (push.clone(), now));
            }
        }
    }
}

/// Whether an event may change a scope's live runs.
pub fn changes_runs(event: &RuntimeTransportEvent) -> bool {
    match event {
        RuntimeTransportEvent::ExecutionStarted { .. }
        | RuntimeTransportEvent::ExecutionCompleted { .. }
        | RuntimeTransportEvent::ExecutionFailed { .. }
        | RuntimeTransportEvent::ExecutionCancelled { .. }
        | RuntimeTransportEvent::HitlRequested { .. }
        | RuntimeTransportEvent::HitlResolved { .. } => true,
        RuntimeTransportEvent::AgentEvent { event } => matches!(
            event.event_type.as_str(),
            "coding.started"
                | "coding.completed"
                | "coding.failed"
                | "coding.queued"
                | "coding.agent_started"
                | "coding.agent_ended"
                | "coding.turn.started"
                | "coding.turn.finished"
                | "coding.approval_requested"
                | "coding.budget_exhausted"
        ),
        _ => false,
    }
}

async fn scope_work(resources: &AgentResources, scope: &ScopeRef) -> Option<WorkLive> {
    let service = resources.artifact_v2_service.as_ref()?;
    work_live::scope_work_live(service, scope).await.ok()
}

/// Whether to send `summary`, given the last delivered summary and its age.
/// A first look always sends: with nothing going it ends a lock-screen
/// surface left from before a restart (a phone showing nothing ignores it).
fn should_send(previous: Option<(&CodingRunsPush, Duration)>, summary: &CodingRunsPush) -> bool {
    match previous {
        None => true,
        Some((previous, age)) => {
            !previous.same_content(summary) || (summary.active && age >= HEARTBEAT)
        },
    }
}

/// Run the coding-runs pusher for the life of the process.
pub fn spawn_coding_runs_pusher(
    resources: Arc<AgentResources>,
    dispatcher: Arc<MobilePushDispatcher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let dirty = Arc::new(AtomicBool::new(true));
        if let Some(broadcaster) = resources.event_broadcaster.clone() {
            let dirty = Arc::clone(&dirty);
            tokio::spawn(async move {
                let mut events = broadcaster.subscribe();
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            if changes_runs(&event) {
                                dirty.store(true, Ordering::Relaxed);
                            }
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            dirty.store(true, Ordering::Relaxed);
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        let mut scopes: HashMap<(String, String), ScopeState> = HashMap::new();
        // Per `work_item` route: the last update it took, and when.
        let mut items_sent: HashMap<String, (WorkItemPush, Instant)> = HashMap::new();
        let mut ticker = tokio::time::interval(PUSH_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut since_refresh = Duration::ZERO;
        loop {
            ticker.tick().await;
            since_refresh += PUSH_TICK;
            let forced = since_refresh >= IDLE_REFRESH;
            let now = Instant::now();
            let retry_due = scopes
                .values()
                .any(|state| state.retry.as_ref().is_some_and(|retry| retry.at <= now));
            if !dirty.swap(false, Ordering::Relaxed) && !forced && !retry_due {
                continue;
            }
            if forced {
                since_refresh = Duration::ZERO;
            }
            let routed = dispatcher.coding_runs_scopes().await;
            // A scope whose routes are all gone is forgotten, retry included.
            let live: HashSet<&(String, String)> = routed.iter().collect();
            scopes.retain(|key, _| live.contains(key));
            for key in &routed {
                let (principal, workspace) = key;
                let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
                let Some(live) = scope_work(&resources, &scope).await else {
                    // Unreadable now: a due retry waits instead of making
                    // every tick due.
                    if let Some(retry) = scopes.get_mut(key).and_then(|state| state.retry.as_mut())
                    {
                        retry.at = retry.at.max(now + retry_after(retry.attempts));
                    }
                    continue;
                };
                push_followed_items(&dispatcher, principal, workspace, &live, &mut items_sent)
                    .await;
                let previous = scopes.get(key);
                let summary = summarize_work(
                    &live,
                    previous.map(|state| &state.last),
                    chrono::Utc::now().timestamp_millis(),
                );
                if should_send(
                    previous.map(|state| (&state.last, now.duration_since(state.sent_at))),
                    &summary,
                ) {
                    let failed: HashSet<String> = dispatcher
                        .deliver_coding_runs(principal, workspace, &summary, None)
                        .await
                        .into_iter()
                        .collect();
                    let retry = carry_retry(
                        previous.and_then(|state| state.retry.as_ref()),
                        failed,
                        summary.start,
                        Instant::now(),
                    );
                    scopes.insert(
                        key.clone(),
                        ScopeState {
                            last: summary,
                            sent_at: Instant::now(),
                            retry,
                        },
                    );
                    continue;
                }
                // Unchanged: re-send the last summary to the devices it has
                // not reached, when their retry is due.
                let Some(state) = scopes.get_mut(key) else {
                    continue;
                };
                let Some(retry) = state.retry.take() else {
                    continue;
                };
                if retry.at > now {
                    state.retry = Some(retry);
                    continue;
                }
                let failed = dispatcher
                    .deliver_coding_runs(
                        principal,
                        workspace,
                        &retry_summary(&state.last),
                        Some(&retry.registration_ids),
                    )
                    .await
                    .into_iter()
                    .collect();
                state.retry = next_retry(failed, retry.attempts + 1, Instant::now());
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(state: LiveRunState, lane: Option<&str>, step: Option<&str>) -> LiveRun {
        LiveRun {
            run_id: "r".to_string(),
            live_task_id: "r".to_string(),
            project_id: "p".to_string(),
            project_name: "Shop".to_string(),
            title: "Speed up search".to_string(),
            latest_turn_title: None,
            state,
            status: "running".to_string(),
            step: step.map(str::to_string),
            summary: None,
            completion_kind: None,
            open_items: Vec::new(),
            turns: 1,
            started_at: "2026-10-13T09:00:00Z".to_string(),
            cost_usd: None,
            approval_id: None,
            approval_respond: None,
            approval_task_id: None,
            waiting_kind: None,
            notice: None,
            crew_id: lane.map(|_| "crew".to_string()),
            lane: lane.map(str::to_string),
            updated_at: "2026-10-13T09:10:00Z".to_string(),
            last_progress_at: None,
        }
    }

    #[test]
    fn the_summary_leads_with_what_needs_the_owner() {
        let runs = vec![
            run(LiveRunState::NeedsYou, Some("API"), None),
            run(LiveRunState::Working, None, Some("Editing search.ts")),
            run(LiveRunState::Done, None, None),
        ];
        let summary = summarize(&runs, None, 5);
        assert_eq!(
            (summary.working, summary.needs_you, summary.done),
            (1, 1, 1)
        );
        assert_eq!(summary.headline, "API lane · Shop");
        assert_eq!(summary.detail, "Needs you");
        assert!(summary.active && summary.start && summary.urgent);
        let again = summarize(&runs, Some(&summary), 5);
        assert!(!again.start, "already active: update, not start");
        assert!(!again.urgent, "nothing new needs the owner");

        let working = summarize(
            &[run(LiveRunState::Working, None, Some("Editing search.ts"))],
            Some(&summary),
            5,
        );
        assert_eq!(working.headline, "Speed up search · Shop");
        assert_eq!(working.detail, "Editing search.ts");

        let idle = summarize(&[run(LiveRunState::Done, None, None)], Some(&summary), 5);
        assert!(!idle.active && !idle.start);
        assert_eq!(idle.done, 1);
    }

    #[test]
    fn a_failed_device_is_retried_with_backoff_until_it_lands() {
        let now = Instant::now();
        assert!(
            next_retry(HashSet::new(), 1, now).is_none(),
            "all delivered"
        );
        let failed: HashSet<String> = ["push_a".to_string()].into();
        let retry = next_retry(failed.clone(), 1, now).expect("retry");
        assert_eq!(retry.registration_ids.len(), 1);
        assert_eq!(retry.at, now + PUSH_TICK * 2);
        // Never given up: a lost end must still land after a long outage.
        let late = next_retry(failed, 40, now).expect("still retried");
        assert_eq!(late.at, now + MAX_RETRY_BACKOFF);
        // A retry stays a start while runs are going, and is never one after.
        let working = summarize(&[run(LiveRunState::Working, None, Some("a"))], None, 5);
        let moved = summarize(
            &[run(LiveRunState::Working, None, Some("b"))],
            Some(&working),
            9,
        );
        assert!(!moved.start && retry_summary(&moved).start);
        assert!(!retry_summary(&summarize(&[], Some(&moved), 9)).start);
    }

    #[test]
    fn a_changed_summary_carries_what_it_could_skip_and_keeps_the_deadline() {
        let now = Instant::now();
        let owed = Retry {
            registration_ids: ["start_row".to_string()].into(),
            attempts: 6,
            at: now + Duration::from_secs(3),
        };
        // Not a start: the owed push-to-start carries over, deadline kept.
        let carried = carry_retry(Some(&owed), HashSet::new(), false, now).expect("carried");
        assert!(carried.registration_ids.contains("start_row"));
        assert_eq!(carried.at, owed.at, "a busy run cannot push it back");
        // A start reached every route: nothing owed is sent a second start.
        assert!(carry_retry(Some(&owed), HashSet::new(), true, now).is_none());
        // What this send missed is still retried, from a short backoff.
        let fresh = carry_retry(None, ["fcm_row".to_string()].into(), true, now).expect("retry");
        assert_eq!(fresh.at, now + retry_after(1));
    }

    #[test]
    fn sends_on_change_on_heartbeat_and_ends_once_after_a_restart() {
        let working = summarize(&[run(LiveRunState::Working, None, Some("a"))], None, 5);
        let idle = summarize(&[], None, 5);
        // First look sends either way: idle ends what a restart left up.
        assert!(should_send(None, &working));
        assert!(should_send(None, &idle));
        // Unchanged and recent: nothing; unchanged but old: heartbeat.
        let same = summarize(
            &[run(LiveRunState::Working, None, Some("a"))],
            Some(&working),
            9,
        );
        assert!(!should_send(
            Some((&working, Duration::from_secs(30))),
            &same
        ));
        assert!(should_send(Some((&working, HEARTBEAT)), &same));
        // An idle scope does not heartbeat.
        assert!(!should_send(Some((&idle, HEARTBEAT)), &idle));
        // A changed step is sent.
        let moved = summarize(
            &[run(LiveRunState::Working, None, Some("b"))],
            Some(&working),
            9,
        );
        assert!(should_send(
            Some((&working, Duration::from_secs(5))),
            &moved
        ));
    }
}
