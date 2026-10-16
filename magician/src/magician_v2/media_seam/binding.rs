//! What a meeting IS, and how long it stays the same one.
//!
//! Plan: `docs/plans/2026-08-07-opc-outward-agent-boundary.md` §4.5a.
//!
//! A room's continuity rests entirely on its binding: the same binding hands
//! back the same chat session, which is where the transcript lives. That made
//! the binding's identity a product decision rather than a naming detail, and
//! the first cut answered it with the calendar:
//! [`derive_meeting_thread_id`](super::derive_meeting_thread_id) appends the
//! date, so a meeting running past midnight becomes a different meeting at
//! 00:00 and a bot that drops at 23:58 rejoins into an empty room.
//!
//! # The two wrong answers, and why the date was the safer one
//!
//! Dropping the date entirely merges every occurrence of a recurring standup
//! into one binding. That is not a UX quirk — it is a containment regression:
//! Monday's participants said things Thursday's participants were never in the
//! room for, and one binding means one transcript.
//!
//! Keeping the date splits a single long meeting. That costs recall and
//! nothing else: the room starts blank, which is exactly what an unbound room
//! is entitled to.
//!
//! So identity here is **the meeting, plus how recently it was alive**. Two
//! parts:
//!
//! * the **identity** — the dateless label (`meet` code, calendar title, or a
//!   hash), which is what makes Monday's standup and Thursday's standup the
//!   same *meeting*;
//! * the **occurrence** — the dated thread, which is what makes them different
//!   *gatherings*.
//!
//! A rejoin continues an occurrence when that occurrence was alive within
//! [`REJOIN_GRACE`]. Past that, it opens a new one.
//!
//! # Why the grace is short, and measured from the last activity
//!
//! From the last activity, because that is what "the meeting is still going"
//! means: the gap that matters is drop→rejoin, not start→now. A meeting that
//! ran nine hours and dropped at 23:58 has a two-minute gap at 00:02 and
//! continues; a standup that ended at 09:30 has a twenty-three-hour gap at
//! 09:00 the next day and does not.
//!
//! Short, because the two failure directions are not symmetric. Too long
//! merges two gatherings — the containment regression above. Too short starts
//! a room blank — a recall cost. So the window is sized to a network drop and
//! a process restart, not to a coffee break, and anything longer deliberately
//! opens a new occurrence.
//!
//! Nothing here reads a clock, a store, or a config: every input is supplied,
//! so the same decision is reproducible in a test, in a diagnostic, and at a
//! join.

use chrono::{DateTime, Duration, NaiveDate, Utc};

/// Every meeting thread id starts with this, and nothing else does. The
/// prefix is what lets a reader tell a room's thread from an owner's without
/// consulting a store — used to decide whether continuity applies at all.
pub const MEETING_THREAD_PREFIX: &str = "meeting-";

/// The `YYYY-MM-DD` suffix every derived meeting thread carries.
const MEETING_THREAD_DATE_LEN: usize = 10;

/// How long after its last activity a dropped meeting may still be rejoined
/// into the SAME occurrence.
///
/// Expiry is INCLUSIVE: a gap of exactly this long has expired. A boundary
/// that admitted the exact value would make the constant read as "up to and
/// including", and every other expiry in this codebase reads the other way —
/// one rule, so nobody has to remember which kind this is.
pub const REJOIN_GRACE: Duration = Duration::minutes(30);

/// A meeting thread id, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingThreadParts {
    /// The dateless label — the same across every occurrence of a recurring
    /// meeting. This is the meeting's IDENTITY.
    pub identity: String,
    /// The `YYYY-MM-DD` this occurrence opened on.
    pub date: String,
}

