//! A directory of `.eml` files, as a [`RunInbox`].
//!
//! The same argument as `delivery_receipts::maildir`, and the same shape: every
//! way a message reaches a host ends in something that can write a file, so a
//! directory commits to no provider's API. A provider-native reader is a second
//! implementor of the same two methods, never an edit to the sweep.
//!
//! # It reads and never consumes
//!
//! [`RunInbox`] has no `settle`, and this honours that literally: nothing is
//! renamed, moved or deleted. A run inbox is an ORDINARY mailbox — it carries
//! the verification code and every other message that person receives — so
//! consuming what we walked past would take mail that was never ours. Safe
//! because the run store is idempotent on `event_ref`; re-reading writes
//! nothing.
//!
//! # The filename is the event ref
//!
//! Which is what makes re-reading a no-op: the same file yields the same ref,
//! and the store recognises it as the event that already closed the wait. A ref
//! derived from the message's contents would change with a re-encode, and a run
//! would then be told its verification had arrived twice.
//!
//! # The arrival time is the message's, not the sweep's
//!
//! From the file's modification time, because that is when the message landed.
//! Using the sweep clock would record every verification as having arrived at
//! whatever moment somebody happened to poll — and the run's own record is what
//! a later reader consults to ask *"when did this land"*. The same rule
//! `scheduling` states for an inbound reply: **the message's own time, never
//! this surface's clock.**

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::run_state::{InboundEvent, RunScope};

use super::RunInbox;

const LOG_TARGET: &str = "run_inbox::maildir";

/// The extension a message must carry to be offered.
///
/// Named rather than "everything in the directory" so a partially-written
/// download is invisible. That is the load-bearing case: a writer that creates
/// the file and then fills it would otherwise be offered mid-write, and a run
/// told its verification arrived when half of it had.
const MESSAGE_EXTENSION: &str = "eml";

/// The most messages one pass will offer.
///
/// Needed because this mailbox is never consumed — that is the whole point, see
/// the module note — so it grows for as long as mail arrives and every sweep
/// would otherwise re-read all of it. Unbounded work per pass on a directory
/// nobody prunes is a watch that gets slower until somebody turns it off.
///
/// Truncation is **reported, never silent**: a capped pass says how many it did
/// not look at, so a mailbox outgrowing the cap reads as the operational fact it
/// is — prune it, or archive it — rather than as a verification that never
/// arrived.
const MAX_MESSAGES_PER_PASS: usize = 1000;

/// A directory of `.eml` files a run's verification may arrive in.
#[derive(Debug, Clone)]
pub struct MaildirRunInbox {
    dir: PathBuf,
    /// The `source_hint` every message from this directory is reported under.
    ///
    /// Supplied, never derived from the path: it has to match the hint a run
    /// raised its wait with, and that is a human's word for "my inbox" rather
    /// than a filesystem location. Deriving it would make the match depend on
    /// where the mail happens to be spooled.
    source_hint: String,
    name: String,
}

impl MaildirRunInbox {
    pub fn new(dir: impl Into<PathBuf>, source_hint: impl Into<String>) -> Self {
        let dir = dir.into();
        let source_hint = source_hint.into();
        let name = format!("maildir:{}", dir.display());
        Self {
            dir,
            source_hint,
            name,
        }
    }

    /// The directory this reads. Exposed for the report, not for callers to
    /// address files by.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl RunInbox for MaildirRunInbox {
    fn name(&self) -> &str {
        &self.name
    }

    fn messages(&self, _scope: &RunScope, now: DateTime<Utc>) -> Result<Vec<InboundEvent>> {
        // Absent is "nothing to look at"; unreadable is an error. A mailbox
        // whose first message has not arrived and one that cannot be opened
        // produce the same empty list and mean opposite things, and only the
        // second leaves a run waiting on a verification that is sitting there.
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading the run inbox at {}", self.dir.display()))
            },
        };

        let paths: Vec<PathBuf> = entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("listing the run inbox at {}", self.dir.display()))?;

        let mut candidates: Vec<(DateTime<Utc>, String, PathBuf)> = Vec::new();
        for path in paths {
            if path.extension().and_then(|ext| ext.to_str()) != Some(MESSAGE_EXTENSION) {
                continue;
            }
            let Some(handle) = path.file_name().and_then(|name| name.to_str()) else {
                warn!(
                    target: LOG_TARGET,
                    path = %path.display(),
                    "a run-inbox entry has a name this platform cannot render as UTF-8; it is \
                     skipped and left in place for a person to look at"
                );
                continue;
            };
            candidates.push((arrived_at(&path, now), handle.to_string(), path));
        }

        // Oldest first, then by name. Oldest because a wait that has been open
        // longest is the one most likely to have been missed — the same
        // ordering `expectations_awaiting` uses. By name as a tie-break because
        // filesystems report the same mtime for files written in the same
        // instant, and an unstable order would make two passes over an
        // unchanged directory disagree about what they saw.
        // NEWEST first, and the direction is the whole correctness of the cap.
        //
        // Nothing is ever settled or removed here — a run inbox is an ORDINARY
        // mailbox, and this reader does not own it. So with oldest-first the cap
        // selects the same 1000 messages on every pass for ever: once a real
        // person's inbox holds more than the cap, the verification code that
        // arrived a minute ago is permanently invisible, which is the one thing
        // this reader exists to find. Newest-first makes the cap drop the
        // ancient tail instead, and what it drops is the part no live
        // expectation is waiting on.
        //
        // Ties break on the handle DESCENDING too, so one pass over an
        // unchanged directory always offers the same set in the same order.
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));

        let skipped = candidates.len().saturating_sub(MAX_MESSAGES_PER_PASS);
        if skipped > 0 {
            warn!(
                target: LOG_TARGET,
                dir = %self.dir.display(),
                "the run inbox holds {} messages and one pass offers {MAX_MESSAGES_PER_PASS}; \
                 the {skipped} OLDEST were not looked at. Nothing is consumed here by design, \
                 so this mailbox grows until somebody prunes or archives it — an expectation \
                 waiting on something older than the cap will not be met.",
                candidates.len()
            );
        }

        Ok(candidates
            .into_iter()
            .take(MAX_MESSAGES_PER_PASS)
            .map(|(at, handle, _)| InboundEvent {
                event_ref: handle,
                source_hint: self.source_hint.clone(),
                at,
            })
            .collect())
    }
}

