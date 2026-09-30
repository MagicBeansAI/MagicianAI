//! Write-ahead journal for the verification controller.
//!
//! §4.1a of the plan requires that the gate, the root execution's
//! `verification_pending` state and the outbox entry be established
//! *atomically*. Three independently written files cannot be made atomic by
//! describing them as atomic — any two without the third leaves a
//! recoverable-looking system in an unrecoverable state:
//!
//! * a non-terminal task with no verification job (hangs forever);
//! * a job with no gated task (worker verifies a task nobody is holding);
//! * a green record that terminalises twice;
//! * a cancelled task later completed by a stale worker.
//!
//! So the three writes are encoded as **one** journal record, committed with a
//! single atomic rename, and only then projected onto the three files. A crash
//! before the rename leaves nothing; a crash after it leaves a committed record
//! that replay will finish. Projection is idempotent, so replaying a committed
//! transaction any number of times converges to the same state.
//!
//! This is deliberately a local write-ahead log rather than a new global
//! journal: it lives beside the execution state it gates, in the same
//! task-scoped directory, so a scope's verification state can never be
//! resolved from another scope's log.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::gate::VerificationGate;
use super::ids::{AttestationId, CandidateRevision, GateId};

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

/// A durable unit of verification work, recovered by replay after a crash.
///
/// The outbox is what guarantees a gated task is always reachable by *some*
/// worker: the gate alone would leave a task pending with nothing scheduled to
/// act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub entry_id: String,
    pub gate_id: GateId,
    pub candidate: CandidateRevision,
    pub enqueued_at: DateTime<Utc>,
    /// Set when a worker has durably taken the entry. Replay does not
    /// re-enqueue a claimed entry; lease expiry is what releases it.
    pub claimed_by: Option<String>,
}

/// What a committed transaction asserts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalPayload {
    /// The atomic entry transition of §4.3: candidate + gate state + outbox
    /// entry, all three or none.
    CandidateEntry {
        gate: Box<VerificationGate>,
        outbox: OutboxEntry,
    },
    /// A gate state change with no outbox implication.
    GateUpdate { gate: Box<VerificationGate> },
    /// A gate transition that also retires its outbox entry — finalisation,
    /// cancellation, or any terminal outcome.
    GateSettled {
        gate: Box<VerificationGate>,
        retire_outbox_entry: String,
        accepted_attestation: Option<AttestationId>,
    },
    /// A repair round: the prior candidate is invalidated.
    ///
    /// `enqueue` is `None` while the repair is in flight. The candidate is
    /// invalidated *at the moment repair starts* (§4.2), so there is nothing
    /// to verify until the engineer produces a successor — enqueueing work for
    /// a candidate that does not exist yet would hand a worker the pre-repair
    /// tree to verify.
    RepairRound {
        gate: Box<VerificationGate>,
        retire_outbox_entry: String,
        #[serde(default)]
        enqueue: Option<OutboxEntry>,
    },
}

/// One committed transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalTransaction {
    pub schema_version: u32,
    pub txn_id: String,
    /// Monotonic within a gate. Replay applies in this order.
    pub sequence: u64,
    pub gate_id: GateId,
    pub committed_at: DateTime<Utc>,
    pub payload: JournalPayload,
    /// Integrity check over the serialized payload. A torn or corrupted record
    /// is refused rather than half-applied — corrupt storage must never
    /// produce green evidence.
    pub payload_digest: String,
}

impl JournalTransaction {
    pub fn new(gate_id: GateId, sequence: u64, payload: JournalPayload) -> Result<Self> {
        let payload_digest = digest_payload(&payload)?;
        Ok(Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            txn_id: format!("vtxn-{}", Uuid::new_v4()),
            sequence,
            gate_id,
            committed_at: Utc::now(),
            payload,
            payload_digest,
        })
    }

    /// Refuse anything we do not fully understand or whose bytes drifted.
    pub fn verify(&self) -> Result<()> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(anyhow!(
                "verification journal txn {} has schema version {}; this build understands {}",
                self.txn_id,
                self.schema_version,
                JOURNAL_SCHEMA_VERSION
            ));
        }
        let expected = digest_payload(&self.payload)?;
        if expected != self.payload_digest {
            return Err(anyhow!(
                "verification journal txn {} failed its integrity check",
                self.txn_id
            ));
        }
        Ok(())
    }

    pub fn gate(&self) -> &VerificationGate {
        match &self.payload {
            JournalPayload::CandidateEntry { gate, .. }
            | JournalPayload::GateUpdate { gate }
            | JournalPayload::GateSettled { gate, .. }
            | JournalPayload::RepairRound { gate, .. } => gate,
        }
    }
}

