//! Durable "voice has already said this" log.
//!
//! A pending diff approval is derived from the `CodeChangeProposal` store on
//! every poll, because that is the only shape of state that survives a
//! restart (see `TaskListItemV3::awaiting_diff_approval`). Deriving is exactly
//! what makes the announcement hard: the derived fact does not change until
//! the user acts on it, so a derive-and-announce loop would say the same
//! sentence on every poll for as long as the diff sits there.
//!
//! The suppression therefore has to be durable too. An in-memory set would
//! make the announcement fire again after every restart — and a restart is a
//! *likely* event during a multi-hour coding run, so the repeat would not be
//! rare. One empty marker file per proposal id, written only after the
//! announcement was actually handed to a live call, is the whole mechanism.
//!
//! **What this costs, stated plainly.** Once a proposal has been announced it
//! is never announced again, not even to a brand-new call hours later. The
//! card is what carries the state forward; the sentence is a one-time nudge.
//! The alternative — re-announcing per call — turns "you have a diff waiting"
//! into something the user hears every time they pick the phone up, which is
//! how a useful notification becomes noise.
//!
//! The marker is written *after* delivery, never before, so a proposal that
//! nobody was listening for stays un-marked and is announced on the first poll
//! that finds a live call. The failure direction is deliberate: a lost marker
//! write costs one repeated sentence; a marker written before delivery would
//! cost the announcement entirely.

use std::path::{Path, PathBuf};

use super::io::write_bytes_durably_sync;

/// Per-scope record of which HITL prompts voice has already spoken.
pub struct SpokenHitlLog {
    root: PathBuf,
}

impl SpokenHitlLog {
    /// Open (lazily — nothing is created until something is marked) the log
    /// under a scope root, beside `code_change_proposals/` that it dedupes
    /// against. Same scope root the proposal store is constructed from, so a
    /// process restarted over the same data reads the same markers.
    pub fn new(scope_root: impl Into<PathBuf>) -> Self {
        Self {
            root: scope_root.into().join("spoken_hitl_announcements"),
        }
    }

    /// True when this diff approval has already been spoken to a live call.
    ///
    /// Best-effort in the safe direction: an unreadable log answers `false`,
    /// which risks one repeated sentence rather than silently swallowing a
    /// prompt the user is waiting on.
    pub fn diff_approval_was_announced(&self, proposal_id: &str) -> bool {
        match marker_file_name(proposal_id) {
            Some(name) => self.root.join(name).is_file(),
            None => false,
        }
    }

    /// Record that this diff approval has been spoken. Idempotent.
    ///
    /// The marker is empty on purpose: the proposal id *is* the fact, and a
    /// body would be one more thing that could be half-written. Errors are
    /// returned rather than swallowed so the caller can log them — but the
    /// caller must not treat a failed mark as a failed announcement, because
    /// the announcement already happened.
    pub fn record_diff_approval_announced(&self, proposal_id: &str) -> std::io::Result<()> {
        let Some(name) = marker_file_name(proposal_id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("proposal id {proposal_id:?} is not usable as a marker file name"),
            ));
        };
        std::fs::create_dir_all(&self.root)?;
        write_marker(&self.root.join(name))
    }
}

/// A proposal id is `ccp-<uuid>` and `CodeChangeProposalId::parse` already
/// restricts the character set to `[A-Za-z0-9_-]`, so it is directly usable as
/// a file name. This re-checks rather than trusting that, because the value
/// arrives here off disk: a hand-edited proposal JSON must not be able to name
/// a path outside the log.
fn marker_file_name(proposal_id: &str) -> Option<String> {
    if proposal_id.is_empty() || proposal_id.len() > 128 {
        return None;
    }
    if !proposal_id
        .chars()
        .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_'))
    {
        return None;
    }
    Some(format!("{proposal_id}.spoken"))
}

/// The marker carries no bytes, so the temp-and-rename this replaced was never
/// protecting content — a zero-byte publish cannot tear. What the marker needs
/// is for its *directory entry* to survive an unclean shutdown, which is the
/// half the hand-rolled sequence was missing: the module above calls this log
/// durable, and a restart is a likely event during a multi-hour coding run, so
/// an entry lost to a crash re-announces a diff the user was already told
/// about. The shared writer is used whole rather than open-coding a create +
/// two syncs; the marker is written once per proposal, so its staging file
/// costs nothing. It also drops the fixed `<id>.spoken.tmp` staging name, which
/// every concurrent writer of the same proposal id shared.
fn write_marker(path: &Path) -> std::io::Result<()> {
    write_bytes_durably_sync(path, b"")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn a_marked_proposal_reads_as_announced_from_a_freshly_opened_log() {
        // The restart case, which is the only one worth testing here: the
        // second `SpokenHitlLog` shares nothing with the first except the
        // directory, exactly as a restarted process does.
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let first = SpokenHitlLog::new(tmp.path());
        assert!(!first.diff_approval_was_announced("ccp-1"));
        first
            .record_diff_approval_announced("ccp-1")
            .expect("marker write");

        let after_restart = SpokenHitlLog::new(tmp.path());
        assert!(
            after_restart.diff_approval_was_announced("ccp-1"),
            "suppression that does not survive a restart re-announces every \
             pending diff on every boot"
        );
        assert!(
            !after_restart.diff_approval_was_announced("ccp-2"),
            "the log is keyed per proposal, so a second diff is still unspoken"
        );
    }

    /// `diff_approval_was_announced` answers on file *presence*, so a staging
    /// sibling left in the log directory is a second name for the same fact
    /// that the reader can never see through — and a re-mark must not
    /// accumulate them.
    #[test]
    fn marking_leaves_no_staging_sibling_in_the_log_directory() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let log = SpokenHitlLog::new(tmp.path());
        log.record_diff_approval_announced("ccp-1")
            .expect("marker write");
        log.record_diff_approval_announced("ccp-1")
            .expect("idempotent re-mark");

        let entries = std::fs::read_dir(tmp.path().join("spoken_hitl_announcements"))
            .expect("log listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec!["ccp-1.spoken".to_string()],
            "the marker must be the only file the log directory holds"
        );
        assert!(log.diff_approval_was_announced("ccp-1"));
    }

    #[test]
    fn a_proposal_id_that_could_escape_the_log_directory_is_refused() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let log = SpokenHitlLog::new(tmp.path());
        assert!(log.record_diff_approval_announced("../escape").is_err());
        assert!(!log.diff_approval_was_announced("../escape"));
        assert!(log.record_diff_approval_announced("").is_err());
    }
}