/// Split `meeting-<identity>-<YYYY-MM-DD>` back into its parts.
///
/// `None` for anything that is not a derived meeting thread — an owner thread,
/// or a thread pinned by `MEET_BOT_THREAD` to a fixed name. Both must fail
/// CLOSED into "no continuity across occurrences" rather than being guessed at:
/// a mis-parse here would either merge two meetings or hand one meeting's
/// binding to an owner thread.
///
/// The label may itself contain dashes, so the date is taken from the END —
/// splitting on the first dash would make `weekly-sync` parse as identity
/// `weekly`.
pub fn meeting_thread_parts(thread: &str) -> Option<MeetingThreadParts> {
    let rest = thread.strip_prefix(MEETING_THREAD_PREFIX)?;
    if rest.len() < MEETING_THREAD_DATE_LEN + 2 {
        // Needs at least one identity character, a separator, and a date.
        return None;
    }
    let split_at = rest.len() - MEETING_THREAD_DATE_LEN;
    let (identity_with_dash, date) = rest.split_at(split_at);
    let identity = identity_with_dash.strip_suffix('-')?;
    if identity.is_empty() {
        return None;
    }
    // A date-shaped suffix is not enough; an identity ending in `-2026-13-45`
    // would otherwise be read as a date and lose its last segment.
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(MeetingThreadParts {
        identity: identity.to_string(),
        date: date.to_string(),
    })
}

/// Whether this thread belongs to a meeting at all.
pub fn is_meeting_thread(thread: &str) -> bool {
    meeting_thread_parts(thread).is_some()
}

/// The containment label for content produced in this thread.
///
/// One occurrence, one label. The thread id is the occurrence's identity —
/// the same string the chat session is keyed by — so the label a room writes
/// and the binding a room reads under can never drift apart.
///
/// `None` for a thread that is not a meeting: an owner thread's content is not
/// an occasion's, and labelling it as one would let a room reach it by
/// claiming that occasion.
pub fn meeting_binding_id(thread: &str) -> Option<String> {
    is_meeting_thread(thread).then(|| thread.to_string())
}

/// One existing occurrence of some meeting, as the caller found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingThreadCandidate {
    /// The occurrence's thread id.
    pub thread: String,
    /// When anything last happened in it. The caller supplies this — the
    /// chat store already tracks it per session, and inventing a second
    /// source of truth for "still alive" is how the two would disagree.
    pub last_activity: DateTime<Utc>,
}

/// Why a rejoin landed where it did. More than a thread id, because the two
/// outcomes are operationally different and a caller that could not tell them
/// apart could not report that a room started blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetingContinuity {
    /// The requested occurrence is the live one — an ordinary same-day rejoin,
    /// or the first join.
    Same,
    /// A DIFFERENT occurrence of the same meeting was still alive, so this
    /// rejoin continues it. This is the midnight case: the requested thread
    /// carries today's date, the live one carries yesterday's.
    Continued { thread: String },
}

impl MeetingContinuity {
    /// The thread to actually use.
    pub fn thread<'a>(&'a self, requested: &'a str) -> &'a str {
        match self {
            Self::Same => requested,
            Self::Continued { thread } => thread.as_str(),
        }
    }
}