/// When the message landed, from the file's modification time.
///
/// Falls back to the sweep clock when the filesystem cannot say — some can't,
/// and a message whose time is unknown is still a message that arrived. The
/// fallback is the only thing here that is approximate, and it is louder than
/// silently stamping every message with the poll time, which is what this
/// replaced.
fn arrived_at(path: &Path, fallback: DateTime<Utc>) -> DateTime<Utc> {
    match std::fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => DateTime::<Utc>::from(modified),
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                path = %path.display(),
                "this filesystem cannot say when the message arrived, so the sweep clock is \
                 recorded instead: {error}"
            );
            fallback
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RunScope {
        RunScope::new("anonymous", "default")
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap()
    }

    /// A missing directory is empty; an unreadable one is an ERROR.
    ///
    /// Folding the second into the first leaves a run waiting forever on a
    /// verification that is sitting in a mailbox nobody could open.
    #[test]
    fn a_missing_directory_is_empty_and_an_unreadable_one_is_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let missing = MaildirRunInbox::new(tmp.path().join("never-created"), "inbox");
        assert!(missing
            .messages(&scope(), now())
            .expect("absent is fine")
            .is_empty());

        // A file where the directory should be: `read_dir` answers
        // `NotADirectory`, which is not `NotFound`, so it must propagate.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write");
        assert!(MaildirRunInbox::new(&blocked, "inbox")
            .messages(&scope(), now())
            .is_err());
    }

    /// A pass is capped, and a capped pass says what it did not look at.
    ///
    /// The cap exists because nothing here is ever consumed — the mailbox grows
    /// for as long as mail arrives. Silently truncating would make a
    /// verification sitting past the cap indistinguishable from one that never
    /// arrived, which is the failure this whole module exists to end.
    #[test]
    fn a_pass_is_capped_and_the_cap_is_not_silent() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&dir).expect("mkdir");
        for index in 0..(MAX_MESSAGES_PER_PASS + 5) {
            std::fs::write(dir.join(format!("m-{index:05}.eml")), b"x").expect("write");
        }
        let held = MaildirRunInbox::new(&dir, "inbox")
            .messages(&scope(), now())
            .expect("read");
        assert_eq!(
            held.len(),
            MAX_MESSAGES_PER_PASS,
            "one pass offers at most the cap"
        );
        assert!(
            held.iter()
                .any(|m| m.event_ref == format!("m-{:05}.eml", MAX_MESSAGES_PER_PASS + 4)),
            "the cap must drop the OLDEST, never the newest: nothing settles here, so a cap \
             taking the oldest would hide every message that arrived after the mailbox filled \
             — permanently, and silently"
        );
        assert!(
            dir.read_dir().expect("list").count() > MAX_MESSAGES_PER_PASS,
            "and consumes none of them"
        );
    }

    /// Only complete messages are offered, the filename is the ref, and nothing
    /// is consumed.
    #[test]
    fn it_offers_complete_messages_by_name_and_leaves_them_in_place() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("m-1.eml"), b"one").expect("write");
        std::fs::write(dir.join("m-2.eml"), b"two").expect("write");
        // Still arriving: invisible, which is why the extension is checked.
        std::fs::write(dir.join("m-3.eml.part"), b"half").expect("write");

        let inbox = MaildirRunInbox::new(&dir, "agent@example.com");
        let held = inbox.messages(&scope(), now()).expect("read");
        assert_eq!(
            held.iter()
                .map(|m| m.event_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["m-2.eml", "m-1.eml"],
            "complete messages only, NEWEST first. The direction is the cap's correctness: \
             nothing is ever settled here, so oldest-first would select the same messages on \
             every pass for ever and a code that arrived a minute ago would never be seen"
        );
        assert!(held.iter().all(|m| m.source_hint == "agent@example.com"));
        assert!(
            held.iter().all(|m| m.at != now()),
            "the arrival time is the message's own, not the clock the sweep was called with"
        );

        // Read twice: the same refs, and every file still there. The store's
        // idempotency is what makes that safe, and this is the half that has to
        // hold up its end by not consuming anything.
        let again = inbox.messages(&scope(), now()).expect("read");
        assert_eq!(held, again);
        assert!(dir.join("m-1.eml").exists() && dir.join("m-2.eml").exists());
    }
}
