//! Bounded, indexed, cancellable folds over the append-only registers.
//!
//! Gate E4 of the apps-platform register
//! (`docs/plans/2026-09-03-apps_platform_open-gates.md`). The app-tool provider
//! pages the commitment shards and the transcript-claims log, but the stores
//! underneath fold the *whole* selected log before a page is cut. Two
//! properties of that fold were the reopen:
//!
//! 1. **Every transition re-scanned the rows folded so far.** A `Confirmed`,
//!    `Superseded` or `Withdrawn` record has to find the head row it acts on,
//!    and both folds did that with a linear search — so a register of `n`
//!    records cost `O(n²)` comparisons, which is what makes a large shard slow
//!    rather than merely large. [`StoreFoldIndex`] turns that lookup into one
//!    hash probe.
//! 2. **Nothing could stop a fold already running.** The provider isolates
//!    these synchronous reads on `spawn_blocking` so a large register cannot
//!    stall a Tokio worker — but dropping the join handle when the outer
//!    timeout fires does not stop the blocking thread. It runs to completion
//!    against a register nobody is waiting for, holding a whole log in memory
//!    the entire time. [`StoreFoldCursor`] observes a [`CancellationToken`] at
//!    a bounded interval so the fold can be abandoned.
//!
//! # The answer does not change
//!
//! This is a cost fix, not a semantics change. [`StoreFoldIndex`] enforces
//! exactly the rule the `seen` set it replaces enforced — the *first* append of
//! an id is the head row and a later duplicate is dropped — and it hands out
//! positions in append order, so a fold's output ordering is untouched. A fold
//! that is never cancelled returns byte-for-byte what it returned before.
//!
//! # A cancelled fold returns an error, never rows
//!
//! Cancellation stops the fold with [`StoreFoldCancelled`]. It never returns
//! the rows folded so far, because a partial fold is a *different* answer
//! rather than a smaller one: the register is a log of transitions, so stopping
//! early drops the `Withdrawn` record that ends a term while keeping the
//! `Recorded` one that opened it. A truncated read would report a dead
//! commitment as live and an answered claim as pending — the exact fail-open
//! `magician_v2::jsonl` refuses one layer down. The caller asked to stop, so it
//! is told the read stopped.
//!
//! # Why there is no record ceiling
//!
//! "Bounded" here is the bound on work between two cancellation observations,
//! and deliberately not a ceiling on how many records a fold may read. Both
//! answers to a register that exceeds a ceiling are wrong: truncating it is the
//! partial fold above, and refusing it takes a working read permanently offline
//! the moment a real relationship accumulates enough history. The bound that
//! belongs here is the one that lets a caller who no longer wants the answer
//! stop paying for it.

use std::collections::HashMap;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

/// How many folded records may pass between two cancellation observations.
///
/// Small enough that an abandoned fold stops promptly, large enough that the
/// atomic load does not show up beside the JSON parse that dominates the loop.
pub const FOLD_CANCELLATION_CHECK_RECORDS: usize = 256;

/// The error a cancelled fold returns.
///
/// A distinct type rather than a message so a caller can tell "you asked me to
/// stop" from "this log is corrupt" — the two demand opposite responses, and
/// only one of them means an operator has to look at the file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "folding the {register} was cancelled after {folded_records} records; a cancelled fold \
     returns no rows, because stopping early would drop the transitions that end a row and \
     report a closed one as open"
)]
pub struct StoreFoldCancelled {
    /// Which register was being read, so a log line names it.
    pub register: &'static str,
    /// How many records had been folded when the signal was observed.
    pub folded_records: usize,
}

/// Whether a store read failed because its fold was cancelled.
///
/// The one thing a caller needs from [`StoreFoldCancelled`] without caring
/// about its fields: a cancelled read is the caller's own doing and is not a
/// register fault to report, retry or escalate.
pub fn fold_was_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<StoreFoldCancelled>().is_some()
}

/// A cancellable cursor over one register fold.
///
/// Constructed per fold, threaded through the loop, and dropped with it. It
/// holds no borrow of the log so a store is free to keep folding exactly the
/// way it did before.
#[derive(Debug)]
pub struct StoreFoldCursor {
    register: &'static str,
    /// `None` is an uncancellable fold — the ordinary in-process read, which
    /// pays nothing for the ability to be abandoned.
    cancellation: Option<CancellationToken>,
    folded: usize,
}

