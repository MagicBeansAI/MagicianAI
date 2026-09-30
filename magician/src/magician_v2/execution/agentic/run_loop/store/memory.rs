//! The loop state store, in memory.
//!
//! # Why this exists, given the filesystem impl does
//!
//! Two reasons, and neither is "for tests".
//!
//! **It is the in-process driver's store.** The design's cutover is a strangler
//! behind an execution-driver flag, and the property that makes parity
//! meaningful is that *"both drivers compile and share the same phase functions —
//! the in-process driver becomes a trivial loop calling them in sequence with an
//! in-memory store"*. Without this the in-process driver would either carry a
//! different code path or write to disk for work that never leaves the process.
//!
//! **It is the second implementation the contract suite needs.** A suite that
//! runs against one implementation proves that implementation, not the contract.
//! Two make the difference visible: every rule the suite asserts is asserted of
//! both stores from the same source, so one enforcing it and the other not is a
//! test failure rather than a surprise in Stage 5.
//!
//! # What it does not do
//!
//! **Survive the process.** That is the whole point of the other implementation,
//! and a caller that needs durability must not be handed this one by default.
//!
//! **Enforce the limits that exist because a file has to be read back.** The
//! filesystem store refuses a state, a lease or a key record too large for its
//! own bounded read, because writing one would leave an execution nothing could
//! load. Those ceilings are properties of reading bytes off a disk, and enforcing
//! them here would mean serializing on every commit — which is the cost the
//! in-process driver holds this store to avoid. The ones that are *not* about
//! bytes are shared: per-record encoding, the journal's record ceiling, the
//! ledger's row ceiling, the wake ledger's resolution ceiling, and every
//! conflict rule run through the same types.
//!
//! The wake ceiling joined that list late. It was declared in `fs` and enforced
//! only there, so a completer reporting without bound met a refusal in
//! production and none at all in the suite that is supposed to run both — which
//! is the exact shape of divergence this store exists to make impossible.
//!
//! So a driver that must run on both is bound by the filesystem store's limits,
//! and passing here is not evidence it stays inside them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;

use super::{
    non_resumable_terminal, ChainClosure, CommittedLoopState, EndedRun, ExecutionKey, LastLease,
    Lease, LoopStateStore, ParkedExecution, ParkedListing, Revision, RunnableScan, ScanCursor,
    StoreError, StoreResult, TerminalOutboxScan, MAX_DISCOVERY_VISITS_PER_PAGE,
    MAX_WAKE_RESOLUTIONS,
};
use crate::magician_v2::execution::agentic::run_loop::effects::{
    EffectId, EffectLedger, EffectLedgerEntry, EffectOutcome,
};
use crate::magician_v2::execution::agentic::run_loop::journal::{
    Journal, JournalAppend, JournalError, JournalRecord, ProjectorCursor, TerminalKind,
    MAX_JOURNAL_RECORDS,
};
use crate::magician_v2::execution::agentic::run_loop::state::{LoopState, WorkerId};

/// The most swept records one execution keeps for inspection.
///
/// The filesystem store caps its retained-attempt file for the same reason and
/// says so when it stops retaining: a run that crashes in a loop would otherwise
/// grow this without limit, and the attempt worth reading is the last one.
const MAX_RETAINED_SWEPT_RECORDS: usize = 4_096;

