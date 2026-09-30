//! A directory of raw mail, as a [`BounceMailbox`].
//!
//! # Why a directory and not a provider client
//!
//! [`super::pull`] states the reason it left the port unimplemented: writing an
//! AgentMail-shaped poller there would tie the bounce path to one provider's
//! API for a channel that predates it by thirty years, and it would have to be
//! written again for the second mailbox. That argument holds. It is also not an
//! argument for having **no** implementor, which is the state it left behind —
//! a bridge that was complete and inert, so every live send landed in
//! `dispatch_unknown` and stayed there.
//!
//! A directory is the implementor that does not pick a provider. Every way a
//! bounce can reach a host ends in something that can write a file: a
//! postmaster alias piped to a script, `fetchmail` or `getmail` into a maildir,
//! an `.eml` dropped by a webhook receiver, a shared volume a mail appliance
//! writes to. So this reads *"whatever is in that directory"* and stays as
//! ignorant of AgentMail as the parser above it is of RFC 3464's publisher.
//!
//! A provider-native reader is still worth building when one deployment's
//! mailbox cannot be reflected onto a filesystem. It would be a second file
//! implementing the same three methods, not an edit to this one or to the
//! bridge.
//!
//! # Settling renames, and does not delete
//!
//! [`BounceMailbox::settle`]'s contract, honoured literally: a settled report
//! moves to `<handle>.settled` beside itself. A hard bounce is not operationally
//! liftable, so the evidence has to outlive the decision that used it —
//! whoever later doubts a suppression must be able to read the message that
//! produced it.
//!
//! # Fail closed, and specifically: unreadable is never empty
//!
//! An unreadable directory propagates as an error. It is the whole reason this
//! type exists rather than a `read_dir(...).unwrap_or_default()` at the call
//! site: a mailbox that cannot be read and a week with no bounces produce the
//! same empty list and mean opposite things, and the second is the reading that
//! lets a dead address stay sendable.
//!
//! Two narrower cases follow the same rule in the other direction, and are
//! **not** errors:
//!
//! - **The directory does not exist yet.** That is a deployment whose first
//!   bounce has not arrived, not a fault, so it reads as empty — and it is the
//!   one absence that is genuinely "nothing new".
//! - **One unreadable file among readable ones.** Skipped with a warning and
//!   counted by its absence rather than failing the batch, because one message
//!   with a permission problem must not stop every other bounce in the
//!   directory from being recorded.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::magician_v2::delivery::DeliveryScope;

use super::pull::{BounceMailbox, InboundMail};

const LOG_TARGET: &str = "delivery_receipts::maildir";

/// The extension a message must carry to be offered.
///
/// Named rather than "everything in the directory" so that a `.settled` file,
/// an editor's swap file, and a partially-written download are all invisible.
/// The last is the load-bearing one: a writer that creates the file and then
/// fills it would otherwise be read mid-write, parsed as unrecognisable, and
/// counted as `unreadable` for a message that was about to be fine.
const MESSAGE_EXTENSION: &str = "eml";

/// The suffix a settled message takes.
const SETTLED_SUFFIX: &str = ".settled";
/// The most messages one `unread` will read into memory.
///
/// Bounces accumulate until they settle, so a mailbox whose sweep has been down
/// holds every message it ever received. Reading that whole backlog into memory
/// at once is how the sweep that would have drained it dies instead. The order
/// is deterministic and settling removes a message from the set, so a capped
/// pass drains a backlog of SETTLEABLE messages across passes.
///
/// It does not drain what the sweep deliberately never settles: a report that
/// claims to be a delivery notification and cannot be read, and one whose
/// recipients correlate to nothing, are both left in place for a person
/// (`delivery_receipts::pull`). Those keep their names, so they keep their
/// place in this sorted order for ever, and a mailbox holding this many of them
/// offers the same messages every pass and never reaches what is behind them.
/// So the cap takes the oldest half AND the newest half rather than a prefix:
/// newly arrived mail is visible however jammed the head is, and the stuck head
/// still appears every pass so a person sees what needs clearing. What a capped
/// pass does not read is the MIDDLE, and the warning below says so.
const MAX_MESSAGES_PER_PASS: usize = 1000;
/// How many settled names to try before deciding the mailbox needs a person.
const MAX_SETTLE_ATTEMPTS: u32 = 1000;