impl StoreFoldCursor {
    /// A fold nobody can abandon: the in-process read path, unchanged.
    pub fn new(register: &'static str) -> Self {
        Self {
            register,
            cancellation: None,
            folded: 0,
        }
    }

    /// A fold that stops when `cancellation` fires.
    pub fn cancelled_by(register: &'static str, cancellation: &CancellationToken) -> Self {
        Self {
            register,
            cancellation: Some(cancellation.clone()),
            folded: 0,
        }
    }

    /// Observe cancellation at a phase boundary — before the log is read, and
    /// again before it is parsed.
    ///
    /// The per-record check below cannot cover either phase: both happen in one
    /// call, inside `magician_v2::jsonl`, and the parse of a large log is the
    /// longest uninterruptible stretch a fold has. Checking at the boundaries
    /// is what keeps a fold cancelled before it started from doing the I/O.
    pub fn checkpoint(&self) -> Result<()> {
        self.refuse_if_cancelled()
    }

    /// Admit one parsed record into the fold.
    ///
    /// Checks the first record and then every
    /// [`FOLD_CANCELLATION_CHECK_RECORDS`] after it. The first-record check is
    /// what makes a token that was already cancelled stop a fold at its first
    /// row rather than at its 256th.
    pub fn admit(&mut self) -> Result<()> {
        self.folded += 1;
        if self.folded == 1 || self.folded.is_multiple_of(FOLD_CANCELLATION_CHECK_RECORDS) {
            self.refuse_if_cancelled()?;
        }
        Ok(())
    }

    /// How many records this fold has admitted.
    pub fn folded_records(&self) -> usize {
        self.folded
    }

    fn refuse_if_cancelled(&self) -> Result<()> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(anyhow::Error::new(StoreFoldCancelled {
                register: self.register,
                folded_records: self.folded,
            }));
        }
        Ok(())
    }
}

/// Position index over the head rows a fold has accumulated.
///
/// Replaces both the `seen` set that enforced "first append of an id wins" and
/// the linear search that found the row a transition acts on. They were always
/// the same question asked twice.
#[derive(Debug, Default)]
pub struct StoreFoldIndex {
    slots: HashMap<String, usize>,
}

