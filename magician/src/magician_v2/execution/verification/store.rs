//! Task-scoped persistence for gates and attestations.
//!
//! Constructed against one scope's root, so cross-scope confusion is
//! structurally impossible in the same way `TransactionStore` and
//! `CodeChangeProposalStore` achieve it — the store simply cannot address
//! another scope's directory. On top of that physical isolation every loaded
//! record is checked against the store's own scope, because physical isolation
//! alone would be defeated by a record copied between directories.
//!
//! All methods are sync; file I/O happens on the caller's thread. Callers
//! inside an async runtime should wrap in `tokio::task::spawn_blocking`, as
//! the neighbouring stores do. Records are small (one JSON file each).
//!
//! ## Mutual exclusion and fencing
//!
//! Two different mechanisms, doing two different jobs:
//!
//! * an advisory **file lock** serialises concurrent writers *within* a
//!   process boundary, so read-modify-write is not interleaved;
//! * the **generation** fencing token stops a writer that lost its lease
//!   minutes ago from committing on top of its replacement.
//!
//! A lock alone is not enough: the classic failure is a worker that stalls,
//! loses its lease, wakes up, acquires the lock legitimately, and writes stale
//! state. Every mutating call therefore re-checks the generation *under* the
//! lock.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fs2::FileExt;
use uuid::Uuid;

use super::attestation::{
    AcceptedResult, AttemptOutcome, AttestationKey, VerificationAttempt, VerificationAttestation,
    ATTESTATION_SCHEMA_VERSION,
};
use super::gate::{GateLease, GateStatus, VerificationGate, GATE_SCHEMA_VERSION};
use super::ids::{AttestationId, CandidateRevision, GateId, Generation};
use super::journal::{JournalPayload, JournalTransaction, OutboxEntry, VerificationJournal};
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

/// Default lease duration.
///
/// Deliberately shorter than any real pass — this repository checks in 5–7
/// minutes and tests in ~16 — because a pass is expected to *renew*, not to
/// outlive a giant TTL. The controller spawns a renewal task alongside every
/// leased pass that extends the lease at half this interval and stops the
/// moment the pass returns, so a worker that dies frees the gate within one
/// TTL instead of holding it for the length of the longest imaginable run.
///
/// The lease is still advisory. What keeps a duplicate worker's result from
/// landing is the **generation fence** in [`VerificationStore::commit_gate`]:
/// `claim_lease` bumps the generation, and every mutating commit re-checks it
/// under the file lock, so a superseded writer is refused. Renewal's job is
/// narrower — it keeps a healthy long pass from *looking* abandoned, so
/// nothing claims the gate out from under it and discards its finished result
/// at the fence.
pub const DEFAULT_LEASE_SECS: i64 = 120;

/// Held for the duration of a read-modify-write.
pub struct StoreLock {
    file: std::fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Index row mapping a root task to its gate.
///
/// Carries the task id so a hash collision or a stale file cannot silently
/// answer for the wrong task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TaskIndexEntry {
    root_task_id: String,
    gate_id: String,
}

/// Index row pointing a key digest at its reusable (green) attestation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AttestationIndexEntry {
    attestation_id: String,
}

/// Outcome of a compare-and-set. Distinguishes "someone else already did this"
/// from "this was refused", because the two need different handling: the first
/// is benign under replay, the second is a real conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasOutcome<T> {
    Applied(T),
    AlreadySettled(T),
}

impl<T> CasOutcome<T> {
    pub fn into_inner(self) -> T {
        match self {
            CasOutcome::Applied(v) | CasOutcome::AlreadySettled(v) => v,
        }
    }

    pub fn was_applied(&self) -> bool {
        matches!(self, CasOutcome::Applied(_))
    }
}

pub struct VerificationStore {
    root: PathBuf,
    scope: TransactionScope,
    journal: VerificationJournal,
}

impl VerificationStore {
    pub fn new(scope_root: impl Into<PathBuf>, scope: TransactionScope) -> Self {
        let root = scope_root.into().join("verification");
        let journal = VerificationJournal::new(&root);
        Self {
            root,
            scope,
            journal,
        }
    }

    pub fn scope(&self) -> &TransactionScope {
        &self.scope
    }

    pub fn journal(&self) -> &VerificationJournal {
        &self.journal
    }

    fn gates_dir(&self) -> PathBuf {
        self.root.join("gates")
    }

    fn attestations_dir(&self) -> PathBuf {
        self.root.join("attestations")
    }

    fn outbox_dir(&self) -> PathBuf {
        self.root.join("outbox")
    }

    fn gate_path(&self, id: &GateId) -> PathBuf {
        self.gates_dir().join(format!("{}.json", id.as_str()))
    }

    fn attestation_path(&self, id: &AttestationId) -> PathBuf {
        self.attestations_dir()
            .join(format!("{}.json", id.as_str()))
    }