/// A directory of `.eml` files that delivery status notifications land in.
#[derive(Debug, Clone)]
pub struct MaildirBounceMailbox {
    dir: PathBuf,
    name: String,
}

impl MaildirBounceMailbox {
    /// `name` is what the health line reports. It is reported, never parsed —
    /// but it is what an operator reads to tell which of two mailboxes went
    /// quiet, so it should say where this one is.
    pub fn new(dir: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            name: name.into(),
        }
    }

    /// The mailbox at `dir`, named after it.
    pub fn at(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let name = format!("maildir:{}", dir.display());
        Self::new(dir, name)
    }

    /// Refuse a handle that could leave the directory.
    ///
    /// The handle comes back from a caller that got it from `unread`, so in the
    /// normal path it is a bare filename this type produced. It is checked
    /// anyway: `settle` renames, and a handle carrying a separator or a parent
    /// segment would rename a file outside the mailbox. Cheap to check, and the
    /// alternative is trusting a round-trip through a coordinator that has no
    /// reason to know the handle is a path.
    /// Returns the trimmed handle alongside its path.
    ///
    /// Both, because `settle` builds a SECOND file name from the same handle,
    /// and deriving that name from the raw argument while the path came from
    /// the trimmed one is how a message ends up settled under a name that does
    /// not match the one it was read as.
    fn message_path<'h>(&self, handle: &'h str) -> Result<(&'h str, PathBuf)> {
        let trimmed = handle.trim();
        if trimmed.is_empty()
            || trimmed.contains('/')
            || trimmed.contains('\\')
            || trimmed.contains("..")
        {
            anyhow::bail!(
                "`{handle}` is not a message handle this mailbox issued; a handle that names a \
                 path would settle a file outside the mailbox"
            );
        }
        Ok((trimmed, self.dir.join(trimmed)))
    }
}

impl BounceMailbox for MaildirBounceMailbox {
    fn name(&self) -> &str {
        &self.name
    }