/// Everything one execution holds.
#[derive(Debug, Default)]
struct ExecutionSlot {
    revision: Revision,
    state: Option<LoopState>,
    journal: Vec<JournalRecord>,
    /// The attempts that were swept because they were never committed. Kept for
    /// the same reason the filesystem store keeps them in a file: an operator
    /// investigating a crash wants the attempt that did not commit.
    swept: Vec<JournalRecord>,
    effects: EffectLedger,
    /// The wall clock read by the commit that published [`Self::state`].
    ///
    /// The in-memory counterpart of the filesystem store's snapshot mtime, and
    /// it exists for one caller: `list_parked` has to be able to say how long a
    /// park has been sitting, and a reconciler with no clock on the park would
    /// either examine every park on every pass or never examine one at all.
    ///
    /// `None` until the first commit, which is the same thing a missing snapshot
    /// means on disk: age unknown, not age zero.
    committed_at_ms: Option<i64>,
    /// The non-resumable terminal this run's **committed** journal prefix ends
    /// on, published by the commit that made it authoritative.
    ///
    /// The in-memory counterpart of the filesystem store's `ended.json`, and it
    /// exists for the same caller: `list_runnable` must not offer a run that has
    /// ended, and a scan that answered that question by replaying each journal
    /// would be `O(records)` per key on the poll path.
    ///
    /// `None` is "no ending is published", which covers both a live run and one
    /// whose terminal record is still an uncommitted orphan.
    ///
    /// # Cleared by a commit whose prefix does not end on one
    ///
    /// An earlier version of this note said it was never cleared, on the ground
    /// that a non-resumable terminal is the last record its log can ever hold.
    /// That is a rule about what a *driver* does, not a property this store
    /// enforces: `append_journal` accepts a record after a committed terminal
    /// without complaint. When it happens, the prefix no longer ends on an
    /// ending. Exact watermark equality already makes the old marker invisible,
    /// and clearing it keeps the index a faithful description of the prefix,
    /// and therefore never quarantined by the check that would have made the
    /// damage visible. `commit` publishes and retracts, so the marker says what
    /// the committed prefix says and nothing else.
    ended: Option<EndedRun>,
    lease: Option<HeldLease>,
    /// Outstanding wake resolutions, by token.
    ///
    /// A set per token rather than a set of tokens: a completer that reports the
    /// same completion twice must be idempotent, while two DIFFERENT completions
    /// on one token are two things to wake for. A flat set of tokens collapsed
    /// the second case into the first, which is what made a second park on one
    /// token unwaitable.
    wakes: BTreeMap<String, BTreeSet<String>>,
    /// How far this run's event outbox has been emitted.
    ///
    /// `None` means nothing has projected yet, which is what a fresh execution
    /// answers and is **not** the same as a store that cannot say — see
    /// [`LoopStateStore::load_projector_cursor`].
    ///
    /// Held as the parsed value rather than as bytes. The filesystem store has
    /// to encode and re-read it, and that round trip is where a hand-edited
    /// window meets `ProjectorCursor`'s clamp; here there is no file to hand-edit
    /// and no bytes to re-read, so re-encoding on every save would buy nothing
    /// and would cost the commit path the allocation this store exists to avoid.
    projector: Option<ProjectorCursor>,
    /// The receipt saying no further segment follows this one.
    ///
    /// `None` covers a run still going, one that died before publishing, and
    /// one that never had a writer — which a reader must treat identically. See
    /// [`ChainClosure`].
    chain_closure: Option<ChainClosure>,
}

#[derive(Debug, Clone)]
struct HeldLease {
    worker: WorkerId,
    fence: u64,
    expires_at_ms: i64,
    released: bool,
}

/// A [`LoopStateStore`] that lives and dies with the process.
///
/// # Cloning yields a second handle, not a second store
///
/// Two `FsLoopStateStore`s over one directory are two handles onto one store,
/// and a clone of this one is the same relationship. A clone that forked the map
/// would be a store two workers could disagree about — and the contract case that
/// opens a second handle would be asserting nothing, since its "second handle"
/// would start empty and every assertion would be about the copy it just made.
/// Two independent stores are what [`MemoryLoopStateStore::new`] gives.
#[derive(Debug, Clone, Default)]
pub struct MemoryLoopStateStore {
    slots: Arc<Mutex<HashMap<ExecutionKey, ExecutionSlot>>>,
    mutation: Arc<tokio::sync::Mutex<()>>,
}