    fn outbox_path(&self, entry_id: &str) -> PathBuf {
        self.outbox_dir().join(format!("{entry_id}.json"))
    }

    fn task_index_dir(&self) -> PathBuf {
        self.root.join("index/by_task")
    }

    /// Index path for a task.
    ///
    /// Keyed by a hash of the task id rather than the id itself: task ids are
    /// externally supplied, and hashing removes any question of path
    /// traversal or of a filesystem's opinion about case and length.
    fn task_index_path(&self, task_id: &str) -> PathBuf {
        let digest = blake3::hash(task_id.as_bytes()).to_hex().to_string();
        self.task_index_dir().join(format!("{digest}.json"))
    }

    /// Resolve the gate for a task in a single read.
    ///
    /// This is the task-read hot path — it runs on every task API response, so
    /// it must not scan the gate directory. Returns `None` for a task with no
    /// gate, which is the overwhelmingly common case.
    pub fn gate_id_for_task(&self, task_id: &str) -> Option<GateId> {
        let bytes = std::fs::read(self.task_index_path(task_id)).ok()?;
        let entry: TaskIndexEntry = serde_json::from_slice(&bytes).ok()?;
        // The index is a cache; a stale entry naming another task must not
        // answer for this one.
        if entry.root_task_id != task_id {
            return None;
        }
        GateId::parse(&entry.gate_id).ok()
    }

    fn write_task_index(&self, gate: &VerificationGate) -> Result<()> {
        let entry = TaskIndexEntry {
            root_task_id: gate.root_task_id.clone(),
            gate_id: gate.gate_id.as_str().to_string(),
        };
        Self::write_atomic(&self.task_index_path(&gate.root_task_id), &entry)
    }

