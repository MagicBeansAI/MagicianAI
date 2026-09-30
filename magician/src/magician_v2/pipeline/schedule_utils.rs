//! Schedule utilities — pure functions, no I/O, no async.

use chrono::{DateTime, Utc};

use super::agent::AgentScheduleKind;

/// Compute the next fire time for `schedule` given `last_fire` and `now`.
///
/// Returns `None` for `OnEvent` (not time-driven) and for `Once` after first fire.
/// Returns `Some(now)` when the agent should fire immediately (never fired or overdue).
///
/// # Cron variant note
/// Uses the scheduler's `compute_next_run_at` which supports timezone-aware evaluation.
/// Returns the next scheduled time after the base time (last_fire or now).
pub fn next_fire(
    schedule: &AgentScheduleKind,
    last_fire: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match schedule {
        AgentScheduleKind::Cron {
            expression,
            timezone,
        } => {
            let tz = timezone.as_deref().unwrap_or("UTC");
            let base = last_fire.unwrap_or(now);
            crate::magician_v2::agents::scheduler::compute_next_run_at(expression, tz, base)
        },

        AgentScheduleKind::Interval { seconds, .. } => {
            match last_fire {
                None => Some(now), // never fired — fire immediately
                Some(last) => {
                    let next = last + chrono::Duration::seconds(*seconds as i64);
                    Some(next) // may be in the past (overdue) or future
                },
            }
        },

        AgentScheduleKind::Once { at } => {
            if last_fire.is_none() {
                Some(*at) // not yet fired
            } else {
                None // already fired once
            }
        },

        AgentScheduleKind::OnEvent { .. } => None, // not time-driven
    }
}

/// Count the number of fires missed between `last_fire` and `now`.
///
/// Walks forward from `last_fire` using `next_fire()`, counting fires whose
/// time is before `now`. Safety cap at 10,000 to prevent infinite loops
/// on very frequent schedules.
pub fn count_missed_fires(
    schedule: &AgentScheduleKind,
    last_fire: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> u32 {
    let mut count = 0u32;
    let mut cursor = last_fire;
    const MAX_MISSED: u32 = 10_000;
    loop {
        match next_fire(schedule, cursor, now) {
            Some(next) if next < now => {
                count += 1;
                cursor = Some(next);
                if count >= MAX_MISSED {
                    break;
                }
            },
            _ => break,
        }
    }
    count
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn interval_with_no_last_fire_returns_now() {
        let now = Utc::now();
        let schedule = AgentScheduleKind::Interval {
            seconds: 3600,
            jitter_seconds: None,
        };
        let result = next_fire(&schedule, None, now);
        assert!(result.is_some());
        // Should fire immediately (no last fire = fire now)
        assert!(result.unwrap() <= now + chrono::Duration::seconds(1));
    }

    #[test]
    fn interval_with_recent_last_fire_returns_future() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(100);
        let schedule = AgentScheduleKind::Interval {
            seconds: 3600,
            jitter_seconds: None,
        };
        let result = next_fire(&schedule, Some(last), now);
        assert!(result.is_some());
        let next = result.unwrap();
        assert!(next > now, "next={} should be in the future", next);
        // Should be ~3500s from now (3600 - 100 elapsed)
        let diff = (next - now).num_seconds();
        assert!(diff > 3400 && diff < 3700, "diff={}", diff);
    }

    #[test]
    fn once_with_no_last_fire_returns_at_time() {
        let fire_at = Utc::now() + chrono::Duration::hours(1);
        let schedule = AgentScheduleKind::Once { at: fire_at };
        let result = next_fire(&schedule, None, Utc::now());
        assert_eq!(result, Some(fire_at));
    }

    #[test]
    fn once_already_fired_returns_none() {
        let fire_at = Utc::now() + chrono::Duration::hours(1);
        let schedule = AgentScheduleKind::Once { at: fire_at };
        // last_fire set = already fired
        let result = next_fire(&schedule, Some(Utc::now()), Utc::now());
        assert_eq!(result, None);
    }

    #[test]
    fn on_event_returns_none() {
        let schedule = AgentScheduleKind::OnEvent {
            event_pattern: "agent.goal.completed".into(),
        };
        let result = next_fire(&schedule, None, Utc::now());
        assert_eq!(result, None); // event-driven: not time-based
    }

    #[test]
    fn cron_next_fire_is_in_the_future() {
        // Every minute at second 0
        let kind = AgentScheduleKind::Cron {
            expression: "0 * * * * *".to_string(),
            timezone: None,
        };
        let now = chrono::Utc::now();
        let result = next_fire(&kind, None, now);
        assert!(result.is_some(), "cron should produce a next fire time");
        assert!(result.unwrap() > now, "next fire should be in the future");
    }

    #[test]
    fn cron_next_fire_respects_last_fire() {
        // C-10: next fire should be after last_fire, not merely after now.
        // Use a far-future last_fire so that `sched.upcoming(now)` would give
        // a time before last_fire, while `sched.after(last_fire)` is correctly later.
        use chrono::Duration;
        let now = chrono::Utc::now();
        // last_fire is 30s in the future (simulates a fire that happened "after now"
        // due to clock skew or test setup); the next cron tick must be after last_fire.
        let last_fire = now + Duration::seconds(30);
        let kind = AgentScheduleKind::Cron {
            expression: "0 * * * * *".to_string(), // every minute
            timezone: None,
        };
        let result = next_fire(&kind, Some(last_fire), now);
        assert!(result.is_some(), "cron should produce a next fire time");
        assert!(
            result.unwrap() > last_fire,
            "next fire {:?} should be after last_fire {:?}",
            result.unwrap(),
            last_fire
        );
    }
}
