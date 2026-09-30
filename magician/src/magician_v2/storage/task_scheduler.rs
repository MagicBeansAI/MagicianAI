//! TaskSchedulerService -- cron and one-time task scheduling.
//!
//! Manages a registry of tasks with cron expressions, tracks last/next fire
//! times, and emits due task IDs on each `tick()`.  Designed to be called
//! periodically from the WakeUpQueue watcher or a dedicated timer loop.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use tracing::warn;

use super::task_models::{Task, TaskScheduleKind};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A single registered scheduled task.
#[derive(Debug, Clone)]
pub struct TaskSchedulerEntry {
    pub task_id: String,
    pub cron_expression: String,
    pub one_shot: bool,
    pub last_fired: Option<DateTime<Utc>>,
    pub next_fire: Option<DateTime<Utc>>,
}

/// One concrete schedule occurrence waiting for durable execution acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSchedulerFire {
    pub task_id: String,
    pub scheduled_at: DateTime<Utc>,
}

/// Internal state for the scheduler.
#[derive(Debug, Default)]
struct TaskSchedulerState {
    entries: HashMap<String, TaskSchedulerEntry>,
    last_tick: Option<DateTime<Utc>>,
}

/// Thread-safe service that tracks cron and one-time scheduled tasks.
///
/// Register tasks with their cron expressions, then call [`tick`] periodically
/// to collect and advance the ones that are due.
#[derive(Debug, Default)]
pub struct TaskSchedulerService {
    state: RwLock<TaskSchedulerState>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize a cron expression to the 7-field format expected by the `cron`
/// crate: `sec min hour dom month dow year`.
///
/// Accepts:
///   - 5-field (standard):  `min hour dom month dow`       → prepends `0`, appends `*`
///   - 6-field:             `sec min hour dom month dow`    → appends `*`
///   - 7-field (native):    passed through as-is
///
/// For 5-field (standard POSIX) cron, the day-of-week field is translated from
/// standard numbering (0=Sun, 1=Mon, ..., 6=Sat, 7=Sun) to the `cron` crate's
/// numbering (1=Sun, 2=Mon, ..., 7=Sat).
pub fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => {
            let dow = translate_standard_dow(fields[4]);
            format!(
                "0 {} {} {} {} {} *",
                fields[0], fields[1], fields[2], fields[3], dow
            )
        },
        6 => format!("{expr} *"),
        7 => expr.to_string(),
        other => {
            warn!(
                "[CRON] Unexpected field count {} in cron expression '{}'; \
                 expected 5 (POSIX), 6, or 7 fields — passing through as-is",
                other, expr
            );
            expr.to_string()
        },
    }
}