    fn lock(&self, name: &str) -> Result<StoreLock> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create verification root {}", self.root.display()))?;
        let path = self.root.join(format!("{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open verification lock {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("lock verification {}", path.display()))?;
        Ok(StoreLock { file })
    }

    fn write_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value).context("serialize verification record")?;
        // The shared durable writer: parent created, unique temp, fsync, rename,
        // parent-dir sync. The previous hand-roll here skipped both fsyncs, so a
        // crash after the rename could publish an empty verification record.
        crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(path, &bytes)
            .with_context(|| format!("write verification record {}", path.display()))
    }

    // ---------------------------------------------------------------- gates

    /// Open a gate: commits the atomic candidate-entry transaction of §4.3,
    /// then projects it.
    ///
    /// The journal write is what makes this all-or-nothing. Projection after
    /// it is a cache refresh; if the process dies between the two, recovery
    /// rebuilds the projection from the journal.
    pub fn open_gate(&self, gate: VerificationGate) -> Result<(VerificationGate, OutboxEntry)> {
        self.assert_scope(&gate.scope, "gate")?;
        let _lock = self.lock("gates")?;

        if self.gate_path(&gate.gate_id).exists() {
            return Err(anyhow!("gate {} already exists", gate.gate_id));
        }

        let entry = OutboxEntry {
            entry_id: format!("obx-{}", Uuid::new_v4()),
            gate_id: gate.gate_id.clone(),
            candidate: gate.current_candidate.clone(),
            enqueued_at: Utc::now(),
            claimed_by: None,
        };

        let txn = JournalTransaction::new(
            gate.gate_id.clone(),
            self.journal.next_sequence(&gate.gate_id)?,
            JournalPayload::CandidateEntry {
                gate: Box::new(gate.clone()),
                outbox: entry.clone(),
            },
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn)?;
        Ok((gate, entry))
    }

    /// Register the successor candidate a repair round produced, re-entering
    /// verification.
    ///
    /// This is the other half of `settle_repairing`. Repair invalidates the
    /// prior candidate and retires its work item with no replacement, because
    /// at that moment there is nothing to verify. When the engineer's
    /// successor lands, this re-arms the gate: the candidate advances, the
    /// status returns to `verification_pending`, and a fresh work item is
    /// enqueued. Without it a repairing gate never runs again.
    ///
    /// Journaled as a `CandidateEntry` for the same reason `open_gate` is —
    /// candidate, gate state, and outbox move as one transaction.
    pub fn record_successor_candidate(
        &self,
        gate_id: &GateId,
        successor: CandidateRevision,
    ) -> Result<(VerificationGate, OutboxEntry)> {
        let _lock = self.lock("gates")?;
        let prior = self.load_gate(gate_id)?;

        if prior.status != GateStatus::Repairing {
            return Err(anyhow!(
                "gate {gate_id} is {:?}, not repairing; a successor candidate is only \
                 meaningful for a gate awaiting one",
                prior.status
            ));
        }
        if successor.revision <= prior.current_candidate.revision {
            return Err(anyhow!(
                "gate {gate_id} successor revision {} does not supersede {}",
                successor.revision,
                prior.current_candidate.revision
            ));
        }

        let mut next = prior.clone();
        next.current_candidate = successor.clone();
        next.status = GateStatus::VerificationPending;
        // The prior round's attestation keyed on the prior candidate; the
        // successor gets its own.
        next.active_attestation_ref = None;
        next.updated_at = Utc::now();
        next.validate_successor(&prior)?;

        let entry = enqueue_repaired_candidate(gate_id.clone(), successor);
        let txn = JournalTransaction::new(
            gate_id.clone(),
            self.journal.next_sequence(gate_id)?,
            JournalPayload::CandidateEntry {
                gate: Box::new(next.clone()),
                outbox: entry.clone(),
            },
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn)?;
        Ok((next, entry))
    }

    pub fn load_gate(&self, id: &GateId) -> Result<VerificationGate> {
        let path = self.gate_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read verification gate {}", path.display()))?;
        let gate: VerificationGate = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse verification gate {}", path.display()))?;
        if gate.schema_version != GATE_SCHEMA_VERSION {
            return Err(anyhow!(
                "verification gate {id} has schema version {}; this build understands {}",
                gate.schema_version,
                GATE_SCHEMA_VERSION
            ));
        }
        // Physical isolation is not enough on its own — a record copied between
        // scope directories would otherwise be honoured.
        self.assert_scope(&gate.scope, "gate")?;
        Ok(gate)
    }

    pub fn gate_exists(&self, id: &GateId) -> bool {
        self.gate_path(id).is_file()
    }

    /// Commit a gate transition through the journal, under the lock, with the
    /// fencing generation re-checked against the *stored* gate.
    pub fn commit_gate(
        &self,
        next: VerificationGate,
        holder_generation: Generation,
        payload: impl FnOnce(Box<VerificationGate>) -> JournalPayload,
    ) -> Result<VerificationGate> {
        self.assert_scope(&next.scope, "gate")?;
        let _lock = self.lock("gates")?;
        let prior = self.load_gate(&next.gate_id)?;

        if prior.generation != holder_generation {
            return Err(anyhow!(
                "gate {} is at generation {} but writer holds {}; refusing a superseded write",
                prior.gate_id,
                prior.generation,
                holder_generation
            ));
        }
        next.validate_successor(&prior)?;

        let txn = JournalTransaction::new(
            next.gate_id.clone(),
            self.journal.next_sequence(&next.gate_id)?,
            payload(Box::new(next.clone())),
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn)?;
        Ok(next)
    }

    // --------------------------------------------------------------- leases

    /// Claim or steal a lease.
    ///
    /// Bumps the generation, which is precisely what invalidates the previous
    /// holder. A live, unexpired lease held by someone else is refused.
    pub fn claim_lease(
        &self,
        id: &GateId,
        holder: &str,
        now: DateTime<Utc>,
        ttl_secs: i64,
    ) -> Result<VerificationGate> {
        let _lock = self.lock("gates")?;
        let mut gate = self.load_gate(id)?;

        if let Some(existing) = &gate.lease {
            if !existing.is_expired_at(now) && existing.holder != holder {
                return Err(anyhow!(
                    "gate {id} lease is held by {} until {}",
                    existing.holder,
                    existing.expires_at
                ));
            }
        }

        let generation = gate.generation.next();
        gate.generation = generation;
        gate.lease = Some(GateLease {
            holder: holder.to_string(),
            token: format!("lease-{}", Uuid::new_v4()),
            generation,
            acquired_at: now,
            expires_at: now + ChronoDuration::seconds(ttl_secs),
        });
        gate.updated_at = now;

        let txn = JournalTransaction::new(
            id.clone(),
            self.journal.next_sequence(id)?,
            JournalPayload::GateUpdate {
                gate: Box::new(gate.clone()),
            },
        )?;
        self.journal.commit(&txn)?;
        self.project(&txn)?;
        Ok(gate)
    }

    /// Extend a lease the caller still legitimately holds.
    ///
    /// Called by the controller's renewal task at half the TTL for as long as
    /// a leased pass is running — see [`DEFAULT_LEASE_SECS`].
    ///
    /// **Deliberately not journaled.** A renewal only pushes `expires_at`
    /// out; it does not touch the fencing generation, which moves on *claim*.
    /// So a renewal that is lost to a crash is harmless — the lease simply
    /// expires and a replacement claims it, which is the same outcome as the
    /// worker having died.
    ///
    /// Journaling it would be actively harmful. A two-hour gate renewing on a
    /// 60-second cadence writes ~120 transactions, each carrying a full cloned
    /// gate; every one is re-parsed and successor-validated on each replay,
    /// and `next_sequence` re-scans that growing directory on every subsequent
    /// write. Durable history should record decisions, not heartbeats.
    pub fn renew_lease(
        &self,
        id: &GateId,
        holder: &str,
        holder_generation: Generation,
        now: DateTime<Utc>,
        ttl_secs: i64,
    ) -> Result<VerificationGate> {
        let _lock = self.lock("gates")?;
        let mut gate = self.load_gate(id)?;
        self.assert_lease_live(&gate, holder, holder_generation, now)?;

        if let Some(lease) = gate.lease.as_mut() {
            lease.expires_at = now + ChronoDuration::seconds(ttl_secs);
        }
        gate.updated_at = now;

        // Projection only. Replay reconstructs the gate without this write,
        // and reconstructs it *without a lease* — which is correct, because a
        // process that crashed is not holding one.
        Self::write_atomic(&self.gate_path(id), &gate)?;
        Ok(gate)
    }

    /// The predicate a renewal goes through.
    ///
    /// Checks holder, generation *and* wall-clock expiry together. Checking
    /// only the generation would let a holder whose lease lapsed — but whose
    /// replacement has not yet claimed — keep writing.
    ///
    /// **Reached only from [`Self::renew_lease`].** The write path that is
    /// actually enforced is [`Self::commit_gate`]'s generation fence. Do not
    /// read this as "every write is lease-checked"; it is not.
    pub fn assert_lease_live(
        &self,
        gate: &VerificationGate,
        holder: &str,
        holder_generation: Generation,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let Some(lease) = &gate.lease else {
            return Err(anyhow!("gate {} holds no lease", gate.gate_id));
        };
        if lease.holder != holder {
            return Err(anyhow!(
                "gate {} lease is held by {}, not {holder}",
                gate.gate_id,
                lease.holder
            ));
        }
        if gate.generation != holder_generation || lease.generation != holder_generation {
            return Err(anyhow!(
                "gate {} is at generation {} but {holder} holds {}; a superseded worker cannot commit",
                gate.gate_id,
                gate.generation,
                holder_generation
            ));
        }
        if lease.is_expired_at(now) {
            return Err(anyhow!(
                "gate {} lease for {holder} expired at {}",
                gate.gate_id,
                lease.expires_at
            ));
        }
        Ok(())
    }

    // --------------------------------------------------------- attestations

    pub fn create_attestation(
        &self,
        attestation: &VerificationAttestation,
    ) -> Result<VerificationAttestation> {
        self.assert_scope(&attestation.key.scope, "attestation")?;
        attestation.key.validate()?;
        let _lock = self.lock("attestations")?;
        let path = self.attestation_path(&attestation.attestation_id);
        if path.exists() {
            return Err(anyhow!(
                "attestation {} already exists",
                attestation.attestation_id
            ));
        }
        Self::write_atomic(&path, attestation)?;
        Ok(attestation.clone())
    }

    pub fn load_attestation(&self, id: &AttestationId) -> Result<VerificationAttestation> {
        let path = self.attestation_path(id);
        let bytes =
            std::fs::read(&path).with_context(|| format!("read attestation {}", path.display()))?;
        let att: VerificationAttestation = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse attestation {}", path.display()))?;
        if att.schema_version != ATTESTATION_SCHEMA_VERSION {
            return Err(anyhow!(
                "attestation {id} has schema version {}; this build understands {}",
                att.schema_version,
                ATTESTATION_SCHEMA_VERSION
            ));
        }
        self.assert_scope(&att.key.scope, "attestation")?;
        Ok(att)
    }

    /// Append an attempt. Refused once the attestation is sealed.
    pub fn append_attempt(
        &self,
        id: &AttestationId,
        attempt: VerificationAttempt,
    ) -> Result<VerificationAttestation> {
        let _lock = self.lock("attestations")?;
        let prior = self.load_attestation(id)?;
        let mut next = prior.clone();
        next.append_attempt(attempt)?;
        next.validate_successor(&prior)?;
        Self::write_atomic(&self.attestation_path(id), &next)?;
        Ok(next)
    }

    /// Write the accepted result exactly once, under compare-and-set.
    ///
    /// Concurrent acceptors produce exactly one winner; the loser is told the
    /// attestation is already settled rather than being handed an error it
    /// would have to interpret.
    pub fn accept_result(
        &self,
        id: &AttestationId,
        accepted: AcceptedResult,
    ) -> Result<CasOutcome<VerificationAttestation>> {
        let _lock = self.lock("attestations")?;
        let prior = self.load_attestation(id)?;

        if let Some(existing) = &prior.accepted_result {
            // Idempotent replay of the identical acceptance is benign.
            if existing.attempt_id == accepted.attempt_id && existing.outcome == accepted.outcome {
                return Ok(CasOutcome::AlreadySettled(prior));
            }
            return Err(anyhow!(
                "attestation {id} already accepted attempt {} with {:?}; accepted_result is write-once",
                existing.attempt_id,
                existing.outcome
            ));
        }

        // The accepted attempt must actually exist and must agree about its
        // own outcome, or the acceptance is describing evidence that was never
        // produced.
        let attempt = prior
            .attempt(&accepted.attempt_id)
            .ok_or_else(|| anyhow!("attestation {id} has no attempt {}", accepted.attempt_id))?;
        if attempt.outcome != accepted.outcome {
            return Err(anyhow!(
                "attestation {id} attempt {} is {:?} but acceptance claims {:?}",
                accepted.attempt_id,
                attempt.outcome,
                accepted.outcome
            ));
        }
        if attempt.outcome == AttemptOutcome::Indeterminate {
            return Err(anyhow!(
                "attestation {id} cannot accept an indeterminate attempt; re-run under a new attempt"
            ));
        }

        // Captured before the move; the early return above guarantees we only
        // reach here when nothing was accepted yet.
        let is_green = accepted.outcome == AttemptOutcome::Green;
        let mut next = prior.clone();
        next.accepted_result = Some(accepted);
        next.validate_successor(&prior)?;
        Self::write_atomic(&self.attestation_path(id), &next)?;
        if is_green {
            // Best-effort: the index is a cache, so a failure to write it
            // costs a future re-run rather than correctness.
            let _ = self.write_attestation_index(&next);
        }
        Ok(CasOutcome::Applied(next))
    }

    /// Find an existing attestation whose key matches exactly.
    ///
    /// Whole-key equality, never digest-only: a digest match with differing
    /// fields would be a collision, and answering "reusable" there is the
    /// cross-project authority hole this guards.
    pub fn find_reusable(&self, key: &AttestationKey) -> Result<Option<VerificationAttestation>> {
        // Indexed by key digest. Scanning and parsing every attestation ever
        // written would make each verification pass cost O(all evidence in
        // the scope) — unbounded growth on the path that runs most.
        //
        // The index is only ever an optimisation: a missing or stale entry
        // costs a re-run, never a wrong answer, because the full key is
        // re-checked below.
        let Ok(bytes) = std::fs::read(self.attestation_index_path(key)) else {
            return Ok(None);
        };
        let Ok(entry) = serde_json::from_slice::<AttestationIndexEntry>(&bytes) else {
            return Ok(None);
        };
        let Ok(id) = AttestationId::parse(&entry.attestation_id) else {
            return Ok(None);
        };
        let Ok(att) = self.load_attestation(&id) else {
            return Ok(None);
        };
        // Whole-key equality, never digest-only: a digest match with differing
        // fields would be a collision, and answering "reusable" there is the
        // cross-project authority hole this guards.
        if att.key.matches(key) && att.key.scope == self.scope {
            return Ok(Some(att));
        }
        Ok(None)
    }

    fn attestation_index_path(&self, key: &AttestationKey) -> PathBuf {
        self.root
            .join("index/by_attestation_key")
            .join(format!("{}.json", key.digest().to_hex()))
    }

    /// Record a settled-green attestation as the reuse candidate for its key.
    ///
    /// Only green results are indexed. A sealed *red* attestation is evidence
    /// of failure, not a cache hit, and indexing it would invite a future
    /// reader to treat it as one.
    fn write_attestation_index(&self, att: &VerificationAttestation) -> Result<()> {
        let entry = AttestationIndexEntry {
            attestation_id: att.attestation_id.as_str().to_string(),
        };
        Self::write_atomic(&self.attestation_index_path(&att.key), &entry)
    }

    // -------------------------------------------------------------- outbox

    /// Path of the interrupted terminal outcome held with a gate.
    fn held_outcome_path(&self, gate_id: &GateId) -> PathBuf {
        self.root
            .join("held")
            .join(format!("{}.json", gate_id.as_str()))
    }

    /// Park the terminal outcome the gate interrupted.
    ///
    /// Holding a candidate means *not* running the terminal transaction, so
    /// the outcome that transaction would have written has to survive
    /// somewhere until the gate settles — including across a restart. Without
    /// it a verified gate has nothing to release: the controller would know
    /// the code is good and still have no completion to hand back.
    ///
    /// Deliberately opaque JSON. This layer has no business knowing the shape
    /// of an execution outcome, and the alternative — importing the
    /// artifact-v2 model here — would make the evidence store depend on the
    /// task runtime it exists to stay independent of.
    pub fn write_held_outcome(&self, gate_id: &GateId, outcome: &serde_json::Value) -> Result<()> {
        Self::write_atomic(&self.held_outcome_path(gate_id), outcome)
    }

    /// The parked outcome, if one was recorded.
    ///
    /// Absent is not an error: gates opened before this existed, and
    /// `blocked_partial` gates that never had a releasable outcome, both read
    /// as `None`.
    pub fn read_held_outcome(&self, gate_id: &GateId) -> Result<Option<serde_json::Value>> {
        let path = self.held_outcome_path(gate_id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read held outcome {}", path.display()))?;
        Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parse held outcome {}", path.display())
        })?))
    }

    /// Where a claimed outcome lives between [`Self::take_held_outcome`] and
    /// [`Self::complete_held_release`] — i.e. while exactly one caller is
    /// mid-release.
    ///
    /// This directory is **enumerated**, by
    /// [`Self::reclaim_orphaned_held_outcomes`]. That is what makes the claim
    /// a lease on a release rather than a deletion: a rename into a directory
    /// nothing walks would leave the completion somewhere no code path can
    /// ever find it, and the task non-terminal forever.
    fn claimed_outcome_path(&self, gate_id: &GateId) -> PathBuf {
        self.claimed_outcome_dir()
            .join(format!("{}.json", gate_id.as_str()))
    }

    fn claimed_outcome_dir(&self) -> PathBuf {
        self.root.join("held/released")
    }

    /// Where a *finished* release's outcome is kept, for forensics.
    ///
    /// Distinct from the claimed directory so that "claimed" means exactly
    /// "claimed and not yet released". If completed releases stayed in the
    /// claimed directory, recovery would re-drive every gate this scope has
    /// ever settled.
    fn completed_outcome_path(&self, gate_id: &GateId) -> PathBuf {
        self.completed_outcome_dir()
            .join(format!("{}.json", gate_id.as_str()))
    }

    fn completed_outcome_dir(&self) -> PathBuf {
        self.root.join("held/completed")
    }

    /// Claim the parked outcome, exactly once, for release.
    ///
    /// `None` means somebody else already claimed it — **not** that there was
    /// never one. That distinction is the whole point: it is what stops a
    /// completion being run twice.
    ///
    /// The re-entry short-circuits in `persist_execution_outcome` cannot do
    /// this job on their own, because the two that key on `completed_at` need
    /// a `completed_at` a held root does not have — that is what being held
    /// means. So two releases racing (a duplicate driver, or a driver whose
    /// `run_pass` finds the gate already terminal) both sail past them.
    ///
    /// A `rename` is the fence. Two callers can both read the file, but only
    /// one can rename it away: after the winner's rename the source is gone,
    /// so the loser's `rename` fails with `NotFound`.
    ///
    /// **The claim is a lease, not a disposal.** It is broken by process exit:
    /// the only holder of an unreleased claim is an in-process release task,
    /// so at startup every record still sitting in the claimed directory
    /// belongs to a release that did not finish.
    /// [`Self::reclaim_orphaned_held_outcomes`] hands those back.
    /// [`Self::complete_held_release`] is what takes a record *out* of the
    /// claimed set once the release has landed.
    pub fn take_held_outcome(&self, gate_id: &GateId) -> Result<Option<serde_json::Value>> {
        let path = self.held_outcome_path(gate_id);
        let claimed = self.claimed_outcome_path(gate_id);
        if let Some(parent) = claimed.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        match std::fs::rename(&path, &claimed) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("claim held outcome {}", path.display()))
            },
        }
        let bytes = std::fs::read(&claimed)
            .with_context(|| format!("read claimed outcome {}", claimed.display()))?;
        Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parse claimed outcome {}", claimed.display())
        })?))
    }

    /// Put a claimed outcome back, so a release that failed can be retried.
    ///
    /// Only meaningful straight after a [`Self::take_held_outcome`] whose
    /// caller could not finish the release. Anything else would re-arm a
    /// completion that has already run.
    pub fn restore_held_outcome(&self, gate_id: &GateId) -> Result<()> {
        let claimed = self.claimed_outcome_path(gate_id);
        if !claimed.exists() {
            return Ok(());
        }
        std::fs::rename(&claimed, self.held_outcome_path(gate_id))
            .with_context(|| format!("restore held outcome {}", claimed.display()))
    }

    /// Whether some caller holds the release claim for this gate right now.
    ///
    /// Lets a losing release distinguish "the winner has it" — routine — from
    /// "there is no completion anywhere and this task can never finish" —
    /// which is the alarm.
    pub fn release_is_in_flight(&self, gate_id: &GateId) -> bool {
        self.claimed_outcome_path(gate_id).is_file()
    }

    /// Whether a release for this gate already ran to completion.
    pub fn release_has_completed(&self, gate_id: &GateId) -> bool {
        self.completed_outcome_path(gate_id).is_file()
    }

    /// Retire a claim whose release actually landed.
    ///
    /// The counterpart of [`Self::restore_held_outcome`]: that one says "the
    /// release did not happen, re-arm it", this one says "the release
    /// happened, stop offering it". Until one of the two is called, the gate
    /// is *in flight* and recovery will pick it up again — which is the
    /// property that makes a crash mid-release survivable.
    ///
    /// Idempotent, and a no-op for a gate that was never claimed.
    pub fn complete_held_release(&self, gate_id: &GateId) -> Result<()> {
        let claimed = self.claimed_outcome_path(gate_id);
        if !claimed.exists() {
            return Ok(());
        }
        let done = self.completed_outcome_path(gate_id);
        if let Some(parent) = done.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::rename(&claimed, &done)
            .with_context(|| format!("retire claimed outcome {}", claimed.display()))
    }

    /// Hand every unfinished claim back, and name every gate that still owes a
    /// release.
    ///
    /// **Startup only.** A claim is held by an in-process release task and
    /// nothing else, so a claim that outlived the process is by construction
    /// orphaned: the release either never ran the terminal transaction, or ran
    /// it and died before [`Self::complete_held_release`]. Both need the same
    /// treatment — re-drive, and let `persist_execution_outcome`'s replay
    /// guards decide whether there is anything left to do.
    ///
    /// Returns the gate ids whose outcome is parked and unreleased, claimed or
    /// not, so recovery can spawn a driver per gate. This is deliberately
    /// **not** derived from the outbox: every terminal settle retires the
    /// outbox entry as part of the transition it commits, and release runs
    /// *after* the settle, so on the release path the outbox is always already
    /// empty. The parked outcome is the only durable record that a task is
    /// still waiting to be completed.
    pub fn reclaim_orphaned_held_outcomes(&self) -> Result<Vec<GateId>> {
        for gate_id in Self::gate_ids_in(&self.claimed_outcome_dir())? {
            self.restore_held_outcome(&gate_id)?;
        }
        // Reclaim is "list, plus hand the orphaned claims back first". Sharing
        // the listing rather than repeating it is what keeps the reconciler's
        // sweep and startup recovery's from drifting apart.
        self.list_unreleased_held_outcomes()
    }

    /// Name every gate whose completion is still parked, touching nothing.
    ///
    /// The reconciler's half of what [`Self::reclaim_orphaned_held_outcomes`]
    /// does at startup. Reclaiming is startup-only — a claim is held by an
    /// in-process release task, so handing one back while the process is live
    /// would steal it from a release that is merely slow. This reads `held/`
    /// and nothing else; a claim orphaned by an in-process crash waits for
    /// the next restart, exactly as it did before the reconciler existed.
    pub fn list_unreleased_held_outcomes(&self) -> Result<Vec<GateId>> {
        Self::gate_ids_in(&self.root.join("held"))
    }

    /// Gate ids named by the `<gate>.json` files directly inside `dir`.
    ///
    /// Non-recursive on purpose: `held/` holds the parked outcomes and the
    /// `released/` + `completed/` subdirectories, and a subdirectory is not a
    /// parked outcome. Ids are parsed rather than trusted, so a stray filename
    /// cannot become a path component elsewhere.
    fn gate_ids_in(dir: &Path) -> Result<Vec<GateId>> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Ok(gate_id) = GateId::parse(stem) {
                out.push(gate_id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Remove any outbox entries pointing at `gate_id`.
    ///
    /// Projection repair for a gate that reached a terminal state without a
    /// settle path retiring its entry — notably `blocked_partial`, which is
    /// terminal from the moment the gate is opened. A live entry on a
    /// terminal gate is re-offered on every scheduler tick forever.
    pub fn retire_outbox_for_gate(&self, gate_id: &GateId) -> Result<()> {
        for entry in self.list_outbox()? {
            if &entry.gate_id == gate_id {
                let path = self.outbox_path(&entry.entry_id);
                if path.exists() {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("retire outbox entry {}", path.display()))?;
                }
            }
        }
        Ok(())
    }

    pub fn list_outbox(&self) -> Result<Vec<OutboxEntry>> {
        let dir = self.outbox_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("read outbox dir {}", dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read outbox entry {}", path.display()))?;
            let item: OutboxEntry = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse outbox entry {}", path.display()))?;
            out.push(item);
        }
        out.sort_by(|a, b| a.enqueued_at.cmp(&b.enqueued_at));
        Ok(out)
    }

    // ------------------------------------------------------------ recovery

    /// Rebuild every projection from the journal.
    ///
    /// Run at startup. The journal is authoritative, so this both repairs a
    /// projection that a crash left behind and is a no-op when everything is
    /// already consistent.
    pub fn recover(&self) -> Result<Vec<GateId>> {
        let _lock = self.lock("gates")?;
        let mut recovered = Vec::new();
        for gate_id in self.journal.gate_ids()? {
            let txns = self.journal.read_all(&gate_id)?;
            let Some(last) = txns.last() else { continue };
            // Fold the history we already hold. Calling `replay_gate` here
            // would re-read and re-parse every transaction a second time.
            let Some(folded) = VerificationJournal::fold(&txns)? else {
                continue;
            };

            // Renewals are projection-only, so the journal's `expires_at` is
            // frozen at claim time. Projecting it blindly would roll a live
            // worker's lease backwards — its next commit would fail the
            // liveness check and a finished verification would be thrown
            // away. Carry the projection's lease forward when it belongs to
            // the same generation, which means no takeover has happened and
            // it is the *same* lease, merely renewed.
            let mut gate = folded;
            if let Ok(projected) = self.load_gate(&gate_id) {
                if projected.generation == gate.generation && projected.lease.is_some() {
                    gate.lease = projected.lease;
                    gate.updated_at = projected.updated_at.max(gate.updated_at);
                }
            }
            Self::write_atomic(&self.gate_path(&gate_id), &gate)?;
            self.write_task_index(&gate)?;

            // Outbox and index still come from the committed transaction.
            self.project_side_effects(last)?;
            recovered.push(gate_id);
        }
        Ok(recovered)
    }

    /// Apply a committed transaction to the projections. Idempotent by
    /// construction: every write is a whole-value overwrite keyed by id.
    fn project(&self, txn: &JournalTransaction) -> Result<()> {
        let gate = txn.gate();
        // Every payload carries the gate, and every projection must keep the
        // task index in step with it — otherwise the task-read hot path would
        // have to fall back to scanning.
        self.write_task_index(gate)?;
        Self::write_atomic(&self.gate_path(&gate.gate_id), gate)?;
        self.project_side_effects(txn)
    }

    /// The outbox half of a projection, separated from the gate write.
    ///
    /// Recovery needs these effects but must *not* take the gate verbatim
    /// from the journal — see `recover`, which preserves a same-generation
    /// live lease that renewals kept out of the journal.
    fn project_side_effects(&self, txn: &JournalTransaction) -> Result<()> {
        let retire = |entry_id: &str| -> Result<()> {
            let path = self.outbox_path(entry_id);
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("retire outbox entry {}", path.display()))?;
            }
            Ok(())
        };

        match &txn.payload {
            JournalPayload::CandidateEntry { outbox, .. } => {
                Self::write_atomic(&self.outbox_path(&outbox.entry_id), outbox)?;
            },
            JournalPayload::GateUpdate { .. } => {},
            JournalPayload::GateSettled {
                retire_outbox_entry,
                ..
            } => retire(retire_outbox_entry)?,
            JournalPayload::RepairRound {
                retire_outbox_entry,
                enqueue,
                ..
            } => {
                retire(retire_outbox_entry)?;
                if let Some(enqueue) = enqueue {
                    Self::write_atomic(&self.outbox_path(&enqueue.entry_id), enqueue)?;
                }
            },
        }
        Ok(())
    }

    /// Refuse a record that belongs to another scope.
    ///
    /// Compared on the **directory segments**, not the raw strings. A scope is
    /// stored raw on the record but a store is often constructed from the
    /// directory name it was found in — `list_scopes` returns
    /// `safe_segment`-sanitised names, so a principal containing any of
    /// `/ \ : * ? " < > |` reaches here as `a_b` while its records say `a/b`.
    /// Comparing raw made startup recovery skip every gate in such a scope: the
    /// path resolved (the sanitiser is idempotent) but the record was then
    /// rejected as foreign, so the recovery that exists to find stranded gates
    /// found none.
    ///
    /// The property still holds. Two different scopes only collide after
    /// sanitisation if they differ solely by characters the filesystem cannot
    /// represent — in which case they already share one directory, and no
    /// comparison here could separate them.
    fn assert_scope(&self, scope: &TransactionScope, kind: &str) -> Result<()> {
        let segments = |s: &TransactionScope| {
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::scope_dir_segments(
                &s.principal,
                &s.workspace,
            )
        };
        if segments(scope) != segments(&self.scope) {
            return Err(anyhow!(
                "{kind} belongs to scope {}/{} but this store is {}/{}; cross-scope access refused",
                scope.principal,
                scope.workspace,
                self.scope.principal,
                self.scope.workspace
            ));
        }
        Ok(())
    }
}

