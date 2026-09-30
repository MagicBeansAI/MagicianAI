//! The six task filter lanes, defined once, on the server.
//!
//! These lanes (`all`, `inbox`, `today`, `overdue`, `running`, `completed`)
//! used to be computed on each client over server-paged data, which made a
//! lane a search of the loaded page rather than of the corpus: a matching
//! task past the first page was simply invisible to the reader. The
//! predicate lives here so the server can answer a lane over the whole pool.
//!
//! `today` belongs to the READER, not to the server: the server cannot know
//! the reader's timezone, so the date lanes take the reader's local date as
//! an explicit `today` argument rather than inventing one from a UTC clock.
//!
//! Only stored task state is modelled here. A client may layer optimistic UI
//! on top (the web client keeps a just-ticked task visible for a grace
//! period); that is deliberately invisible to the server.

use std::collections::BTreeMap;

/// The narrow slice of a task a lane is allowed to see.
///
/// Deliberately not the full task record: a lane must not come to depend on
/// a field the counts pass does not have cheaply to hand.
#[derive(Debug, Clone)]
pub struct LaneTask {
    pub status: String,
    pub tags: Vec<String>,
    pub due_date: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskLane {
    All,
    Inbox,
    Today,
    Overdue,
    Running,
    Completed,
}

impl TaskLane {
    pub const ALL: [TaskLane; 6] = [
        TaskLane::All,
        TaskLane::Inbox,
        TaskLane::Today,
        TaskLane::Overdue,
        TaskLane::Running,
        TaskLane::Completed,
    ];

    /// The name this lane travels under on the wire (`?view=`).
    pub fn wire_name(self) -> &'static str {
        match self {
            TaskLane::All => "all",
            TaskLane::Inbox => "inbox",
            TaskLane::Today => "today",
            TaskLane::Overdue => "overdue",
            TaskLane::Running => "running",
            TaskLane::Completed => "completed",
        }
    }

    /// Exact-match parse of a wire name. Unknown values are rejected rather
    /// than defaulted, so a typo cannot silently serve a different lane.
    pub fn parse(value: &str) -> Option<TaskLane> {
        TaskLane::ALL
            .into_iter()
            .find(|lane| lane.wire_name() == value)
    }

    /// Whether this lane is meaningless without the reader's local date.
    pub fn needs_today(self) -> bool {
        matches!(self, TaskLane::Today | TaskLane::Overdue)
    }

    /// `today` is `YYYY-MM-DD` in the READER's timezone.
    pub fn matches(self, task: &LaneTask, today: &str) -> bool {
        match self {
            TaskLane::All => task.status != "completed",
            TaskLane::Inbox => task.tags.is_empty() && task.status == "pending",
            TaskLane::Today => task
                .due_date
                .as_deref()
                .is_some_and(|due| due.starts_with(today)),
            TaskLane::Overdue => {
                task.due_date
                    .as_deref()
                    // An empty string is absence. Without this guard it sorts
                    // before every real date and every untouched task reads
                    // as overdue.
                    .filter(|due| !due.is_empty())
                    .is_some_and(|due| due < today)
                    && task.status != "completed"
            },
            TaskLane::Running => task.status == "running" || task.status == "paused",
            TaskLane::Completed => task.status == "completed",
        }
    }
}

