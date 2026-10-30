//! Who may use a chat thread's shared browser window.
//!
//! Runs a chat launches (pack calls, delegations, handovers, sub-goals) share
//! one window keyed on the chat thread, so a follow-up sees the page the last
//! run left. Two launches running at the same time must not share it: the
//! window's tabs belong to the session, so a second task would navigate the
//! first task's tab. The chat marks each launch in its session override
//! (`magician-chat-<thread>~<launch>`); every run of that launch inherits the
//! same override and so forms one family.
//!
//! Every window a launch is given — the thread's shared one, or its own
//! `<shared>-t<launch>` when another launch holds the shared one — has a
//! lease naming that launch and the runs using it:
//!
//! * [`resolve`] gives a run its launch's window, checking the lease each
//!   time: a run whose window was released, or taken over by another launch,
//!   is routed afresh before its next call.
//! * [`hold`] records that a run uses its window, once one of its browser
//!   calls has passed every check. A launch's claim that none of its runs
//!   ever used lapses after [`UNUSED_CLAIM_TTL`].
//! * Terminal cleanup releases a run ([`release_run`]). A window may be closed
//!   only when its last run has released it. The ID stays reserved through
//!   asynchronous teardown, then another launch can reuse it. Pauses keep holding.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Separates a chat's shared session from the launch that asked for it.
pub const LAUNCH_SEPARATOR: char = '~';

/// How long a launch's claim on a window lasts when none of its runs ever
/// completes a browser call.
pub const UNUSED_CLAIM_TTL: Duration = Duration::from_secs(5 * 60);

/// Past this many remembered runs, those no lease depends on are dropped.
const MAX_REMEMBERED_RUNS: usize = 4096;

struct Lease {
    family: String,
    runs: HashSet<String>,
    claimed_at: Instant,
    retiring: bool,
}

impl Lease {
    fn claim(family: &str) -> Self {
        Self {
            family: family.to_string(),
            runs: HashSet::new(),
            claimed_at: Instant::now(),
            retiring: false,
        }
    }

    fn lapsed(&self) -> bool {
        !self.retiring && self.runs.is_empty() && self.claimed_at.elapsed() >= UNUSED_CLAIM_TTL
    }
}

#[derive(Clone)]
struct Assignment {
    session: String,
    launch: String,
}

#[derive(Default)]
struct Leases {
    /// Window session id → its lease.
    leases: HashMap<String, Lease>,
    /// Run → the window it was given.
    by_run: HashMap<String, Assignment>,
    /// Actual IDs (including scoped variants/retrieval loans), retained across
    /// pauses so cancellation does not have to reconstruct what a run opened.
    sessions_by_run: HashMap<String, HashSet<String>>,
}

fn leases() -> &'static Mutex<Leases> {
    static LEASES: OnceLock<Mutex<Leases>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(Leases::default()))
}

/// The override a chat launch hands its runs: the thread's shared session,
/// marked with a fresh launch id.
pub fn chat_launch_override(shared_session: &str) -> String {
    let launch = uuid::Uuid::new_v4().simple().to_string();
    format!("{shared_session}{LAUNCH_SEPARATOR}{}", &launch[..12])
}