/// Translate a standard POSIX cron day-of-week field to the `cron` crate's
/// numbering. Standard: 0=Sun,1=Mon,...,6=Sat,7=Sun. Crate: 1=Sun,...,7=Sat.
///
/// Handles: `*`, `?`, single values, ranges (`a-b`), lists (`a,b`), steps
/// (`*/n`, `a-b/n`), and named days (SUN, MON, etc. — passed through).
fn translate_standard_dow(field: &str) -> String {
    if field == "*" || field == "?" {
        return field.to_string();
    }
    field
        .split(',')
        .map(|segment| {
            let (base, step) = match segment.split_once('/') {
                Some((b, s)) => (b, Some(s)),
                None => (segment, None),
            };

            if base == "*" || base == "?" {
                let base_str = base.to_string();
                match step {
                    Some(s) => format!("{base_str}/{s}"),
                    None => base_str,
                }
            } else if let Some((start, end)) = base.split_once('-') {
                match (dow_std_to_crate(start), dow_std_to_crate(end)) {
                    (Some(s), Some(e)) if s <= e => {
                        let range = format!("{s}-{e}");
                        match step {
                            Some(st) => format!("{range}/{st}"),
                            None => range,
                        }
                    },
                    (Some(s), Some(e)) => {
                        // Wrapping range (e.g. Fri-Sun → 6,7,1): expand to list,
                        // applying the step during expansion.
                        let step_val: u8 = step.and_then(|s| s.parse().ok()).unwrap_or(1);
                        let mut vals = Vec::new();
                        let mut v = s;
                        let mut count: u8 = 0;
                        loop {
                            if count.is_multiple_of(step_val) {
                                vals.push(v.to_string());
                            }
                            if v == e {
                                break;
                            }
                            v = if v == 7 { 1 } else { v + 1 };
                            count += 1;
                        }
                        vals.join(",")
                    },
                    _ => {
                        // Named days — pass through with step
                        let base_str = base.to_string();
                        match step {
                            Some(s) => format!("{base_str}/{s}"),
                            None => base_str,
                        }
                    },
                }
            } else {
                let translated = match dow_std_to_crate(base) {
                    Some(n) => n.to_string(),
                    None => base.to_string(), // named day
                };
                match step {
                    Some(s) => format!("{translated}/{s}"),
                    None => translated,
                }
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Convert a single standard POSIX dow value (0-7) to the cron crate value (1-7).
/// Returns `None` for non-numeric (named days like SUN, MON).
fn dow_std_to_crate(val: &str) -> Option<u8> {
    let n: u8 = val.trim().parse().ok()?;
    Some((n % 7) + 1)
}

/// Check whether a 7-field normalized cron expression has both DoM and DoW as
/// non-wildcard. POSIX cron uses OR semantics in this case but the `cron` crate
/// uses AND.
fn has_restricted_dom_and_dow(normalized: &str) -> bool {
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    if fields.len() < 6 {
        return false;
    }
    // fields: sec(0) min(1) hour(2) dom(3) month(4) dow(5) [year(6)]
    let dom = fields[3];
    let dow = fields[5];
    let is_wildcard = |f: &str| f == "*" || f == "?";
    !is_wildcard(dom) && !is_wildcard(dow)
}

/// Split a 7-field normalized cron expression into two: one with DoW=*, one
/// with DoM=*. This restores POSIX OR semantics for the `cron` crate which
/// uses AND when both are restricted.
fn split_dom_dow(normalized: &str) -> (String, String) {
    let fields: Vec<&str> = normalized.split_whitespace().collect();
    // DoM-only: replace dow (field 5) with *
    let dom_only = fields
        .iter()
        .enumerate()
        .map(|(i, f)| if i == 5 { "*" } else { f })
        .collect::<Vec<_>>()
        .join(" ");
    // DoW-only: replace dom (field 3) with *
    let dow_only = fields
        .iter()
        .enumerate()
        .map(|(i, f)| if i == 3 { "*" } else { f })
        .collect::<Vec<_>>()
        .join(" ");
    (dom_only, dow_only)
}

/// Compute next fire time with POSIX OR semantics.
///
/// When both day-of-month and day-of-week are non-wildcard, the schedule
/// fires when **either** matches (not both). The expression is split into
/// two sub-expressions (one DoW=\*, one DoM=\*) and the earlier next-fire
/// time is returned.
///
/// Generic over timezone so callers with `DateTime<Utc>`, `DateTime<FixedOffset>`,
/// or `DateTime<Tz>` can all use this directly.
pub fn posix_or_cron_next<Tz: chrono::TimeZone>(
    normalized: &str,
    after: &DateTime<Tz>,
) -> Option<DateTime<Utc>>
where
    Tz::Offset: std::fmt::Display,
{
    if has_restricted_dom_and_dow(normalized) {
        let (dom_only, dow_only) = split_dom_dow(normalized);
        let a = CronSchedule::from_str(&dom_only)
            .ok()
            .and_then(|s| s.after(after).next().map(|dt| dt.with_timezone(&Utc)));
        let b = CronSchedule::from_str(&dow_only)
            .ok()
            .and_then(|s| s.after(after).next().map(|dt| dt.with_timezone(&Utc)));
        match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (Some(x), None) => Some(x),
            (None, Some(y)) => Some(y),
            (None, None) => None,
        }
    } else {
        CronSchedule::from_str(normalized)
            .ok()
            .and_then(|s| s.after(after).next().map(|dt| dt.with_timezone(&Utc)))
    }
}

/// Parse a cron expression and compute the next fire time after `after`.
///
/// Normalizes the expression and delegates to [`posix_or_cron_next`] for
/// POSIX OR semantics.
/// Returns `None` if the expression is invalid or has no future occurrences.
fn next_fire_after(cron_expression: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let normalized = normalize_cron(cron_expression);
    posix_or_cron_next(&normalized, &after)
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl TaskSchedulerService {
    /// Create a new, empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or re-register) a task with the given cron expression.
    ///
    /// The cron expression is parsed immediately and `next_fire` is computed
    /// relative to `Utc::now()`.  Returns `Err` if the expression is invalid.
    pub fn register_task(
        &self,
        task_id: impl Into<String>,
        cron_expression: impl Into<String>,
    ) -> Result<(), String> {
        let task_id = task_id.into();
        let raw_expression = cron_expression.into();
        let cron_expression = normalize_cron(&raw_expression);

        // Validate the (normalized) cron expression eagerly.
        CronSchedule::from_str(&cron_expression).map_err(|e| {
            format!(
                "invalid cron expression '{}' (normalized: '{}'): {}",
                raw_expression, cron_expression, e
            )
        })?;

        let mut state = self.state.write().expect("TaskSchedulerState poisoned");
        if state
            .entries
            .get(&task_id)
            .is_some_and(|entry| !entry.one_shot && entry.cron_expression == cron_expression)
        {
            return Ok(());
        }

        let now = Utc::now();
        let next_fire = next_fire_after(&cron_expression, now);

        let entry = TaskSchedulerEntry {
            task_id: task_id.clone(),
            cron_expression,
            one_shot: false,
            last_fired: None,
            next_fire,
        };

        state.entries.insert(task_id, entry);
        Ok(())
    }

    /// Register a durable one-time task. A due one-shot remains due until the
    /// caller acknowledges durable execution acceptance; dependency or
    /// persistence failures therefore retry instead of losing the reminder.
    pub fn register_once_task(
        &self,
        task_id: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Result<(), String> {
        let task_id = task_id.into();
        if task_id.trim().is_empty() {
            return Err("one-time task id must not be empty".to_string());
        }
        let mut state = self.state.write().expect("TaskSchedulerState poisoned");
        if state
            .entries
            .get(&task_id)
            .is_some_and(|entry| entry.one_shot && entry.next_fire == Some(at))
        {
            return Ok(());
        }
        let entry = TaskSchedulerEntry {
            task_id: task_id.clone(),
            cron_expression: String::new(),
            one_shot: true,
            last_fired: None,
            next_fire: Some(at),
        };
        state.entries.insert(task_id, entry);
        Ok(())
    }

    /// Remove a task from the scheduler.
    pub fn unregister_task(&self, task_id: &str) {
        let mut state = self.state.write().expect("TaskSchedulerState poisoned");
        state.entries.remove(task_id);
    }

    /// Return the IDs of all tasks whose `next_fire <= now`.
    ///
    /// This is a read-only check -- it does **not** advance `last_fired` or
    /// recompute `next_fire`.  Use [`tick`] for the full advance cycle.
    pub fn collect_due_tasks(&self, now: DateTime<Utc>) -> Vec<String> {
        let state = self.state.read().expect("TaskSchedulerState poisoned");
        state
            .entries
            .values()
            .filter(|e| matches!(e.next_fire, Some(nf) if nf <= now))
            .map(|e| e.task_id.clone())
            .collect()
    }

    /// Collect concrete due occurrences without consuming them.
    ///
    /// A fire remains due until [`Self::acknowledge_fire`] is called after the
    /// execution record has been durably accepted.
    pub fn tick(&self, now: DateTime<Utc>) -> Vec<TaskSchedulerFire> {
        let mut state = self.state.write().expect("TaskSchedulerState poisoned");
        state.last_tick = Some(now);

        state
            .entries
            .values()
            .filter(|e| matches!(e.next_fire, Some(nf) if nf <= now))
            .filter_map(|entry| {
                entry.next_fire.map(|scheduled_at| TaskSchedulerFire {
                    task_id: entry.task_id.clone(),
                    scheduled_at,
                })
            })
            .collect()
    }

    /// Consume one occurrence after durable execution acceptance.
    ///
    /// Returns `false` for a stale/duplicate acknowledgement. One-time entries
    /// are retired here; cron entries advance from the accepted occurrence.
    pub fn acknowledge_fire(
        &self,
        task_id: &str,
        scheduled_at: DateTime<Utc>,
        accepted_at: DateTime<Utc>,
    ) -> bool {
        let mut state = self.state.write().expect("TaskSchedulerState poisoned");
        let Some(entry) = state.entries.get(task_id) else {
            return false;
        };
        if entry.next_fire != Some(scheduled_at) {
            return false;
        }
        if entry.one_shot {
            state.entries.remove(task_id);
            return true;
        }
        let entry = state.entries.get_mut(task_id).expect("entry checked above");
        entry.last_fired = Some(accepted_at);
        entry.next_fire = next_fire_after(&entry.cron_expression, accepted_at);
        true
    }

    /// Startup reconciliation: scan a slice of [`Task`]s and register cron and
    /// one-time schedules. Interval and event schedules remain unsupported by
    /// this polling service.
    pub fn init(&self, tasks: &[Task]) {
        for task in tasks {
            if let Some(ref schedule) = task.schedule {
                let result = match &schedule.kind {
                    TaskScheduleKind::Cron { expression, .. } => {
                        self.register_task(&task.id, expression)
                    },
                    TaskScheduleKind::Once { at } => self.register_once_task(&task.id, *at),
                    TaskScheduleKind::Interval { .. } | TaskScheduleKind::OnEvent { .. } => {
                        continue;
                    },
                };
                if let Err(e) = result {
                    warn!(
                        task_id = %task.id,
                        error = %e,
                        "TaskSchedulerService::init: failed to register task"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::pipeline::agent::AgentScheduleKind;
    use crate::magician_v2::storage::task_models::{Task, TaskCreatedBy, TaskSchedule, TaskStatus};
    use chrono::{Datelike, Duration, Timelike};

    /// Helper: build a minimal `Task` suitable for unit tests.
    fn make_task(id: &str, cron: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            principal: "test-principal".to_string(),
            workspace: "test-ws".to_string(),
            ui_thread_id: crate::magician_v2::storage::task_models::default_ui_thread_id(),
            title: format!("Task {id}"),
            description: format!("Description for task {id}"),
            status: TaskStatus::Ready,
            priority: None,
            due_date: None,
            tags: vec![],
            agent_id: "personal-assistant".to_string(),
            schedule: cron.map(|expr| TaskSchedule {
                kind: AgentScheduleKind::Cron {
                    expression: expr.to_string(),
                    timezone: None,
                },
                timezone: None,
                missed_fire_policy: Default::default(),
                concurrent_execution_policy: Default::default(),
                execution_history_retention: None,
                max_runs: None,
                paused: None,
            }),
            created_by: TaskCreatedBy::default(),
            linked_task_ids: vec![],
            depends_on: vec![],
            approved: true,
            has_plan: false,
            active_root_execution_id: None,
            latest_root_execution_id: None,
            last_completed_root_execution_id: None,
            error_message: None,
            retry_at: None,
            current_step: None,
            progress: None,
            completion_summary: None,
            completion_outcome: None,
            completion_artifact_names: None,
            auto_surface_policy: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// Cron expression that fires every second (useful for testing "always due").
    /// The cron crate uses 7-field format: sec min hour day month dow year
    const EVERY_SECOND: &str = "* * * * * * *";

    /// Cron expression that fires at midnight on Jan 1, 2099 — practically never.
    const FAR_FUTURE: &str = "0 0 0 1 1 * 2099";

    #[test]
    fn test_register_and_tick_fires_due_task() {
        let svc = TaskSchedulerService::new();
        // Register a task that fires every second.
        svc.register_task("task-1", EVERY_SECOND).unwrap();

        // Tick at a time slightly in the future (ensures the next_fire is past).
        let now = Utc::now() + Duration::seconds(2);
        let due = svc.tick(now);
        let fire = due
            .iter()
            .find(|fire| fire.task_id == "task-1")
            .expect("task-1 should be due after tick");

        // Merely observing a due occurrence cannot consume it.
        let state = svc.state.read().unwrap();
        let entry = state.entries.get("task-1").unwrap();
        assert_eq!(entry.last_fired, None);
        let scheduled_at = fire.scheduled_at;
        drop(state);

        assert!(svc.acknowledge_fire("task-1", scheduled_at, now));
        let state = svc.state.read().unwrap();
        let entry = state.entries.get("task-1").unwrap();
        assert_eq!(entry.last_fired, Some(now));
        assert!(entry.next_fire > Some(now));
    }

    #[test]
    fn test_tick_does_not_fire_future_task() {
        let svc = TaskSchedulerService::new();
        svc.register_task("future-task", FAR_FUTURE).unwrap();

        let now = Utc::now();
        let due = svc.tick(now);
        assert!(
            due.is_empty(),
            "far-future task should not be due, got: {:?}",
            due
        );
    }

    #[test]
    fn one_time_task_remains_due_until_dispatch_unregisters_it() {
        let svc = TaskSchedulerService::new();
        let at = Utc::now() + Duration::minutes(10);
        svc.register_once_task("reminder-1", at).unwrap();

        assert!(svc.tick(at - Duration::seconds(1)).is_empty());
        assert_eq!(
            svc.tick(at),
            vec![TaskSchedulerFire {
                task_id: "reminder-1".to_string(),
                scheduled_at: at,
            }]
        );
        // A dependency/persistence failure after tick must not consume the only
        // fire. The dispatch path acknowledges only after durable acceptance.
        assert_eq!(svc.tick(at + Duration::seconds(1))[0].scheduled_at, at);
        assert!(svc.acknowledge_fire("reminder-1", at, at));
        assert!(svc.tick(at + Duration::seconds(2)).is_empty());
    }

    #[test]
    fn duplicate_wake_is_idempotent_until_acceptance() {
        let svc = TaskSchedulerService::new();
        let at = Utc::now() - Duration::seconds(1);
        svc.register_once_task("reminder-duplicate", at).unwrap();

        let first = svc.tick(Utc::now());
        let duplicate = svc.tick(Utc::now() + Duration::seconds(1));
        assert_eq!(first, duplicate);
        assert!(svc.acknowledge_fire("reminder-duplicate", at, at));
        assert!(!svc.acknowledge_fire("reminder-duplicate", at, at));
    }

    #[test]
    fn newly_registered_reminder_is_discovered_on_next_tick() {
        let svc = TaskSchedulerService::new();
        let now = Utc::now();
        assert!(svc.tick(now).is_empty());

        svc.register_once_task("new-reminder", now).unwrap();
        assert_eq!(svc.tick(now)[0].task_id, "new-reminder");
    }

    #[test]
    fn test_unregister_removes_task() {
        let svc = TaskSchedulerService::new();
        svc.register_task("task-x", EVERY_SECOND).unwrap();

        svc.unregister_task("task-x");

        let now = Utc::now() + Duration::seconds(2);
        let due = svc.tick(now);
        assert!(
            due.is_empty(),
            "unregistered task should not appear in tick results"
        );

        // Also verify the entry is gone from the state map.
        let state = svc.state.read().unwrap();
        assert!(
            !state.entries.contains_key("task-x"),
            "task-x should be removed from entries"
        );
    }

    #[test]
    fn test_init_rebuilds_from_tasks() {
        let svc = TaskSchedulerService::new();

        let tasks = vec![
            make_task("cron-task-1", Some(EVERY_SECOND)),
            make_task("cron-task-2", Some(FAR_FUTURE)),
            make_task("no-schedule", None), // should be skipped
        ];

        svc.init(&tasks);

        let state = svc.state.read().unwrap();
        assert!(
            state.entries.contains_key("cron-task-1"),
            "cron-task-1 should be registered"
        );
        assert!(
            state.entries.contains_key("cron-task-2"),
            "cron-task-2 should be registered"
        );
        assert!(
            !state.entries.contains_key("no-schedule"),
            "task without schedule should not be registered"
        );
        assert_eq!(
            state.entries.len(),
            2,
            "exactly 2 cron tasks should be registered"
        );
    }

    #[test]
    fn test_normalize_cron_5_field() {
        // Standard 5-field cron: "min hour dom month dow"
        // dow 1-5 (Mon-Fri) → crate 2-6 (Mon-Fri)
        assert_eq!(normalize_cron("0 6 * * 1-5"), "0 0 6 * * 2-6 *");
        assert_eq!(normalize_cron("*/15 * * * *"), "0 */15 * * * * *");
    }

    #[test]
    fn test_normalize_cron_5_field_sunday() {
        // Standard dow 0 (Sun) → crate 1 (Sun)
        assert_eq!(normalize_cron("0 9 * * 0"), "0 0 9 * * 1 *");
        // Standard dow 7 (Sun alias) → crate 1 (Sun)
        assert_eq!(normalize_cron("0 9 * * 7"), "0 0 9 * * 1 *");
    }

    #[test]
    fn test_normalize_cron_5_field_dow_list() {
        // Standard dow 0,6 (Sun,Sat) → crate 1,7 (Sun,Sat)
        assert_eq!(normalize_cron("0 9 * * 0,6"), "0 0 9 * * 1,7 *");
    }

    #[test]
    fn test_normalize_cron_5_field_wrapping_range() {
        // Standard dow 5-0 (Fri-Sun) → wrapping expands to crate 6,7,1
        assert_eq!(normalize_cron("0 9 * * 5-0"), "0 0 9 * * 6,7,1 *");
    }

    #[test]
    fn test_normalize_cron_5_field_wrapping_range_with_step() {
        // Standard dow 5-1/2 (Fri-Mon, every 2nd) → crate values 6(Fri),1(Mon)
        // Fri=6, skip Sat=7, Sun=1, skip Mon=2 → only 6,1
        assert_eq!(normalize_cron("0 9 * * 5-1/2"), "0 0 9 * * 6,1 *");
    }

    #[test]
    fn test_normalize_cron_6_field() {
        // 6-field (already cron-crate format): no dow translation
        assert_eq!(normalize_cron("0 30 6 * * 1-5"), "0 30 6 * * 1-5 *");
    }

    #[test]
    fn test_normalize_cron_7_field_passthrough() {
        // 7-field: already native, passed through
        assert_eq!(normalize_cron("0 0 6 * * 1-5 *"), "0 0 6 * * 1-5 *");
    }

    #[test]
    fn test_register_standard_5_field_cron() {
        let svc = TaskSchedulerService::new();
        // "At 06:00 on weekdays" — standard 5-field cron
        svc.register_task("weekday-morning", "0 6 * * 1-5")
            .expect("5-field cron should be accepted");

        let state = svc.state.read().unwrap();
        let entry = state.entries.get("weekday-morning").unwrap();
        assert!(entry.next_fire.is_some(), "next fire should be computed");
    }

    // ── POSIX OR semantics for DoM + DoW ──────────────────────────

    #[test]
    fn test_posix_or_dom_and_dow_fires_on_either() {
        use chrono::NaiveDate;

        // "At 12:00 on day 15 of the month OR on Mondays"
        // Standard 5-field: `0 12 15 * 1`
        // 2025-01-13 (Monday) should match via DoW even though it's not the 15th.
        let after = NaiveDate::from_ymd_opt(2025, 1, 12)
            .unwrap()
            .and_hms_opt(23, 0, 0)
            .unwrap()
            .and_utc();
        let next = next_fire_after("0 12 15 * 1", after);
        assert!(next.is_some(), "Should find a next fire");
        let next = next.unwrap();
        // Should fire on Mon Jan 13 at 12:00, NOT wait until Wed Jan 15
        assert_eq!(next.day(), 13, "Should fire on Monday the 13th (DoW match)");
        assert_eq!(next.hour(), 12);
    }

    #[test]
    fn test_posix_or_dom_only_wildcard_dow_no_split() {
        // "At 12:00 on the 15th" — only DoM restricted, DoW is *
        // This should NOT trigger the OR split.
        use chrono::NaiveDate;
        let after = NaiveDate::from_ymd_opt(2025, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let next = next_fire_after("0 12 15 * *", after);
        assert!(next.is_some());
        assert_eq!(next.unwrap().day(), 15);
    }

    #[test]
    fn test_posix_or_dom_fires_before_dow() {
        use chrono::NaiveDate;

        // "At 12:00 on day 14 of the month OR on Fridays"
        // After 2025-01-12 23:00:
        //   DoM match: Jan 14 at 12:00
        //   DoW match: Jan 17 at 12:00 (next Friday)
        // Should pick Jan 14 (earlier).
        let after = NaiveDate::from_ymd_opt(2025, 1, 12)
            .unwrap()
            .and_hms_opt(23, 0, 0)
            .unwrap()
            .and_utc();
        let next = next_fire_after("0 12 14 * 5", after);
        assert!(next.is_some());
        assert_eq!(
            next.unwrap().day(),
            14,
            "DoM should win when it fires sooner"
        );
    }
}