impl StoreFoldIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a head row, unless this id already has one.
    ///
    /// Rows are pushed *through* the index rather than beside it, so a slot can
    /// never name a position the row vector does not have. That invariant is
    /// the whole reason [`Self::head_mut`] may hand back a plain `Option`: a
    /// miss means the fold has not seen the head yet, never that the index and
    /// the rows disagree.
    ///
    /// Returns whether the row was appended; `false` is a duplicate id, which
    /// the fold drops exactly as it did before.
    pub fn push_head<T>(&mut self, rows: &mut Vec<T>, id: String, row: T) -> bool {
        if self.slots.contains_key(&id) {
            return false;
        }
        self.slots.insert(id, rows.len());
        rows.push(row);
        true
    }

    /// The head row a transition acts on, if the fold has seen its append.
    pub fn head_mut<'rows, T>(&self, rows: &'rows mut [T], id: &str) -> Option<&'rows mut T> {
        let position = *self.slots.get(id)?;
        rows.get_mut(position)
    }

    /// How many distinct head rows have been folded.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token that is already cancelled stops the fold at its FIRST record.
    /// Waiting for the interval boundary would make an abandoned read pay for
    /// up to 255 records it was told not to do.
    #[test]
    fn an_already_cancelled_fold_refuses_at_its_first_record() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut cursor = StoreFoldCursor::cancelled_by("commitment register", &cancellation);

        let error = cursor.admit().expect_err("a cancelled fold refuses");
        assert!(fold_was_cancelled(&error), "{error}");
        let cancelled = error
            .downcast_ref::<StoreFoldCancelled>()
            .expect("the typed refusal");
        assert_eq!(cancelled.register, "commitment register");
        assert_eq!(cancelled.folded_records, 1);
    }

    /// The bound, as behaviour: cancellation raised mid-fold is observed within
    /// one interval, not at the end of the log.
    #[test]
    fn cancellation_is_observed_within_the_bounded_interval() {
        let cancellation = CancellationToken::new();
        let mut cursor = StoreFoldCursor::cancelled_by("transcript claim register", &cancellation);

        for _ in 0..FOLD_CANCELLATION_CHECK_RECORDS {
            cursor.admit().expect("an uncancelled fold continues");
        }
        cancellation.cancel();

        let mut admitted_after_cancel = 0;
        let error = loop {
            match cursor.admit() {
                Ok(()) => {
                    admitted_after_cancel += 1;
                    assert!(
                        admitted_after_cancel <= FOLD_CANCELLATION_CHECK_RECORDS,
                        "a cancelled fold must stop within one check interval"
                    );
                },
                Err(error) => break error,
            }
        };
        assert!(fold_was_cancelled(&error), "{error}");
    }

    /// A fold with no token never refuses, however long it runs. The in-process
    /// read path must not acquire a new way to fail.
    #[test]
    fn an_uncancellable_fold_never_refuses() {
        let mut cursor = StoreFoldCursor::new("commitment register");
        for _ in 0..(FOLD_CANCELLATION_CHECK_RECORDS * 4) {
            cursor.admit().expect("no token, no refusal");
        }
        assert_eq!(cursor.folded_records(), FOLD_CANCELLATION_CHECK_RECORDS * 4);
        cursor.checkpoint().expect("no token, no refusal");
    }

    /// The checkpoint is what stops a fold that was cancelled before it began,
    /// so the log is never read at all.
    #[test]
    fn a_checkpoint_refuses_before_the_log_is_read() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cursor = StoreFoldCursor::cancelled_by("commitment register", &cancellation);

        let error = cursor.checkpoint().expect_err("cancelled before the read");
        assert!(fold_was_cancelled(&error), "{error}");
        assert_eq!(
            error
                .downcast_ref::<StoreFoldCancelled>()
                .expect("typed")
                .folded_records,
            0,
            "nothing was folded, and the message says so"
        );
    }

    /// A store fault is not a cancellation, and the discriminator has to hold
    /// for the caller that reacts differently to each.
    #[test]
    fn an_ordinary_store_error_is_not_a_cancellation() {
        let error = anyhow::anyhow!("unparseable record at log:7 — corruption");
        assert!(!fold_was_cancelled(&error));
    }

    /// The index keeps the FIRST append of an id and drops later ones — the
    /// rule the `seen` set enforced — and finds that row in place.
    #[test]
    fn the_index_keeps_the_first_append_and_finds_it_in_place() {
        let mut index = StoreFoldIndex::new();
        let mut rows: Vec<&str> = Vec::new();

        assert!(index.push_head(&mut rows, "a".to_string(), "first a"));
        assert!(index.push_head(&mut rows, "b".to_string(), "first b"));
        assert!(
            !index.push_head(&mut rows, "a".to_string(), "second a"),
            "a duplicate append is dropped, not folded"
        );

        assert_eq!(
            rows,
            vec!["first a", "first b"],
            "append order is preserved"
        );
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.head_mut(&mut rows, "a").map(|row| *row),
            Some("first a")
        );
        assert_eq!(
            index.head_mut(&mut rows, "b").map(|row| *row),
            Some("first b")
        );
        assert!(
            index.head_mut(&mut rows, "c").is_none(),
            "a transition against an id with no head row finds nothing"
        );
    }

    /// A transition mutates the row in the vector, not a copy of it.
    #[test]
    fn the_indexed_row_is_the_folded_row() {
        let mut index = StoreFoldIndex::new();
        let mut rows: Vec<String> = Vec::new();
        index.push_head(&mut rows, "a".to_string(), "open".to_string());
        index.push_head(&mut rows, "b".to_string(), "open".to_string());

        *index.head_mut(&mut rows, "b").expect("head") = "withdrawn".to_string();
        assert_eq!(rows, vec!["open".to_string(), "withdrawn".to_string()]);
    }

    /// An empty index is empty, and asking it anything is a miss rather than a
    /// panic.
    #[test]
    fn an_empty_index_answers_nothing() {
        let index = StoreFoldIndex::new();
        let mut rows: Vec<u8> = Vec::new();
        assert!(index.is_empty());
        assert!(index.head_mut(&mut rows, "a").is_none());
    }
}