/// Split a chat launch override into `(shared session, launch)`; `None` for
/// any other override, which is used as written.
pub fn split_launch_override(override_id: &str) -> Option<(&str, &str)> {
    let (shared, launch) = override_id.rsplit_once(LAUNCH_SEPARATOR)?;
    (!shared.is_empty() && !launch.is_empty() && launch.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some((shared, launch))
}

/// Whether `session` (which may carry suffixes such as `--fenced`) is built
/// from the window `assigned`.
pub fn derives_from(session: &str, assigned: &str) -> bool {
    session == assigned
        || session
            .strip_prefix(assigned)
            .is_some_and(|rest| rest.starts_with("--"))
}

/// The window `run` of `launch` uses for `shared`. Decides, but holds
/// nothing: see [`hold`].
pub fn resolve(shared: &str, launch: &str, run: &str) -> String {
    let mut leases = leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(assignment) = leases.by_run.get(run) {
        let still_ours = leases
            .leases
            .get(&assignment.session)
            .is_some_and(|lease| lease.family == launch && !lease.retiring);
        if still_ours {
            return assignment.session.clone();
        }
        // Released, or taken over by another launch: route afresh.
        leases.by_run.remove(run);
    }
    if leases.by_run.len() >= MAX_REMEMBERED_RUNS {
        forget_unheld(&mut leases);
    }
    let own = format!("{shared}-t{launch}");
    // A family may have received a fresh generation while an older session
    // was retiring. Its later children must still join that active generation.
    let existing = leases
        .leases
        .iter()
        .find(|(window, lease)| {
            lease.family == launch
                && !lease.retiring
                && (window.as_str() == shared || window.starts_with(&own))
        })
        .map(|(window, _)| window.clone());
    let session = if let Some(existing) = existing {
        existing
    } else if leases.leases.get(shared).is_none_or(Lease::lapsed) {
        leases
            .leases
            .insert(shared.to_string(), Lease::claim(launch));
        shared.to_string()
    } else {
        let own = if leases.leases.contains_key(&own) {
            format!("{own}-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
        } else {
            own
        };
        leases.leases.insert(own.clone(), Lease::claim(launch));
        own
    };
    leases.by_run.insert(
        run.to_string(),
        Assignment {
            session: session.clone(),
            launch: launch.to_string(),
        },
    );
    session
}

/// `run` is now using its window (a browser call went through). Idempotent;
/// a run that never resolved a chat window is ignored.
#[cfg(test)]
pub fn hold(run: &str) -> bool {
    hold_inner(run, None)
}

pub fn hold_session(run: &str, session: &str) -> bool {
    hold_inner(run, Some(session))
}

fn hold_inner(run: &str, expected_session: Option<&str>) -> bool {
    let mut leases = leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(assignment) = leases.by_run.get(run).cloned() else {
        return expected_session.is_none();
    };
    if expected_session.is_some_and(|session| !derives_from(session, &assignment.session)) {
        return false;
    }
    match leases.leases.get_mut(&assignment.session) {
        Some(lease) if lease.family == assignment.launch && !lease.retiring => {
            lease.runs.insert(run.to_string());
            true
        },
        // Another launch holds it now; this run is routed afresh on its next
        // call (`resolve`) and never joins someone else's window.
        Some(_) | None => false,
    }
}

/// Release `run` from its window. Returns that window and whether it may now
/// be closed (no other run holds it); `None` when the run got no chat window.
/// Cleanup applies the answer to every session id derived from the window.
pub fn release_run(run: &str) -> Option<(String, bool)> {
    let mut leases = leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let assignment = leases.by_run.get(run)?.clone();
    let closable = match leases.leases.get_mut(&assignment.session) {
        Some(lease) if lease.family == assignment.launch => {
            lease.runs.remove(run);
            let last = lease.runs.is_empty();
            if last {
                // Reserve the ID until asynchronous teardown is finished. A new
                // launch must not reuse a window an old cleanup is closing.
                lease.retiring = true;
            }
            last
        },
        // Another launch's window now: not this run's to close.
        Some(_) => false,
        None => true,
    };
    if !closable {
        leases.by_run.remove(run);
    }
    Some((assignment.session, closable))
}

/// Reserve a session for teardown before doing asynchronous I/O. A resolved
/// claim is protected even before its first browser command reaches `hold`.
/// Callers serialize cleanup so a duplicate cleanup cannot race a fresh lease.
pub fn begin_cleanup(session: &str) -> bool {
    let mut state = leases().lock().unwrap_or_else(|error| error.into_inner());
    if state
        .sessions_by_run
        .values()
        .any(|sessions| sessions.contains(session))
    {
        return false;
    }
    if let Some((_, lease)) = state
        .leases
        .iter()
        .find(|(window, _)| derives_from(session, window))
    {
        return lease.retiring;
    }
    let mut lease = Lease::claim("");
    lease.retiring = true;
    state.leases.insert(session.to_string(), lease);
    true
}

pub fn record_session(run: &str, session: &str) -> bool {
    let mut state = leases().lock().unwrap_or_else(|error| error.into_inner());
    if state
        .leases
        .iter()
        .any(|(window, lease)| lease.retiring && derives_from(session, window))
    {
        return false;
    }
    state
        .sessions_by_run
        .entry(run.to_string())
        .or_default()
        .insert(session.to_string());
    true
}

pub fn sessions_for_run(run: &str) -> Vec<String> {
    leases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .sessions_by_run
        .get(run)
        .map(|sessions| sessions.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn release_session_users(run: &str) {
    leases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .sessions_by_run
        .remove(run);
}

/// Release the teardown reservation only after every derived session closed.
pub fn finish_cleanup(window: &str) {
    let mut state = leases().lock().unwrap_or_else(|error| error.into_inner());
    if state.leases.get(window).is_some_and(|lease| lease.retiring) {
        state.leases.remove(window);
        state
            .by_run
            .retain(|_, assignment| assignment.session != window);
    }
}

/// Sessions derived from a window, for terminal settlement even if its
/// executor was dropped while paused. The assignment is consumed by cleanup.
pub fn assigned_window(run: &str) -> Option<String> {
    leases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .by_run
        .get(run)
        .map(|assignment| assignment.session.clone())
}

pub fn cleanup_pending(window: &str) -> bool {
    leases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .leases
        .get(window)
        .is_some_and(|lease| lease.retiring)
}

pub async fn cleanup_guard(window: &str) -> tokio::sync::OwnedMutexGuard<()> {
    type Locks = HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
    let lock = {
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        match locks.get(window).and_then(std::sync::Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(window.to_string(), std::sync::Arc::downgrade(&lock));
                lock
            },
        }
    };
    lock.lock_owned().await
}

/// Whether some run still holds the window `session` is built from, so it
/// must stay open whichever run's cleanup asks.
pub fn window_in_use(session: &str) -> bool {
    let leases = leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    leases
        .leases
        .iter()
        .any(|(window, lease)| !lease.runs.is_empty() && derives_from(session, window))
}

/// Drop remembered runs no lease depends on, and lapsed unused claims.
fn forget_unheld(leases: &mut Leases) {
    leases.leases.retain(|_, lease| !lease.lapsed());
    let held: HashSet<String> = leases
        .leases
        .values()
        .flat_map(|lease| lease.runs.iter().cloned())
        .collect();
    let retiring: HashSet<String> = leases
        .leases
        .iter()
        .filter(|(_, lease)| lease.retiring)
        .map(|(window, _)| window.clone())
        .collect();
    leases
        .by_run
        .retain(|run, assignment| held.contains(run) || retiring.contains(&assignment.session));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_window_is_reserved_until_its_async_cleanup_finishes() {
        let shared = "magician-chat-retiring-test";
        assert_eq!(resolve(shared, "old", "retire-old"), shared);
        assert!(hold_session("retire-old", shared));
        assert_eq!(release_run("retire-old"), Some((shared.to_string(), true)));
        assert!(begin_cleanup(shared));
        assert!(cleanup_pending(shared));
        let next = resolve(shared, "new", "retire-new");
        assert_ne!(next, shared);
        assert!(!hold_session("retire-old", shared));
        finish_cleanup(shared);
        assert!(!cleanup_pending(shared));
        assert_eq!(resolve(shared, "third", "retire-third"), shared);
        // Even a just-resolved claim is protected from duplicate old cleanup.
        assert!(!begin_cleanup(shared));
        assert!(!hold_session("retire-old", shared));
        release_run("retire-new");
        finish_cleanup(&next);
        release_run("retire-third");
        finish_cleanup(shared);
    }

    #[test]
    fn exact_used_ids_survive_pauses_and_cannot_close_under_another_user() {
        let session = "scope-program-example-magician-paused-cleanup--cdp";
        assert!(record_session("used-ids-a", session));
        assert!(record_session("used-ids-b", session));
        assert_eq!(sessions_for_run("used-ids-a"), vec![session.to_string()]);
        release_session_users("used-ids-a");
        assert!(!begin_cleanup(session));
        release_session_users("used-ids-b");
        assert!(begin_cleanup(session));
        assert!(!record_session("used-ids-late", session));
        finish_cleanup(session);
        assert!(record_session("used-ids-late", session));
        release_session_users("used-ids-late");
    }

    #[test]
    fn a_delayed_command_cannot_hold_an_older_resolved_window() {
        let shared = "scope-engagement-example-magician-chat-delay-test";
        let original = resolve(shared, "one", "delay-one");
        release_run("delay-one");
        let fresh = resolve(shared, "one", "delay-one");
        assert_ne!(fresh, original);
        assert!(!hold_session("delay-one", &original));
        assert!(hold_session("delay-one", &format!("{fresh}--cdp--fenced")));
        assert_eq!(resolve(shared, "one", "delay-child"), fresh);
        assert!(hold_session("delay-child", &fresh));
        assert_eq!(release_run("delay-one"), Some((fresh.clone(), false)));
        release_run("delay-child");
        finish_cleanup(&fresh);
        finish_cleanup(shared);
    }

    #[test]
    fn a_concurrent_launch_gets_its_own_window_and_the_family_shares() {
        let shared = "magician-chat-lease-test-a";
        assert_eq!(resolve(shared, "aaa", "a-1"), shared);
        hold("a-1");
        // A child of the same launch shares the window.
        assert_eq!(resolve(shared, "aaa", "a-2"), shared);
        hold("a-2");
        // Another launch while the first is live gets its own.
        let own_b = format!("{shared}-tbbb");
        assert_eq!(resolve(shared, "bbb", "b-1"), own_b);
        hold("b-1");
        assert_eq!(resolve(shared, "bbb", "b-1"), own_b);
        // The shared window stays open until its last run ends.
        assert_eq!(release_run("a-1"), Some((shared.to_string(), false)));
        assert_eq!(release_run("a-2"), Some((shared.to_string(), true)));
        // Launch B's own window is shared by its family the same way: a child
        // ending first does not close it under the parent.
        assert_eq!(resolve(shared, "bbb", "b-2"), own_b);
        hold("b-2");
        assert_eq!(release_run("b-2"), Some((own_b.clone(), false)));
        assert_eq!(release_run("b-1"), Some((own_b, true)));
        finish_cleanup(shared);
        // Cleaned: the next launch takes the shared window over.
        assert_eq!(resolve(shared, "ccc", "c-1"), shared);
        hold("c-1");
        assert_eq!(release_run("c-1"), Some((shared.to_string(), true)));
    }

    #[test]
    fn a_late_run_of_a_finished_launch_never_joins_another_launchs_window() {
        let shared = "magician-chat-lease-test-b";
        assert_eq!(resolve(shared, "old", "o-1"), shared);
        hold("o-1");
        assert_eq!(release_run("o-1"), Some((shared.to_string(), true)));
        finish_cleanup(shared);
        // A new launch takes the window.
        assert_eq!(resolve(shared, "new", "n-1"), shared);
        hold("n-1");
        // A run of the finished launch arriving now gets its own window.
        assert_eq!(resolve(shared, "old", "o-2"), format!("{shared}-told"));
        release_run("o-2");
        release_run("n-1");
    }

    #[test]
    fn a_run_refused_before_any_browser_call_holds_nothing() {
        let shared = "magician-chat-lease-test-c";
        assert_eq!(resolve(shared, "par", "p-1"), shared);
        hold("p-1");
        // A child resolves but is refused: it never holds.
        assert_eq!(resolve(shared, "par", "p-child"), shared);
        assert_eq!(release_run("p-1"), Some((shared.to_string(), true)));
        finish_cleanup(shared);
        assert_eq!(resolve(shared, "next", "x-1"), shared);
        release_run("x-1");
        release_run("p-child");
    }

    #[test]
    fn a_held_window_reports_in_use_until_its_last_run_releases() {
        let shared = "magician-chat-lease-test-d";
        assert_eq!(resolve(shared, "fam", "f-1"), shared);
        hold("f-1");
        assert!(window_in_use(&format!("{shared}--fenced")));
        let _ = release_run("f-1");
        assert!(!window_in_use(shared));
    }

    #[test]
    fn derived_session_ids_follow_their_window() {
        assert!(derives_from("magician-chat-g", "magician-chat-g"));
        assert!(derives_from("magician-chat-g--fenced", "magician-chat-g"));
        assert!(!derives_from("magician-chat-g-tabc", "magician-chat-g"));
        assert!(!derives_from("retrieval-handoff", "magician-chat-g"));
    }

    #[test]
    fn only_a_marked_chat_override_is_split() {
        let marked = chat_launch_override("magician-chat-general");
        let (shared, launch) = split_launch_override(&marked).expect("marked");
        assert_eq!(shared, "magician-chat-general");
        assert_eq!(launch.len(), 12);
        assert_eq!(split_launch_override("magician-chat-general"), None);
        assert_eq!(split_launch_override("flat-browser-e2e-test"), None);
        assert_eq!(split_launch_override("x~not ok"), None);
    }
}