/// Every lane's total over the whole pool, in one pass.
///
/// Every lane is present even at zero. A missing key makes a client render
/// nothing where it should render `0`, and "no badge" and "zero" are
/// different claims about the reader's work.
///
/// `BTreeMap` for stable JSON key order — a diffable payload is worth more
/// than the microseconds a hash map saves on six keys.
pub fn lane_counts(pool: &[LaneTask], today: &str) -> BTreeMap<&'static str, usize> {
    let mut counts: BTreeMap<&'static str, usize> = TaskLane::ALL
        .into_iter()
        .map(|lane| (lane.wire_name(), 0))
        .collect();
    for task in pool {
        for lane in TaskLane::ALL {
            if lane.matches(task, today) {
                *counts.get_mut(lane.wire_name()).expect("seeded above") += 1;
            }
        }
    }
    counts
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    const TODAY: &str = "2026-07-30";
    const YESTERDAY: &str = "2026-07-29";
    const TOMORROW: &str = "2026-07-31";

    fn task(status: &str, tags: &[&str], due_date: Option<&str>) -> LaneTask {
        LaneTask {
            status: status.to_string(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            due_date: due_date.map(str::to_string),
        }
    }

    #[test]
    fn all_keeps_unfinished_work_including_running_and_failed() {
        for status in ["pending", "running", "paused", "failed", "cancelled"] {
            assert!(
                TaskLane::All.matches(&task(status, &[], None), TODAY),
                "`all` must keep status {status}"
            );
        }
        assert!(
            !TaskLane::All.matches(&task("completed", &[], None), TODAY),
            "`all` must drop completed work"
        );
    }

    #[test]
    fn inbox_needs_untagged_and_pending_together() {
        assert!(TaskLane::Inbox.matches(&task("pending", &[], None), TODAY));
        assert!(
            !TaskLane::Inbox.matches(&task("pending", &["work"], None), TODAY),
            "a tagged task has been triaged out of the inbox"
        );
        assert!(
            !TaskLane::Inbox.matches(&task("running", &[], None), TODAY),
            "an untagged task already underway is not waiting in the inbox"
        );
        assert!(!TaskLane::Inbox.matches(&task("completed", &[], None), TODAY));
    }

    #[test]
    fn today_matches_a_date_prefix_so_a_full_timestamp_still_counts() {
        assert!(TaskLane::Today.matches(&task("pending", &[], Some(TODAY)), TODAY));
        assert!(
            TaskLane::Today.matches(&task("pending", &[], Some("2026-07-30T14:03:00Z")), TODAY),
            "a stored timestamp is still due today"
        );
        assert!(!TaskLane::Today.matches(&task("pending", &[], Some(TOMORROW)), TODAY));
        assert!(!TaskLane::Today.matches(&task("pending", &[], Some(YESTERDAY)), TODAY));
        assert!(!TaskLane::Today.matches(&task("pending", &[], None), TODAY));
        assert!(
            !TaskLane::Today.matches(&task("pending", &[], Some("")), TODAY),
            "an empty due date is absence, not a match on every day"
        );
    }

    #[test]
    fn overdue_excludes_completed_absent_and_empty_due_dates() {
        assert!(TaskLane::Overdue.matches(&task("pending", &[], Some(YESTERDAY)), TODAY));
        assert!(
            TaskLane::Overdue.matches(&task("running", &[], Some("2026-07-29T23:59:00Z")), TODAY)
        );
        assert!(
            !TaskLane::Overdue.matches(&task("completed", &[], Some(YESTERDAY)), TODAY),
            "finished work is not still owed"
        );
        assert!(
            !TaskLane::Overdue.matches(&task("pending", &[], None), TODAY),
            "a task with no due date can never be late"
        );
        assert!(
            !TaskLane::Overdue.matches(&task("pending", &[], Some("")), TODAY),
            "an empty due date sorts before every real date; it must read as absence"
        );
        assert!(
            !TaskLane::Overdue.matches(&task("pending", &[], Some(TODAY)), TODAY),
            "due today is not yet overdue"
        );
        assert!(!TaskLane::Overdue.matches(&task("pending", &[], Some(TOMORROW)), TODAY));
    }

    #[test]
    fn running_folds_paused_in() {
        assert!(TaskLane::Running.matches(&task("running", &[], None), TODAY));
        assert!(
            TaskLane::Running.matches(&task("paused", &[], None), TODAY),
            "a paused run is still a run the reader started"
        );
        assert!(!TaskLane::Running.matches(&task("pending", &[], None), TODAY));
        assert!(!TaskLane::Running.matches(&task("completed", &[], None), TODAY));
        assert!(!TaskLane::Running.matches(&task("failed", &[], None), TODAY));
    }

    #[test]
    fn completed_matches_only_completed() {
        assert!(TaskLane::Completed.matches(&task("completed", &[], None), TODAY));
        for status in ["pending", "running", "paused", "failed"] {
            assert!(!TaskLane::Completed.matches(&task(status, &[], None), TODAY));
        }
    }

    #[test]
    fn every_lane_parses_from_its_wire_name_and_nothing_else() {
        // Spelled out rather than derived from ALL, so a lane dropped from
        // ALL (or duplicated into it) fails here instead of quietly agreeing
        // with itself.
        assert_eq!(TaskLane::parse("all"), Some(TaskLane::All));
        assert_eq!(TaskLane::parse("inbox"), Some(TaskLane::Inbox));
        assert_eq!(TaskLane::parse("today"), Some(TaskLane::Today));
        assert_eq!(TaskLane::parse("overdue"), Some(TaskLane::Overdue));
        assert_eq!(TaskLane::parse("running"), Some(TaskLane::Running));
        assert_eq!(TaskLane::parse("completed"), Some(TaskLane::Completed));

        for lane in TaskLane::ALL {
            assert_eq!(TaskLane::parse(lane.wire_name()), Some(lane));
        }

        for junk in [
            "", " ", "ALL", "Today", "inbox ", "todo", "overdue!", "none",
        ] {
            assert_eq!(TaskLane::parse(junk), None, "{junk} must not parse");
        }
    }

    #[test]
    fn only_the_two_date_lanes_need_today() {
        assert!(TaskLane::Today.needs_today());
        assert!(TaskLane::Overdue.needs_today());
        for lane in [
            TaskLane::All,
            TaskLane::Inbox,
            TaskLane::Running,
            TaskLane::Completed,
        ] {
            assert!(
                !lane.needs_today(),
                "{} does not read a clock",
                lane.wire_name()
            );
        }
    }

    #[test]
    fn counts_every_lane_over_a_mixed_pool() {
        let pool = vec![
            // untagged + pending -> all, inbox
            task("pending", &[], None),
            // tagged, due today with a timestamp -> all, today, running
            task("running", &["work"], Some("2026-07-30T09:00:00Z")),
            // tagged, due yesterday, unfinished -> all, overdue
            task("pending", &["home"], Some(YESTERDAY)),
            // finished yesterday -> completed only; NOT all, NOT overdue
            task("completed", &[], Some(YESTERDAY)),
            // paused with an empty due date -> all, running; no date lane
            task("paused", &[], Some("")),
        ];

        let counts = lane_counts(&pool, TODAY);

        assert_eq!(
            counts.get("all"),
            Some(&4),
            "completed work is not in `all`"
        );
        assert_eq!(
            counts.get("inbox"),
            Some(&1),
            "only the untagged pending task is untriaged"
        );
        assert_eq!(counts.get("today"), Some(&1));
        assert_eq!(
            counts.get("overdue"),
            Some(&1),
            "the completed and empty-due-date rows must not count as late"
        );
        assert_eq!(counts.get("running"), Some(&2), "paused counts as running");
        assert_eq!(counts.get("completed"), Some(&1));

        // Stable, sorted key order — the payload stays diffable.
        assert_eq!(
            counts.keys().copied().collect::<Vec<_>>(),
            vec!["all", "completed", "inbox", "overdue", "running", "today"]
        );
    }

    #[test]
    fn an_empty_pool_reports_every_lane_at_zero_rather_than_omitting_it() {
        let counts = lane_counts(&[], TODAY);

        assert_eq!(counts.len(), 6, "all six lanes must be present");
        for lane in TaskLane::ALL {
            assert_eq!(
                counts.get(lane.wire_name()),
                Some(&0),
                "{} must report 0, not go missing — a client renders nothing \
                 for a missing key where it should render a zero",
                lane.wire_name()
            );
        }
    }
}