/// Decide which occurrence a join belongs to.
///
/// `requested` is what the calendar-derived naming produced (today's dated
/// thread). `candidates` are the meeting threads that already exist in this
/// scope, with their last activity. The rule:
///
/// 1. a candidate that IS the requested thread wins outright — a same-day
///    rejoin is not a continuity question;
/// 2. otherwise the most recently active candidate sharing the requested
///    thread's IDENTITY continues, if its gap is inside [`REJOIN_GRACE`];
/// 3. otherwise the requested thread stands, and the room opens a new
///    occurrence.
///
/// Only threads sharing the identity are ever considered, so this can never
/// reach another meeting however long the grace is set — the window decides
/// whether a gathering is still the same one, never which meeting it is.
///
/// A candidate whose last activity is in the FUTURE relative to `now` (clock
/// skew, a restored backup) is treated as expired rather than as maximally
/// fresh: an unexplained timestamp must not be the thing that merges two
/// gatherings.
pub fn decide_meeting_continuity(
    requested: &str,
    candidates: &[MeetingThreadCandidate],
    now: DateTime<Utc>,
    grace: Duration,
) -> MeetingContinuity {
    let Some(requested_parts) = meeting_thread_parts(requested) else {
        // Not a derived meeting thread — a pinned name, or an owner thread.
        // No identity to continue, so nothing to continue into.
        return MeetingContinuity::Same;
    };
    if candidates
        .iter()
        .any(|candidate| candidate.thread == requested)
    {
        return MeetingContinuity::Same;
    }
    let mut best: Option<&MeetingThreadCandidate> = None;
    for candidate in candidates {
        if candidate.thread == requested {
            continue;
        }
        let Some(parts) = meeting_thread_parts(&candidate.thread) else {
            continue;
        };
        if parts.identity != requested_parts.identity {
            continue;
        }
        if candidate.last_activity > now {
            continue;
        }
        if now - candidate.last_activity >= grace {
            continue;
        }
        let replaces = best.is_none_or(|current| {
            candidate.last_activity > current.last_activity
                || (candidate.last_activity == current.last_activity
                    && candidate.thread > current.thread)
        });
        if replaces {
            best = Some(candidate);
        }
    }
    match best {
        Some(candidate) => MeetingContinuity::Continued {
            thread: candidate.thread.clone(),
        },
        None => MeetingContinuity::Same,
    }
}