impl MemoryLoopStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The records swept from this execution because they were never committed.
    ///
    /// The in-memory counterpart of the filesystem store's `journal.swept.jsonl`
    /// and it exists for the same reason: an operator — or an in-process driver
    /// reporting on a crash — wants the attempt that did not commit, and a store
    /// that silently discarded it would make the orphan sweep unobservable from
    /// either implementation.
    pub fn swept_records(&self, key: &ExecutionKey) -> StoreResult<Vec<JournalRecord>> {
        let slots = self.slots()?;
        Ok(slots
            .get(key)
            .map(|slot| slot.swept.clone())
            .unwrap_or_default())
    }

    /// Take the map, refusing to serve a half-written one.
    ///
    /// # Poison fails closed, and that is deliberate
    ///
    /// A poisoned lock means some holder panicked part way through a mutation.
    /// The convention elsewhere in this codebase is to recover into the inner
    /// value, and for a cache that is right — the worst case is a stale entry.
    /// It is **not** right here: this store's whole contract is that what it
    /// returns is what was committed, and a map that was being mutated when its
    /// holder panicked cannot vouch for that. So it answers `Unavailable`, which
    /// is the same posture the filesystem store takes when the disk cannot be
    /// read, and the same posture the design asks for: *"refuse to advance rather
    /// than run unrecorded work"*.
    fn slots(
        &self,
    ) -> StoreResult<std::sync::MutexGuard<'_, HashMap<ExecutionKey, ExecutionSlot>>> {
        self.slots.lock().map_err(|_| StoreError::Unavailable {
            detail: "the in-memory loop state store was poisoned by a panicking holder".to_string(),
        })
    }

    fn validate_current_lease(&self, key: &ExecutionKey, lease: &Lease) -> StoreResult<()> {
        if &lease.key != key {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        let slots = self.slots()?;
        let current = slots
            .get(key)
            .and_then(|slot| slot.lease.as_ref())
            .ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released
            || current.fence != lease.fence
            || current.worker != lease.worker
            || current.expires_at_ms <= Utc::now().timestamp_millis()
        {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        Ok(())
    }
}

#[async_trait]
impl LoopStateStore for MemoryLoopStateStore {
    async fn load(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
        let slots = self.slots()?;
        let Some(slot) = slots.get(key) else {
            return Ok(None);
        };
        let Some(state) = slot.state.clone() else {
            return Ok(None);
        };
        if let Some(batch) = &state.pending {
            batch.validate()?;
        }
        Ok(Some(CommittedLoopState {
            revision: slot.revision,
            state,
        }))
    }

    async fn commit(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
    ) -> StoreResult<Revision> {
        if let Some(batch) = &state.pending {
            batch.validate()?;
        }
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        if slot.revision != expected {
            return Err(StoreError::Conflict {
                expected,
                found: slot.revision,
            });
        }
        let last_seq = slot.journal.last().map_or(0, |record| record.seq);
        if state.journal_seq > last_seq {
            return Err(StoreError::WatermarkAhead {
                watermark: state.journal_seq,
                last_seq,
            });
        }
        slot.revision = expected.next();
        slot.state = Some(state.clone());
        slot.committed_at_ms = Some(Utc::now().timestamp_millis());

        // The ending this commit made authoritative, published for the scan.
        //
        // Derived from the prefix the watermark vouches for, never from the
        // whole log: a `RunEnded` record sitting above the watermark is an
        // orphan left by a worker that appended and died, the run has not ended,
        // and publishing it here would strand a live execution.
        //
        // `authoritative` rather than `slot.journal.last()`, for that reason
        // alone — the two differ exactly when there is an orphan, which is the
        // case that matters.
        let authoritative_len = slot
            .journal
            .iter()
            .position(|record| record.seq > state.journal_seq)
            .unwrap_or(slot.journal.len());
        // Assigned in both directions, never only set. A marker is read against
        // the watermark of whatever state is committed NOW, so one left behind
        // by an earlier commit keeps answering for a prefix this commit has
        // replaced. Exact seq/watermark equality makes a stale marker inert;
        // clearing it also keeps the stored index truthful for operators. The
        // filesystem store retracts its file for the same reason.
        //
        // # This assignment CANNOT fail, and that is a coverage fact, not a win
        //
        // The marker and the state are one field and one field of the same slot,
        // under one mutex, so this store publishes-or-retracts atomically with
        // the commit for free. `store/fs.rs` cannot: its marker is a second file
        // beside the snapshot, so it splits the two arms by which way their
        // failure points. It now writes either the marker or its retraction as a
        // required operation before publishing the snapshot; exact equality
        // keeps that prepublication invisible to the old snapshot.
        //
        // So this store is structurally incapable of reaching the failure that
        // machinery exists for, and any contract case written against it would
        // pass whether or not that machinery is there. That is why the case is
        // `store/fs.rs`'s
        // `a_lost_ending_marker_is_republished_by_the_next_claim` and NOT a
        // member of the shared suite: a fake whose vocabulary cannot express a
        // defect is not evidence about the defect.
        slot.ended = non_resumable_terminal(&slot.journal[..authoritative_len]);
        Ok(slot.revision)
    }

    async fn commit_fenced(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        lease: &Lease,
    ) -> StoreResult<Revision> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.commit(key, state, expected).await
    }

    async fn append_journal(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
    ) -> StoreResult<u64> {
        Journal::check_batch_addresses(key.execution_id(), appends)?;
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        let watermark = slot.state.as_ref().map_or(0, |state| state.journal_seq);
        if appends.is_empty() {
            return Ok(watermark);
        }

        // Sweep before appending, for the reason the store's module docs give:
        // records placed after an orphan get buried below the next watermark and
        // are replayed as though they had been committed.
        if slot.journal.last().map_or(0, |record| record.seq) > watermark {
            let orphan_start = slot
                .journal
                .iter()
                .position(|record| record.seq > watermark)
                .unwrap_or(slot.journal.len());
            let orphans = slot.journal.split_off(orphan_start);
            slot.swept.extend(orphans);
            // Retention, never correctness: the sweep itself is unconditional and
            // what is dropped here is the oldest attempt, not the newest.
            if slot.swept.len() > MAX_RETAINED_SWEPT_RECORDS {
                let excess = slot.swept.len() - MAX_RETAINED_SWEPT_RECORDS;
                slot.swept.drain(..excess);
            }
        }

        // The same ceiling the filesystem store's reader applies. A journal past
        // it is one `Journal::parse` refuses, so a store that accepted the record
        // here and refused it there would be a driver that passes its tests and
        // bricks an execution in production.
        if watermark.saturating_add(appends.len() as u64) > MAX_JOURNAL_RECORDS as u64 {
            return Err(StoreError::Journal(JournalError::TooManyRecords {
                limit: MAX_JOURNAL_RECORDS,
            }));
        }

        let at_ms = Utc::now().timestamp_millis();
        let mut seq = watermark;
        for append in appends {
            seq += 1;
            let record = JournalRecord {
                seq,
                iteration: append.iteration,
                phase: append.phase,
                ordinal: append.ordinal,
                at_ms,
                body: append.body.clone(),
            };
            // The same encode-side bound the filesystem store applies. A record
            // this store accepted and the other refused would make the contract
            // suite pass on a difference that matters.
            record.to_line()?;
            slot.journal.push(record);
        }
        Ok(seq)
    }

    async fn append_journal_fenced(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
        lease: &Lease,
    ) -> StoreResult<u64> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.append_journal(key, appends).await
    }

    async fn read_journal(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
    ) -> StoreResult<Vec<JournalRecord>> {
        let slots = self.slots()?;
        let Some(slot) = slots.get(key) else {
            return Ok(Vec::new());
        };
        Ok(slot
            .journal
            .iter()
            .filter(|record| record.seq >= from_seq)
            .cloned()
            .collect())
    }

    async fn record_effect_intent(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
    ) -> StoreResult<()> {
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        slot.effects.record_intent(entry.clone())?;
        Ok(())
    }

    async fn record_effect_intent_fenced(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.record_effect_intent(key, entry).await
    }

    async fn record_effect_outcome(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> StoreResult<()> {
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        slot.effects.record_outcome(effect_id, outcome)?;
        Ok(())
    }

    async fn record_effect_outcome_fenced(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.record_effect_outcome(key, effect_id, outcome).await
    }

    async fn load_effects(&self, key: &ExecutionKey) -> StoreResult<EffectLedger> {
        let slots = self.slots()?;
        Ok(slots
            .get(key)
            .map(|slot| slot.effects.clone())
            .unwrap_or_default())
    }

    async fn claim(
        &self,
        key: &ExecutionKey,
        worker: &WorkerId,
        ttl: Duration,
    ) -> StoreResult<Lease> {
        let _mutation = self.mutation.lock().await;
        let now_ms = Utc::now().timestamp_millis();
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        if let Some(held) = &slot.lease {
            // A worker id is not proof that the caller still possesses the
            // lease. Only `renew`, with the current fence, may extend it.
            if !held.released && held.expires_at_ms > now_ms {
                return Err(StoreError::LeaseHeld {
                    by: held.worker.clone(),
                    until_ms: held.expires_at_ms,
                });
            }
        }
        let fence = slot.lease.as_ref().map_or(0, |held| held.fence) + 1;
        let expires_at_ms = now_ms.saturating_add(ttl_millis(ttl));
        slot.lease = Some(HeldLease {
            worker: worker.clone(),
            fence,
            expires_at_ms,
            released: false,
        });
        Ok(Lease {
            key: key.clone(),
            worker: worker.clone(),
            fence,
            expires_at_ms,
        })
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> StoreResult<Lease> {
        let _mutation = self.mutation.lock().await;
        let now_ms = Utc::now().timestamp_millis();
        let mut slots = self.slots()?;
        let slot = slots.entry(lease.key.clone()).or_default();
        let current = slot
            .lease
            .as_ref()
            .ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released || current.fence != lease.fence || current.worker != lease.worker {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        let fence = current.fence + 1;
        let expires_at_ms = now_ms.saturating_add(ttl_millis(ttl));
        slot.lease = Some(HeldLease {
            worker: lease.worker.clone(),
            fence,
            expires_at_ms,
            released: false,
        });
        Ok(Lease {
            key: lease.key.clone(),
            worker: lease.worker.clone(),
            fence,
            expires_at_ms,
        })
    }

    async fn release(&self, lease: Lease) -> StoreResult<()> {
        let _mutation = self.mutation.lock().await;
        let mut slots = self.slots()?;
        let slot = slots.entry(lease.key.clone()).or_default();
        let current = slot
            .lease
            .as_ref()
            .ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released || current.fence != lease.fence || current.worker != lease.worker {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        // Marked released rather than cleared, so the fence keeps going forward
        // and a straggler can never re-present the one it held.
        slot.lease = Some(HeldLease {
            worker: lease.worker,
            fence: current.fence + 1,
            expires_at_ms: 0,
            released: true,
        });
        Ok(())
    }

    async fn resolve_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_id: &str,
    ) -> StoreResult<()> {
        let mut slots = self.slots()?;
        let slot = slots.entry(key.clone()).or_default();
        let outstanding = slot.wakes.entry(wake_token.to_string()).or_default();
        // The same ceiling the filesystem store applies, and shared for the
        // reason this store's module docs give: the limits it deliberately does
        // not enforce are the ones that exist because a file has to be read
        // back, and this is not one of them. A completer reporting without bound
        // must meet the same refusal under either driver — otherwise it is
        // refused in production and admitted by the suite that runs both.
        //
        // A re-delivery of something already recorded is admitted whatever the
        // count says: it adds nothing, and refusing it would turn a bound on a
        // misbehaving completer into a failure for a well-behaved one.
        if outstanding.len() >= MAX_WAKE_RESOLUTIONS && !outstanding.contains(resolution_id) {
            return Err(StoreError::WakeLedgerFull {
                wake_token: wake_token.to_string(),
                limit: MAX_WAKE_RESOLUTIONS,
            });
        }
        outstanding.insert(resolution_id.to_string());
        Ok(())
    }

    async fn wake_resolutions(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<String>> {
        let slots = self.slots()?;
        Ok(slots
            .get(key)
            .and_then(|slot| slot.wakes.get(wake_token))
            .map(|ids| ids.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn consume_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
    ) -> StoreResult<usize> {
        let mut slots = self.slots()?;
        let Some(slot) = slots.get_mut(key) else {
            return Ok(0);
        };
        let Some(outstanding) = slot.wakes.get_mut(wake_token) else {
            return Ok(0);
        };
        let removed = resolution_ids
            .iter()
            .filter(|id| outstanding.remove(*id))
            .count();
        if outstanding.is_empty() {
            slot.wakes.remove(wake_token);
        }
        Ok(removed)
    }

    async fn consume_wake_fenced(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
        lease: &Lease,
    ) -> StoreResult<usize> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.consume_wake(key, wake_token, resolution_ids).await
    }

    async fn list_runnable(
        &self,
        worker: &WorkerId,
        limit: usize,
    ) -> StoreResult<Vec<ExecutionKey>> {
        // One walk, not two. Written as a delegation rather than as a second
        // copy of the filter, because two copies is how the paged answer and the
        // unpaged one come to disagree about what "runnable" means — and the
        // disagreement would show as a key one caller can see and another
        // cannot, which is not a shape any test asks about.
        Ok(self.scan_runnable(worker, limit, None).await?.keys)
    }

    async fn scan_runnable(
        &self,
        worker: &WorkerId,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<RunnableScan> {
        let now_ms = Utc::now().timestamp_millis();
        let slots = self.slots()?;
        // Ordered, so a caller polling repeatedly sees a stable sequence rather
        // than a hash order that shifts under it. Cheap here, worth the
        // determinism, and now load-bearing rather than merely tidy: a cursor
        // into an order that moved under the caller would skip work silently.
        let ordered: BTreeMap<&ExecutionKey, &ExecutionSlot> = slots.iter().collect();
        let mut keys = Vec::new();
        // The last execution this page **examined**, offered or not. The resume
        // point has to be that rather than the last key returned, or a page
        // whose tail was all withheld work would resume in front of the withheld
        // run and re-examine it on every page — and a ceiling that fired before
        // anything was offered could not name a resume point at all.
        let mut last_visited: Option<ScanCursor> = None;
        let mut stopped_short = false;
        let mut visits = 0usize;
        for (key, slot) in ordered {
            let here = [
                key.principal().to_string(),
                key.workspace().to_string(),
                key.execution_id().to_string(),
            ];
            // `BTreeMap<ExecutionKey, _>` iterates in `ExecutionKey`'s derived
            // order — principal, then workspace, then execution id — which is
            // exactly this triple's lexicographic order. The cursor comparison
            // is the walk's own order for that reason, and not by coincidence:
            // if `ExecutionKey`'s field order ever changed, this comparison
            // would have to change with it or a cursor would skip live work.
            if let Some(after) = after {
                if &here <= after.segments() {
                    continue;
                }
            }
            if keys.len() >= limit || visits >= MAX_DISCOVERY_VISITS_PER_PAGE {
                // This entry was NOT examined, so the walk stopped short of the
                // store and `last_visited` is the previous entry — resuming
                // after it brings this one back.
                //
                // Unless there is no previous entry, which one input produces: a
                // `limit` of zero fills the page before the first entry is
                // looked at. A page that named nothing would have to hand back
                // the cursor it arrived with, and a caller paging in a loop
                // would ask the same question forever. So a page that examined
                // nothing names the entry it stopped ON — the next page resumes
                // past it, which loses nothing because a scan asked for zero
                // keys offers nothing wherever it stops, and the loop
                // terminates. The filesystem store does the same thing at the
                // same place.
                stopped_short = true;
                if last_visited.is_none() {
                    last_visited = Some(ScanCursor::from_segments(here));
                }
                break;
            }
            visits += 1;
            last_visited = Some(ScanCursor::from_segments(here));

            let Some(state) = &slot.state else {
                continue;
            };
            // Same instant the rest of this scan judges against, so a pin that
            // lapsed since the last pass is picked up on this one.
            if !state.claimable_by(worker, now_ms) {
                continue;
            }
            // Deadline-expired unparked states must be offered so the driver can
            // retire them durably. Unresolved parks are still withheld below and
            // remain the reconciler's responsibility.
            if state.runnable_at_ms > now_ms && !state.deadline_passed(now_ms) {
                continue;
            }
            if let Some(wait) = &state.wait {
                // Any unconsumed resolution naming this token satisfies the park.
                if slot
                    .wakes
                    .get(&wait.wake_token())
                    .is_none_or(|ids| ids.is_empty())
                {
                    continue;
                }
            }
            if state
                .terminal_settlement_receipt
                .as_ref()
                .is_some_and(|receipt| {
                    receipt.descriptor.terminal_seq == state.journal_seq
                        && receipt.descriptor.exact_segment_id == key.execution_id()
                })
            {
                continue;
            }
            // A run that ended for good. The watermark comparison is what keeps
            // this honest: an ending published against a prefix the currently
            // committed state does not reach is not an ending this state
            // vouches for.
            if let Some(ended) = &slot.ended {
                if ended.seq == state.journal_seq {
                    let Some(projection_through) = ended.projection_through_seq() else {
                        continue;
                    };
                    let projected_through = slot
                        .projector
                        .as_ref()
                        .map(ProjectorCursor::emitted_through_seq)
                        .unwrap_or(0);
                    if projected_through >= projection_through {
                        continue;
                    }
                }
            }
            if let Some(held) = &slot.lease {
                if !held.released && held.expires_at_ms > now_ms {
                    continue;
                }
            }
            keys.push(key.clone());
        }
        Ok(RunnableScan {
            keys,
            // No fallback to the caller's own cursor. Every stop above names a
            // position strictly after it, and handing back the cursor a page
            // arrived with is not a resume point at all — it is the same page
            // again, which a caller paging in a loop never escapes. If a later
            // edit produces a stop that can name nothing, this page ends: an
            // under-reported pass is recovered by the next poll and a repeated
            // page is recovered by nothing.
            resume: if stopped_short { last_visited } else { None },
        })
    }

    async fn scan_terminal_outbox_debt(
        &self,
        max_visits: NonZeroUsize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<TerminalOutboxScan> {
        let slots = self.slots()?;
        let ordered: BTreeMap<&ExecutionKey, &ExecutionSlot> = slots.iter().collect();
        let mut keys = Vec::new();
        let mut visits = 0usize;
        let mut last_visited = None;
        let mut stopped_short = false;

        for (key, slot) in ordered {
            let here = [
                key.principal().to_string(),
                key.workspace().to_string(),
                key.execution_id().to_string(),
            ];
            if after.is_some_and(|after| &here <= after.segments()) {
                continue;
            }
            if visits >= max_visits.get() {
                stopped_short = true;
                break;
            }
            visits += 1;
            last_visited = Some(ScanCursor::from_segments(here));

            let Some(state) = &slot.state else {
                continue;
            };
            // Receipt acknowledgement is lifecycle debt even for an eventless
            // nonterminal/cancelled run. It must not depend on ended.json (the
            // in-memory analogue is `slot.ended`) because a holder can die
            // after runtime cancellation skips normal loop recovery.
            if state.steer_consume_receipt.is_some() {
                keys.push(key.clone());
                continue;
            }
            let receipt_lifecycle_debt =
                state
                    .terminal_settlement_receipt
                    .as_ref()
                    .is_some_and(|receipt| {
                        receipt.descriptor.terminal_seq == state.journal_seq
                            && (state.identity.task_id.is_some()
                                || state.identity.execution_id.is_some())
                            && (slot
                                .projector
                                .as_ref()
                                .and_then(ProjectorCursor::runtime_settled_terminal_seq)
                                != Some(receipt.descriptor.terminal_seq)
                                || slot
                                    .projector
                                    .as_ref()
                                    .map(ProjectorCursor::emitted_through_seq)
                                    .unwrap_or(0)
                                    < receipt.descriptor.terminal_seq)
                    });
            let Some(ended) = slot.ended else {
                if receipt_lifecycle_debt {
                    keys.push(key.clone());
                }
                continue;
            };
            if ended.seq != state.journal_seq {
                if receipt_lifecycle_debt {
                    keys.push(key.clone());
                }
                continue;
            }
            let event_debt = ended
                .projection_through_seq()
                .is_some_and(|projection_through| {
                    slot.projector
                        .as_ref()
                        .map(ProjectorCursor::emitted_through_seq)
                        .unwrap_or(0)
                        < projection_through
                });
            let runtime_settlement_debt = ended.terminal != TerminalKind::HandedOff
                && (state.identity.task_id.is_some() || state.identity.execution_id.is_some())
                && (ended.terminal == TerminalKind::CannotProceed
                    || state
                        .terminal_settlement_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.descriptor.terminal_seq == ended.seq))
                && slot
                    .projector
                    .as_ref()
                    .and_then(ProjectorCursor::runtime_settled_terminal_seq)
                    != Some(ended.seq);
            if event_debt || runtime_settlement_debt || receipt_lifecycle_debt {
                keys.push(key.clone());
            }
        }

        Ok(TerminalOutboxScan {
            keys,
            resume: stopped_short.then_some(last_visited).flatten(),
        })
    }

    async fn list_parked(&self, limit: usize) -> StoreResult<ParkedListing> {
        self.scan_parked(limit, None).await
    }

    async fn scan_parked(
        &self,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<ParkedListing> {
        // A zero-sized page cannot publish a strict-after cursor without
        // skipping the first unvisited key forever. One is the smallest
        // bounded page that can make progress.
        let limit = limit.max(1);
        let slots = self.slots()?;
        // Ordered for the same reason `list_runnable` orders: a caller comparing
        // one pass against the next must not have to account for a hash order
        // that moved under it.
        let ordered: BTreeMap<&ExecutionKey, &ExecutionSlot> = slots.iter().collect();
        let mut parked = Vec::new();
        let mut incomplete = false;
        let mut last_visited = None;
        let mut visits = 0usize;
        for (key, slot) in ordered {
            let here = [
                key.principal().to_string(),
                key.workspace().to_string(),
                key.execution_id().to_string(),
            ];
            if after.is_some_and(|after| &here <= after.segments()) {
                continue;
            }
            if parked.len() >= limit || visits >= MAX_DISCOVERY_VISITS_PER_PAGE {
                // Stopping short is a hole in the caller's coverage rather than
                // simply less work taken, so it is reported instead of implied by
                // the length.
                incomplete = true;
                if last_visited.is_none() {
                    last_visited = Some(ScanCursor::from_segments(here));
                }
                break;
            }
            visits += 1;
            last_visited = Some(ScanCursor::from_segments(here));
            let Some(state) = &slot.state else {
                continue;
            };
            let Some(wait) = state.wait.clone() else {
                continue;
            };
            parked.push(ParkedExecution {
                key: key.clone(),
                wait,
                parked_since_ms: slot.committed_at_ms,
                last_lease: slot.lease.as_ref().map(|held| LastLease {
                    worker: held.worker.clone(),
                    fence: held.fence,
                    expires_at_ms: held.expires_at_ms,
                    released: held.released,
                }),
                deadline_at_ms: state.deadline_at_ms,
            });
        }
        Ok(ParkedListing {
            parked,
            resume: incomplete.then_some(last_visited).flatten(),
            incomplete,
        })
    }

    async fn load_projector_cursor(
        &self,
        key: &ExecutionKey,
    ) -> StoreResult<Option<ProjectorCursor>> {
        let slots = self.slots()?;
        Ok(slots.get(key).and_then(|slot| slot.projector.clone()))
    }

    async fn save_projector_cursor(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
    ) -> StoreResult<()> {
        let mut slots = self.slots()?;
        // `or_default`, not "the slot must exist": the mark is saved after a
        // commit and a commit creates the slot, but a caller that projected a
        // run whose state lives in another handle's substrate would otherwise
        // silently drop the mark and re-emit forever.
        slots.entry(key.clone()).or_default().projector = Some(cursor.clone());
        Ok(())
    }

    async fn save_projector_cursor_fenced(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _mutation = self.mutation.lock().await;
        self.validate_current_lease(key, lease)?;
        self.save_projector_cursor(key, cursor).await
    }

    async fn load_chain_closure(&self, key: &ExecutionKey) -> StoreResult<Option<ChainClosure>> {
        let slots = self.slots()?;
        Ok(slots.get(key).and_then(|slot| slot.chain_closure.clone()))
    }

    async fn record_chain_closure(
        &self,
        key: &ExecutionKey,
        closure: &ChainClosure,
    ) -> StoreResult<()> {
        let mut slots = self.slots()?;
        // `or_default` for the same reason the projector mark uses it: the
        // receipt is published after the commit that ended the segment, and a
        // caller closing a chain whose state lives in another handle's
        // substrate must not have its receipt silently dropped — an absent
        // receipt is indistinguishable from a chain that never closed, so the
        // drop would be invisible and permanent.
        slots.entry(key.clone()).or_default().chain_closure = Some(closure.clone());
        Ok(())
    }
}

fn ttl_millis(ttl: Duration) -> i64 {
    i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::journal::RecordedStep;
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
    use crate::magician_v2::execution::agentic::run_loop::store::contract::{
        self, ContractHarness,
    };
    use crate::magician_v2::execution::agentic::run_loop::store::loop_state_store_contract;

    pub struct MemoryHarness {
        store: MemoryLoopStateStore,
    }

    impl ContractHarness for MemoryHarness {
        type Store = MemoryLoopStateStore;

        fn create() -> Self {
            Self {
                store: MemoryLoopStateStore::new(),
            }
        }

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn reopen(&self) -> Self::Store {
            // The substrate here is the process, so a second handle onto it is a
            // clone. This is the weakest form the case takes — it cannot fail for
            // an in-memory store — and the honest reading is that the case is
            // aimed at the implementations that have a substrate to be wrong
            // about.
            self.store.clone()
        }
    }

    loop_state_store_contract!(MemoryHarness);

    #[tokio::test]
    async fn a_swept_attempt_is_kept_rather_than_dropped() {
        let store = MemoryLoopStateStore::new();
        let key = contract::key("swept");
        let mut state = contract::fresh_state(&key);

        store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        state.journal_seq = 1;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");
        store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the attempt that never commits");
        store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the re-run that sweeps it");

        let swept = store.swept_records(&key).expect("read the swept attempt");
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].seq, 2);
        assert_eq!(
            store.read_journal(&key, 0).await.expect("read").len(),
            2,
            "the live log holds the committed record and the re-run's, not the orphan"
        );
    }

    #[tokio::test]
    async fn a_completer_reporting_without_bound_meets_the_same_ceiling_here() {
        // The divergence this closes: `MAX_WAKE_RESOLUTIONS` was declared in
        // `fs` and enforced only there, so a completer that reported without
        // bound was refused in production and admitted by the in-process driver
        // — growing a map nothing ever trims. The limits this store deliberately
        // does not share are the ones that exist because a file has to be read
        // back; a count of outstanding resolutions is not one of them.
        let store = MemoryLoopStateStore::new();
        let key = contract::key("wake-flood");
        let token = "job:coding-7";

        for index in 0..MAX_WAKE_RESOLUTIONS {
            store
                .resolve_wake(&key, token, &format!("report-{index}"))
                .await
                .unwrap_or_else(|error| panic!("report {index} must be admitted: {error}"));
        }
        let error = store
            .resolve_wake(&key, token, "report-one-too-many")
            .await
            .expect_err("the ceiling must refuse the next NEW resolution");
        assert!(
            matches!(error, StoreError::WakeLedgerFull { limit, .. } if limit == MAX_WAKE_RESOLUTIONS),
            "got {error}"
        );

        // A re-delivery of something already recorded is still admitted at the
        // ceiling: it adds nothing, and refusing it would turn a bound on a
        // misbehaving completer into a failure for a well-behaved one. Without
        // this the assertion above would pass on an implementation that refused
        // every report once full, including the retries.
        store
            .resolve_wake(&key, token, "report-0")
            .await
            .expect("a re-delivery adds nothing and must not be refused");
        assert_eq!(
            store
                .wake_resolutions(&key, token)
                .await
                .expect("read")
                .len(),
            MAX_WAKE_RESOLUTIONS
        );
    }

    #[tokio::test]
    async fn a_poisoned_store_refuses_to_answer_rather_than_serving_half_a_mutation() {
        // The failure this prevents: a holder panics mid-commit, and every later
        // read reports whatever state the panic left behind as though it had
        // been committed. Recovering into the inner value is right for a cache
        // and wrong for a store whose contract is "what you get is what was
        // committed".
        let store = MemoryLoopStateStore::new();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.slots.lock().expect("the lock starts healthy");
            panic!("a holder panicked while mutating");
        }));
        assert!(
            panicked.is_err(),
            "the fixture must actually poison the lock"
        );

        let error = store
            .load(&contract::key("poisoned"))
            .await
            .expect_err("a poisoned store must not answer");
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "got {error}"
        );
    }

    #[tokio::test]
    async fn a_sparse_runnable_page_stops_at_the_visit_ceiling_and_resumes() {
        let store = MemoryLoopStateStore::new();
        let now_ms = Utc::now().timestamp_millis();
        let mut tail = None;
        for index in 0..=MAX_DISCOVERY_VISITS_PER_PAGE {
            let key = contract::key(&format!("bounded-runnable-{index:05}"));
            let mut state = contract::fresh_state(&key);
            if index < MAX_DISCOVERY_VISITS_PER_PAGE {
                state.runnable_at_ms = now_ms + 3_600_000;
            } else {
                tail = Some(key.clone());
            }
            store
                .commit(&key, &state, Revision::INITIAL)
                .await
                .expect("commit sparse runnable fixture");
        }

        let first = store
            .scan_runnable(&WorkerId::new("worker"), 1, None)
            .await
            .expect("first bounded page");
        assert!(first.keys.is_empty());
        let resume = first.resume.expect("unvisited tail remains");
        let second = store
            .scan_runnable(&WorkerId::new("worker"), 1, Some(&resume))
            .await
            .expect("resumed page");
        assert_eq!(second.keys, vec![tail.expect("tail key")]);
    }

    #[tokio::test]
    async fn a_sparse_parked_page_stops_at_the_visit_ceiling_and_resumes() {
        let store = MemoryLoopStateStore::new();
        let mut tail = None;
        for index in 0..=MAX_DISCOVERY_VISITS_PER_PAGE {
            let key = contract::key(&format!("bounded-parked-{index:05}"));
            let mut state = contract::fresh_state(&key);
            if index == MAX_DISCOVERY_VISITS_PER_PAGE {
                state.wait = Some(
                    crate::magician_v2::execution::agentic::run_loop::state::WaitReason::Job {
                        job_id: "tail-job".to_string(),
                    },
                );
                tail = Some(key.clone());
            }
            store
                .commit(&key, &state, Revision::INITIAL)
                .await
                .expect("commit sparse parked fixture");
        }

        let first = store
            .scan_parked(1, None)
            .await
            .expect("first bounded page");
        assert!(first.parked.is_empty());
        assert!(first.incomplete);
        let resume = first.resume.expect("unvisited tail remains");
        let second = store
            .scan_parked(1, Some(&resume))
            .await
            .expect("resumed page");
        assert_eq!(second.parked.len(), 1);
        assert_eq!(second.parked[0].key, tail.expect("tail key"));
    }
}