/// Convenience: the terminal payload builder used at finalisation.
pub fn settled_payload(
    retire_outbox_entry: String,
    accepted: Option<AttestationId>,
) -> impl FnOnce(Box<VerificationGate>) -> JournalPayload {
    move |gate| JournalPayload::GateSettled {
        gate,
        retire_outbox_entry,
        accepted_attestation: accepted,
    }
}

/// Enqueue a work item for a repaired candidate.
///
/// Called when the engineer's successor candidate is registered — *not* when
/// repair starts. At repair time the prior candidate is invalidated and its
/// entry retired with no replacement, because there is nothing to verify
/// until the successor exists.
pub fn enqueue_repaired_candidate(gate_id: GateId, candidate: CandidateRevision) -> OutboxEntry {
    OutboxEntry {
        entry_id: format!("obx-{}", Uuid::new_v4()),
        gate_id,
        candidate,
        enqueued_at: Utc::now(),
        claimed_by: None,
    }
}

/// The gate status implied by a settled attestation, for callers translating
/// an accepted result into a terminal gate state.
pub fn status_for_outcome(outcome: AttemptOutcome) -> GateStatus {
    match outcome {
        AttemptOutcome::Green => GateStatus::Verified,
        // Red is not itself terminal — the controller decides repair vs
        // exhausted based on budget. Callers that reach here with Red are
        // recording the failed round, not settling the gate.
        AttemptOutcome::Red => GateStatus::Repairing,
        AttemptOutcome::Indeterminate => GateStatus::Unavailable,
    }
}