    fn unread(&self, _scope: &DeliveryScope, _now: DateTime<Utc>) -> Result<Vec<InboundMail>> {
        // Absent is "nothing new"; unreadable is an error. See the module note:
        // these are the two cases that look identical from an empty list.
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading the bounce mailbox at {}", self.dir.display())
                })
            },
        };

        // Only the messages, and filtered before the vector is built rather
        // than after it is sorted. A settled report keeps its name for ever —
        // nothing in this tree ever removes one — so a mailbox that has been
        // running for a while holds far more `.settled` files than unread
        // `.eml` ones, and collecting plus sorting all of them makes the cost
        // of a pass proportional to everything the mailbox has ever received
        // rather than to what this pass can offer.
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let path = entry
                .with_context(|| format!("listing the bounce mailbox at {}", self.dir.display()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some(MESSAGE_EXTENSION) {
                paths.push(path);
            }
        }
        // Sorted so a batch is deterministic. Nothing downstream depends on
        // order — correlation is per message — but a reproducible batch is what
        // makes a health count reproducible when somebody re-runs a tick to
        // understand one.
        paths.sort();

        // A SPLIT window, not a prefix. The oldest half of the cap and the
        // newest half, when the mailbox holds more than one pass can carry.
        //
        // A plain prefix is FIFO, which is right for bounces and wrong here for
        // one reason: the sweep deliberately never settles some messages — an
        // unreadable report, one that correlates to nothing — and those hold
        // their place in a sorted order for ever. Enough of them at the head
        // and every pass reads the same stuck messages and never reaches what
        // is behind them. A bounce that never surfaces is an address we go on
        // mailing after it bounced, which is the exact harm this subsystem
        // exists to prevent, and it happens silently.
        //
        // Taking both ends means the newest mail is ALWAYS visible however
        // jammed the head is, while the stuck head still shows up every pass so
        // a person sees what needs clearing. Deterministic, and no state.
        let paths = if paths.len() > MAX_MESSAGES_PER_PASS {
            let newest = MAX_MESSAGES_PER_PASS / 2;
            let oldest = MAX_MESSAGES_PER_PASS - newest;
            let tail = paths.split_off(paths.len() - newest);
            paths.truncate(oldest);
            paths.extend(tail);
            paths
        } else {
            paths
        };

        let mut out = Vec::new();
        for path in paths {
            let Some(handle) = path.file_name().and_then(|name| name.to_str()) else {
                warn!(
                    target: LOG_TARGET,
                    path = %path.display(),
                    "a bounce mailbox entry has a name this platform cannot render as UTF-8; \
                     skipped, and left in place for a person to look at"
                );
                continue;
            };
            if out.len() >= MAX_MESSAGES_PER_PASS {
                warn!(
                    target: LOG_TARGET,
                    dir = %self.dir.display(),
                    limit = MAX_MESSAGES_PER_PASS,
                    "the bounce mailbox holds more than one pass can carry. This pass read the \
                     oldest and the newest half of the cap, so newly arrived mail is being \
                     seen — but the middle was not read at all, and a mailbox that stays at \
                     this limit is one a person has to empty. Anything the sweep never settles \
                     — an unreadable report, one that correlates to nothing — holds its place \
                     in this order for ever"
                );
                break;
            }
            match std::fs::read_to_string(&path) {
                Ok(raw) => out.push(InboundMail {
                    handle: handle.to_string(),
                    raw,
                }),
                // One bad file must not stop the batch. Left in place, so it is
                // still there when somebody looks — and still unsettled, so it
                // is offered again on the next tick if the cause was transient.
                Err(error) => warn!(
                    target: LOG_TARGET,
                    path = %path.display(),
                    "a message in the bounce mailbox could not be read and was skipped: {error}"
                ),
            }
        }
        Ok(out)
    }

    fn settle(&self, _scope: &DeliveryScope, handle: &str, _now: DateTime<Utc>) -> Result<()> {
        let (handle, from) = self.message_path(handle)?;
        // `rename` REPLACES the destination on Unix. If a message arrives under a
        // name that was already settled once — a sender reusing ids, an operator
        // dropping a file by hand — settling it would delete the earlier bounce,
        // which is the evidence its suppression rests on. So the destination is
        // found, not assumed, and a name already taken is stepped past rather
        // than written over.
        let mut to = self.dir.join(format!("{handle}{SETTLED_SUFFIX}"));
        let mut attempt = 1u32;
        while to.exists() {
            to = self.dir.join(format!("{handle}{SETTLED_SUFFIX}.{attempt}"));
            attempt += 1;
            if attempt > MAX_SETTLE_ATTEMPTS {
                anyhow::bail!(
                    "settling {handle} in the bounce mailbox at {} would need more than \
                     {MAX_SETTLE_ATTEMPTS} names; the mailbox is not draining and wants a person",
                    self.dir.display()
                );
            }
        }
        // A bare `fs::rename`, and correct as written — see the identical note
        // in `delivery_hygiene::worker`'s test fixture. The store-durability
        // rule counts hand-rolled ATOMIC WRITES, where a rename publishes new
        // contents over a store file and can outrun their durability. Nothing
        // is written here: an existing message moves aside to mark it settled,
        // and the shared helper cannot express that because it publishes bytes.
        std::fs::rename(&from, &to).with_context(|| {
            format!(
                "settling {} in the bounce mailbox at {}",
                handle,
                self.dir.display()
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> DeliveryScope {
        DeliveryScope::new("alpha", "prod")
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap()
    }

    /// The load-bearing distinction: a mailbox that has never received anything
    /// is empty, and one that cannot be read is an ERROR.
    ///
    /// Folding the second into the first is how a broken mount reports a week
    /// with no bounces, and a dead address stays sendable because of it.
    #[test]
    fn a_missing_directory_is_empty_and_an_unreadable_one_is_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let missing = MaildirBounceMailbox::at(tmp.path().join("never-created"));
        assert!(
            missing
                .unread(&scope(), now())
                .expect("absent is fine")
                .is_empty(),
            "a mailbox whose first bounce has not arrived is empty, not broken"
        );

        // A file where the directory should be: readable path, unreadable
        // directory. `read_dir` answers `NotADirectory`, which is not
        // `NotFound`, so it must propagate.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write");
        let broken = MaildirBounceMailbox::at(&blocked);
        assert!(
            broken.unread(&scope(), now()).is_err(),
            "a mailbox that cannot be read must not answer `nothing new`"
        );
    }

    /// Only `.eml` is offered, and settling renames rather than deletes.
    #[test]
    fn it_offers_messages_once_and_keeps_them_after_settling() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("bounces");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("b-1.eml"), b"raw one").expect("write");
        std::fs::write(dir.join("b-2.eml"), b"raw two").expect("write");
        // A half-written download and an already-settled report are both
        // invisible; the first is why the extension is checked at all.
        std::fs::write(dir.join("b-3.eml.part"), b"still arriving").expect("write");
        std::fs::write(dir.join("b-0.eml.settled"), b"already handled").expect("write");

        let mailbox = MaildirBounceMailbox::at(&dir);
        let held = mailbox.unread(&scope(), now()).expect("read");
        assert_eq!(
            held.iter()
                .map(|mail| mail.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["b-1.eml", "b-2.eml"],
            "only complete messages are offered, in a deterministic order"
        );

        mailbox.settle(&scope(), "b-1.eml", now()).expect("settle");
        let after = mailbox.unread(&scope(), now()).expect("read");
        assert_eq!(
            after
                .iter()
                .map(|mail| mail.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["b-2.eml"],
            "a settled message is not offered again"
        );
        assert!(
            dir.join("b-1.eml.settled").exists(),
            "settling must not delete the evidence a suppression rests on"
        );
    }

    /// A name already settled once is stepped past, never written over.
    #[test]
    fn settling_a_recycled_name_keeps_the_bounce_it_would_have_replaced() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("bounces");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // The same name, settled once already — a sender reusing ids, or a file
        // dropped by hand. `rename` replaces its destination, so the naive
        // settle would delete the first bounce to file the second.
        std::fs::write(dir.join("b-1.eml.settled"), b"the first bounce").expect("write");
        std::fs::write(dir.join("b-1.eml"), b"the second bounce").expect("write");

        let mailbox = MaildirBounceMailbox::at(&dir);
        mailbox.settle(&scope(), "b-1.eml", now()).expect("settle");

        assert_eq!(
            std::fs::read_to_string(dir.join("b-1.eml.settled")).expect("first"),
            "the first bounce",
            "settling must never overwrite evidence an earlier suppression rests on"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b-1.eml.settled.1")).expect("second"),
            "the second bounce",
            "and the message it was asked to settle is still settled, beside it"
        );
        assert!(
            mailbox.unread(&scope(), now()).expect("read").is_empty(),
            "a settled message is not offered again, whatever name it took"
        );
    }

    /// A handle that names a path cannot rename a file out of the mailbox.
    #[test]
    fn a_handle_naming_a_path_is_refused() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("bounces");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let outside = tmp.path().join("important.eml");
        std::fs::write(&outside, b"not a bounce").expect("write");

        let mailbox = MaildirBounceMailbox::at(&dir);
        for handle in ["../important.eml", "sub/other.eml", "  "] {
            assert!(
                mailbox.settle(&scope(), handle, now()).is_err(),
                "`{handle}` must be refused"
            );
        }
        assert!(outside.exists(), "nothing outside the mailbox was touched");
    }
}