fn digest_payload(payload: &JournalPayload) -> Result<String> {
    let bytes = serde_json::to_vec(payload).context("serialize verification journal payload")?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Append-only transaction log for one scope's verification activity.
///
/// Layout, under the scope root:
///
/// ```text
/// verification/
///   journal/<gate_id>/<sequence>.json     committed transactions
///   gates/<gate_id>.json                  projected gate
///   attestations/<attestation_id>.json    projected attestations
///   outbox/<entry_id>.json                projected work items
/// ```
///
/// The journal is the source of truth; the other three are projections. A
/// projection that disagrees with the journal is repaired by replay, never the
/// other way round.
pub struct VerificationJournal {
    root: PathBuf,
}

impl VerificationJournal {
    pub fn new(verification_root: impl Into<PathBuf>) -> Self {
        Self {
            root: verification_root.into().join("journal"),
        }
    }

    fn gate_dir(&self, gate_id: &GateId) -> PathBuf {
        self.root.join(gate_id.as_str())
    }

    fn txn_path(&self, gate_id: &GateId, sequence: u64) -> PathBuf {
        // Zero-padded so lexical order matches numeric order, which keeps
        // replay correct even when a reader sorts filenames as strings.
        self.gate_dir(gate_id).join(format!("{sequence:020}.json"))
    }

    /// The next sequence number for a gate.
    pub fn next_sequence(&self, gate_id: &GateId) -> Result<u64> {
        Ok(self.last_sequence(gate_id)?.map_or(0, |s| s + 1))
    }

    pub fn last_sequence(&self, gate_id: &GateId) -> Result<Option<u64>> {
        let dir = self.gate_dir(gate_id);
        if !dir.is_dir() {
            return Ok(None);
        }
        let mut max: Option<u64> = None;
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read verification journal dir {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            // Ignore in-flight temp files; only committed records count.
            let Ok(seq) = stem.parse::<u64>() else {
                continue;
            };
            max = Some(max.map_or(seq, |m: u64| m.max(seq)));
        }
        Ok(max)
    }

    /// Commit a transaction with a single atomic rename.
    ///
    /// Before this returns, either the whole record is durable or none of it
    /// is. Refuses to overwrite an existing sequence, so a duplicate commit is
    /// an error rather than a silent history rewrite.
    pub fn commit(&self, txn: &JournalTransaction) -> Result<()> {
        txn.verify()?;
        let dir = self.gate_dir(&txn.gate_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create verification journal dir {}", dir.display()))?;
        let path = self.txn_path(&txn.gate_id, txn.sequence);
        if path.exists() {
            return Err(anyhow!(
                "verification journal already has sequence {} for gate {}",
                txn.sequence,
                txn.gate_id
            ));
        }
        let bytes = serde_json::to_vec_pretty(txn).context("serialize verification journal txn")?;
        // Unique temp name so two concurrent committers cannot clobber each
        // other's partial write before the rename.
        let tmp = dir.join(format!(
            "{:020}.{}.json.tmp",
            txn.sequence,
            Uuid::new_v4().simple()
        ));
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("write verification journal tmp {}", tmp.display()))?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                Err(err).with_context(|| {
                    format!("commit verification journal txn -> {}", path.display())
                })
            },
        }
    }

    /// Read every committed transaction for a gate, in sequence order.
    ///
    /// A corrupt record aborts the read rather than being skipped: silently
    /// skipping a transaction is how a gate ends up projected into a state
    /// nothing ever committed.
    pub fn read_all(&self, gate_id: &GateId) -> Result<Vec<JournalTransaction>> {
        let dir = self.gate_dir(gate_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<(u64, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read verification journal dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            let Ok(seq) = stem.parse::<u64>() else {
                continue;
            };
            paths.push((seq, path));
        }
        paths.sort_by_key(|(seq, _)| *seq);

        let mut out = Vec::with_capacity(paths.len());
        for (seq, path) in paths {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read verification journal txn {}", path.display()))?;
            let txn: JournalTransaction = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse verification journal txn {}", path.display()))?;
            txn.verify()
                .with_context(|| format!("verify verification journal txn {}", path.display()))?;
            if txn.sequence != seq {
                return Err(anyhow!(
                    "verification journal txn {} claims sequence {} but is filed at {}",
                    txn.txn_id,
                    txn.sequence,
                    seq
                ));
            }
            out.push(txn);
        }
        Ok(out)
    }

    /// Fold a gate's whole history into its current state.
    ///
    /// This is the recovery path: the projected `gates/<id>.json` is a cache,
    /// and this is what it is a cache *of*.
    pub fn replay_gate(&self, gate_id: &GateId) -> Result<Option<VerificationGate>> {
        Self::fold(&self.read_all(gate_id)?)
    }

    /// Fold an already-read history. Split out so callers that need both the
    /// transactions and the folded state do not read the journal twice.
    pub fn fold(txns: &[JournalTransaction]) -> Result<Option<VerificationGate>> {
        let mut current: Option<VerificationGate> = None;
        for txn in txns {
            let next = txn.gate().clone();
            if let Some(prior) = &current {
                // Replay is held to the same invariants as a live write, so a
                // journal that was tampered with cannot walk a gate into a
                // state the typed API would have refused.
                next.validate_successor(prior).with_context(|| {
                    format!(
                        "verification journal txn {} is not a legal successor",
                        txn.txn_id
                    )
                })?;
            }
            current = Some(next);
        }
        Ok(current)
    }

    /// Every gate id with journal history in this scope. Used at startup to
    /// find work abandoned by a crashed process.
    pub fn gate_ids(&self) -> Result<Vec<GateId>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("read verification journal root {}", self.root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Unparseable directory names are ignored rather than fatal: a
            // stray directory should not take down recovery for every gate.
            if let Ok(id) = GateId::parse(name) {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use crate::magician_v2::execution::verification::gate::{
        GateBudgets, GateOrigin, GateStatus, VerificationGate,
    };

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn gate() -> VerificationGate {
        VerificationGate::new(
            TransactionScope {
                principal: "anonymous".into(),
                workspace: "default".into(),
            },
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 1).unwrap(),
            GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: Some("pi".into()),
                constraint_auto: false,
                coding_invocation_ref: None,
                child_execution_id: None,
            },
            GateBudgets::default(),
        )
        .unwrap()
    }

    fn outbox(gate: &VerificationGate) -> OutboxEntry {
        OutboxEntry {
            entry_id: "obx-1".into(),
            gate_id: gate.gate_id.clone(),
            candidate: gate.current_candidate.clone(),
            enqueued_at: Utc::now(),
            claimed_by: None,
        }
    }

    fn entry_txn(g: &VerificationGate, seq: u64) -> JournalTransaction {
        JournalTransaction::new(
            g.gate_id.clone(),
            seq,
            JournalPayload::CandidateEntry {
                gate: Box::new(g.clone()),
                outbox: outbox(g),
            },
        )
        .unwrap()
    }

    #[test]
    fn commit_then_replay_round_trips_all_three_parts() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();

        let txns = j.read_all(&g.gate_id).unwrap();
        assert_eq!(txns.len(), 1);
        match &txns[0].payload {
            JournalPayload::CandidateEntry { gate, outbox } => {
                // Both parts survive as one unit — that is the point.
                assert_eq!(gate.gate_id, g.gate_id);
                assert_eq!(outbox.gate_id, g.gate_id);
            },
            other => panic!("unexpected payload {other:?}"),
        }
    }

    #[test]
    fn replaying_a_committed_transaction_is_idempotent() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();

        let first = j.replay_gate(&g.gate_id).unwrap().unwrap();
        let second = j.replay_gate(&g.gate_id).unwrap().unwrap();
        let third = j.replay_gate(&g.gate_id).unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn a_duplicate_sequence_is_refused_rather_than_rewriting_history() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();
        assert!(j.commit(&entry_txn(&g, 0)).is_err());
    }

    #[test]
    fn a_torn_or_edited_record_is_refused_not_half_applied() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        let txn = entry_txn(&g, 0);
        j.commit(&txn).unwrap();

        // Tamper with the committed bytes the way a corrupt disk or a manual
        // edit would.
        let path = j.txn_path(&g.gate_id, 0);
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["payload"]["gate"]["status"] = serde_json::json!("verified");
        std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

        // Corrupt storage must never yield usable (let alone green) state.
        assert!(j.read_all(&g.gate_id).is_err());
        assert!(j.replay_gate(&g.gate_id).is_err());
    }

    #[test]
    fn an_unknown_schema_version_is_refused() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        let mut txn = entry_txn(&g, 0);
        txn.schema_version = JOURNAL_SCHEMA_VERSION + 1;
        assert!(txn.verify().is_err());
        assert!(j.commit(&txn).is_err());
    }

    #[test]
    fn crash_before_rename_leaves_nothing_committed() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();

        // Simulate the pre-rename state: a temp file exists, the record does
        // not. Recovery must see no transaction at all.
        let gdir = j.gate_dir(&g.gate_id);
        std::fs::create_dir_all(&gdir).unwrap();
        std::fs::write(gdir.join("00000000000000000000.abc.json.tmp"), b"{partial").unwrap();

        assert!(j.read_all(&g.gate_id).unwrap().is_empty());
        assert!(j.replay_gate(&g.gate_id).unwrap().is_none());
        assert_eq!(j.next_sequence(&g.gate_id).unwrap(), 0);
    }

    #[test]
    fn replay_enforces_successor_rules_on_tampered_history() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();

        // A second transaction that walks the gate backwards. Committing it
        // is possible (the journal does not know the prior state), but replay
        // must refuse to fold it.
        let mut regressed = g.clone();
        regressed.status = GateStatus::Verified;
        let settled = JournalTransaction::new(
            g.gate_id.clone(),
            1,
            JournalPayload::GateUpdate {
                gate: Box::new(regressed.clone()),
            },
        )
        .unwrap();
        j.commit(&settled).unwrap();
        assert!(j.replay_gate(&g.gate_id).unwrap().is_some());

        // Now an illegal transition out of the terminal state.
        let mut back_to_pending = regressed;
        back_to_pending.status = GateStatus::VerificationPending;
        let bad = JournalTransaction::new(
            g.gate_id.clone(),
            2,
            JournalPayload::GateUpdate {
                gate: Box::new(back_to_pending),
            },
        )
        .unwrap();
        j.commit(&bad).unwrap();
        assert!(j.replay_gate(&g.gate_id).is_err());
    }

    #[test]
    fn sequences_replay_in_numeric_not_lexical_order() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();
        for seq in [1u64, 2, 10, 11] {
            let mut next = g.clone();
            next.generation = super::super::ids::Generation(seq);
            j.commit(
                &JournalTransaction::new(
                    g.gate_id.clone(),
                    seq,
                    JournalPayload::GateUpdate {
                        gate: Box::new(next),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        }
        let txns = j.read_all(&g.gate_id).unwrap();
        let seqs: Vec<u64> = txns.iter().map(|t| t.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2, 10, 11]);
        assert_eq!(j.last_sequence(&g.gate_id).unwrap(), Some(11));
        assert_eq!(j.next_sequence(&g.gate_id).unwrap(), 12);
    }

    #[test]
    fn gate_ids_lists_recoverable_work_and_ignores_strays() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        j.commit(&entry_txn(&g, 0)).unwrap();
        std::fs::create_dir_all(j.root.join("not a valid id")).unwrap();

        let ids = j.gate_ids().unwrap();
        assert_eq!(ids, vec![g.gate_id.clone()]);
    }

    #[test]
    fn an_absent_journal_is_empty_not_an_error() {
        let dir = temp_root();
        let j = VerificationJournal::new(dir.path());
        let g = gate();
        assert!(j.gate_ids().unwrap().is_empty());
        assert!(j.read_all(&g.gate_id).unwrap().is_empty());
        assert!(j.replay_gate(&g.gate_id).unwrap().is_none());
    }
}