#[cfg(test)]
mod tests {
    use crate::magician_v2::media_seam::*;
    use chrono::{DateTime, Utc};

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("test timestamp")
            .with_timezone(&Utc)
    }

    fn candidate(thread: &str, last_activity: &str) -> MeetingThreadCandidate {
        MeetingThreadCandidate {
            thread: thread.to_string(),
            last_activity: at(last_activity),
        }
    }

    /// The identity must survive a dashed label, which every calendar title
    /// produces. Splitting on the first dash would read `weekly-sync` as
    /// `weekly` and silently give two different meetings the same identity.
    #[test]
    fn a_dashed_label_keeps_its_whole_identity() {
        assert_eq!(
            meeting_thread_parts("meeting-weekly-sync-2026-08-18"),
            Some(MeetingThreadParts {
                identity: "weekly-sync".to_string(),
                date: "2026-08-18".to_string(),
            })
        );
        assert_eq!(
            meeting_thread_parts("meeting-abc-defg-hij-2026-08-18").map(|parts| parts.identity),
            Some("abc-defg-hij".to_string())
        );
    }

    /// Everything that is not a derived meeting thread fails CLOSED to "no
    /// identity". A pinned `MEET_BOT_THREAD` name and an owner thread both
    /// land here, and treating either as a meeting identity would let one
    /// continue into the other.
    #[test]
    fn a_non_meeting_thread_has_no_identity_and_no_binding() {
        for thread in [
            "general",
            "meeting-bot",
            "meeting-",
            "meeting--2026-08-18",
            "meeting-standup-2026-13-45",
            "meeting-standup-not-a-date",
            "standup-2026-08-18",
        ] {
            assert!(
                meeting_thread_parts(thread).is_none(),
                "`{thread}` parsed as a meeting occurrence"
            );
            assert!(!is_meeting_thread(thread));
            assert_eq!(
                meeting_binding_id(thread),
                None,
                "`{thread}` produced a containment label it must not have"
            );
        }
        assert_eq!(
            meeting_binding_id("meeting-standup-2026-08-18"),
            Some("meeting-standup-2026-08-18".to_string())
        );
    }

    /// The bug this exists to fix: a meeting running past midnight, or a bot
    /// that drops at 23:58 and rejoins at 00:02, must land in the occurrence
    /// that holds its transcript rather than in a new, empty one.
    #[test]
    fn a_rejoin_across_midnight_continues_the_live_occurrence() {
        let yesterday = candidate("meeting-all-hands-2026-08-18", "2026-08-18T23:58:00Z");
        let decision = decide_meeting_continuity(
            "meeting-all-hands-2026-08-19",
            std::slice::from_ref(&yesterday),
            at("2026-08-19T00:02:00Z"),
            REJOIN_GRACE,
        );
        assert_eq!(
            decision,
            MeetingContinuity::Continued {
                thread: "meeting-all-hands-2026-08-18".to_string(),
            },
            "a four-minute gap across midnight opened a new room, so the bot \
             rejoined blank"
        );
        assert_eq!(
            decision.thread("meeting-all-hands-2026-08-19"),
            "meeting-all-hands-2026-08-18"
        );
    }

    /// The containment half, and the reason the grace is short: the next
    /// occurrence of a recurring meeting is a DIFFERENT gathering. Merging
    /// them would put Tuesday's participants in front of Monday's transcript.
    #[test]
    fn tomorrows_standup_does_not_continue_todays() {
        let yesterday = candidate("meeting-standup-2026-08-18", "2026-08-18T09:30:00Z");
        assert_eq!(
            decide_meeting_continuity(
                "meeting-standup-2026-08-19",
                &[yesterday],
                at("2026-08-19T09:00:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Same,
            "a recurring meeting's next occurrence inherited the previous one's room"
        );
    }

    /// Expiry is INCLUSIVE, asserted at the exact boundary because that is the
    /// only place an off-by-one hides: one second inside continues, exactly at
    /// the grace does not.
    #[test]
    fn the_grace_expires_inclusively() {
        let inside = candidate("meeting-sync-2026-08-18", "2026-08-18T23:30:01Z");
        assert_eq!(
            decide_meeting_continuity(
                "meeting-sync-2026-08-19",
                &[inside],
                at("2026-08-19T00:00:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Continued {
                thread: "meeting-sync-2026-08-18".to_string(),
            }
        );
        let exactly = candidate("meeting-sync-2026-08-18", "2026-08-18T23:30:00Z");
        assert_eq!(
            decide_meeting_continuity(
                "meeting-sync-2026-08-19",
                &[exactly],
                at("2026-08-19T00:00:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Same,
            "a gap of exactly the grace must read as expired"
        );
    }

    /// A stale binding must never outlive its meeting, and a different meeting
    /// must never be reachable however fresh it is. Both are pinned together
    /// because the second is what a longer grace would break.
    #[test]
    fn continuity_never_reaches_a_different_meeting() {
        let other = candidate("meeting-board-review-2026-08-19", "2026-08-19T00:01:00Z");
        assert_eq!(
            decide_meeting_continuity(
                "meeting-all-hands-2026-08-19",
                &[other],
                at("2026-08-19T00:02:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Same,
            "a rejoin continued a DIFFERENT meeting that happened to be live"
        );
    }

    /// A future timestamp is not freshness. Clock skew or a restored backup
    /// must not be the thing that merges two gatherings.
    #[test]
    fn a_future_last_activity_does_not_continue() {
        let skewed = candidate("meeting-sync-2026-08-18", "2026-08-19T10:00:00Z");
        assert_eq!(
            decide_meeting_continuity(
                "meeting-sync-2026-08-19",
                &[skewed],
                at("2026-08-19T00:02:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Same
        );
    }

    /// The ordinary same-day rejoin is not a continuity question: the
    /// requested occurrence exists, so it wins outright even when an older
    /// occurrence of the same meeting is still inside the grace. Without this
    /// arm a same-day rejoin could be pulled backwards into yesterday's room.
    #[test]
    fn an_existing_requested_occurrence_wins_outright() {
        let candidates = [
            candidate("meeting-sync-2026-08-19", "2026-08-19T00:05:00Z"),
            candidate("meeting-sync-2026-08-18", "2026-08-19T00:06:00Z"),
        ];
        assert_eq!(
            decide_meeting_continuity(
                "meeting-sync-2026-08-19",
                &candidates,
                at("2026-08-19T00:07:00Z"),
                REJOIN_GRACE,
            ),
            MeetingContinuity::Same
        );
    }
}
