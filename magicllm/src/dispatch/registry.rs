//! In-memory registry of jobs, with ring buffers for completed + tombstoned.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedSemaphorePermit;

use super::job::JobMeta;
use super::types::{JobId, TombstoneReason};

/// Live job state plus a bounded history of recently terminated jobs.
pub struct JobRegistry {
    pub pending: DashMap<JobId, JobMeta>,
    pub in_flight: DashMap<JobId, JobMeta>,
    /// External cancellation intents recorded by `cancel_task` /
    /// `cancel_chat_session` / `cancel_job`. The worker consults this map
    /// at pickup to plug the race window between removing a pending
    /// registry entry and the channel-side LlmJob being picked up.
    cancellation_intent: DashMap<JobId, TombstoneReason>,
    /// Provider reservations acquired by the provider admission coordinator
    /// and consumed at resumed worker pickup. The provider identity travels
    /// with the permit so a hot route change can never execute with a permit
    /// obtained from a different provider.
    capacity_permits: Mutex<HashMap<JobId, ProviderCapacityPermit>>,
    /// Queue admission reservations. A reservation is retained while a job is
    /// in a lane, executing, sleeping for retry, or waiting for provider
    /// capacity. This makes the configured lane capacities true bounds on all
    /// admitted synchronous work rather than only on channel occupancy.
    admission_permits: Mutex<HashMap<JobId, OwnedSemaphorePermit>>,
    completed: Mutex<VecDeque<JobMeta>>,
    tombstoned: Mutex<VecDeque<JobMeta>>,
    failed: Mutex<VecDeque<JobMeta>>,
    completed_cap: usize,
    tombstoned_cap: usize,
    failed_cap: usize,
}

impl JobRegistry {
    /// Construct with bounded ring buffers.
    pub fn new(completed_cap: usize, tombstoned_cap: usize, failed_cap: usize) -> Arc<Self> {
        Arc::new(Self {
            pending: DashMap::new(),
            in_flight: DashMap::new(),
            cancellation_intent: DashMap::new(),
            capacity_permits: Mutex::new(HashMap::new()),
            admission_permits: Mutex::new(HashMap::new()),
            completed: Mutex::new(VecDeque::with_capacity(completed_cap)),
            tombstoned: Mutex::new(VecDeque::with_capacity(tombstoned_cap)),
            failed: Mutex::new(VecDeque::with_capacity(failed_cap)),
            completed_cap,
            tombstoned_cap,
            failed_cap,
        })
    }

    /// Record an external cancellation intent for a job that may still be in
    /// the lane channel awaiting worker pickup. The actual transition to a
    /// terminal state (timing, ring push, event emission and response
    /// resolution) is done by the worker after it takes the intent. An intent
    /// must not make a still-queued job appear durably tombstoned.
    pub fn record_cancellation_intent(&self, job_id: &JobId, reason: TombstoneReason) {
        self.cancellation_intent.insert(job_id.clone(), reason);
        self.capacity_permits.lock().remove(job_id);
    }

    pub fn store_admission_permit(&self, job_id: &JobId, permit: OwnedSemaphorePermit) {
        self.admission_permits.lock().insert(job_id.clone(), permit);
    }

    pub fn release_admission_permit(&self, job_id: &JobId) {
        self.admission_permits.lock().remove(job_id);
    }

    pub fn store_capacity_permit(
        &self,
        job_id: &JobId,
        provider: crate::capability::LLMProviderKind,
        permit: OwnedSemaphorePermit,
    ) {
        self.capacity_permits
            .lock()
            .insert(job_id.clone(), ProviderCapacityPermit { provider, permit });
    }

    pub fn take_capacity_permit(
        &self,
        job_id: &JobId,
        expected_provider: &crate::capability::LLMProviderKind,
    ) -> Option<OwnedSemaphorePermit> {
        let reserved = self.capacity_permits.lock().remove(job_id)?;
        if &reserved.provider == expected_provider {
            Some(reserved.permit)
        } else {
            None
        }
    }

    pub fn release_capacity_permit(&self, job_id: &JobId) {
        self.capacity_permits.lock().remove(job_id);
    }

    /// Take and clear any pending cancellation intent.
    pub fn take_cancellation_intent(&self, job_id: &JobId) -> Option<TombstoneReason> {
        self.cancellation_intent.remove(job_id).map(|(_, r)| r)
    }

    pub fn has_cancellation_intent(&self, job_id: &JobId) -> bool {
        self.cancellation_intent.contains_key(job_id)
    }

    /// Mark a job as pending (just submitted).
    pub fn insert_pending(&self, meta: JobMeta) {
        self.pending.insert(meta.job_id.clone(), meta);
    }

    /// Transition pending → in_flight.
    pub fn mark_in_flight(&self, job_id: &JobId, mutator: impl FnOnce(&mut JobMeta)) {
        if let Some((_, mut meta)) = self.pending.remove(job_id) {
            mutator(&mut meta);
            self.in_flight.insert(job_id.clone(), meta);
        }
    }

    /// Transition pending → terminal (skipping in_flight) for tombstones at
    /// the pre-dispatch gate.
    pub fn pending_to_tombstone(
        &self,
        job_id: &JobId,
        mutator: impl FnOnce(&mut JobMeta),
    ) -> Option<JobMeta> {
        self.release_capacity_permit(job_id);
        self.release_admission_permit(job_id);
        if let Some((_, mut meta)) = self.pending.remove(job_id) {
            mutator(&mut meta);
            self.push_tombstone(meta.clone());
            return Some(meta);
        }
        // Could also be in_flight if the cancel signal fired after pickup.
        if let Some((_, mut meta)) = self.in_flight.remove(job_id) {
            mutator(&mut meta);
            self.push_tombstone(meta.clone());
            return Some(meta);
        }
        None
    }

    /// Transition in_flight → completed.
    pub fn in_flight_to_completed(
        &self,
        job_id: &JobId,
        mutator: impl FnOnce(&mut JobMeta),
    ) -> Option<JobMeta> {
        self.release_capacity_permit(job_id);
        self.release_admission_permit(job_id);
        if let Some((_, mut meta)) = self.in_flight.remove(job_id) {
            mutator(&mut meta);
            self.push_completed(meta.clone());
            return Some(meta);
        }
        None
    }

    /// Transition in_flight → failed. Also recovers if the job is still
    /// in `pending` (caller terminal-failed it at the pre-dispatch gate,
    /// before mark_in_flight).
    pub fn in_flight_to_failed(
        &self,
        job_id: &JobId,
        mutator: impl FnOnce(&mut JobMeta),
    ) -> Option<JobMeta> {
        self.release_capacity_permit(job_id);
        self.release_admission_permit(job_id);
        if let Some((_, mut meta)) = self.in_flight.remove(job_id) {
            mutator(&mut meta);
            self.push_failed(meta.clone());
            return Some(meta);
        }
        if let Some((_, mut meta)) = self.pending.remove(job_id) {
            mutator(&mut meta);
            self.push_failed(meta.clone());
            return Some(meta);
        }
        None
    }

    /// Re-queue: move from in_flight back to pending (with mutated state).
    pub fn in_flight_to_pending(
        &self,
        job_id: &JobId,
        mutator: impl FnOnce(&mut JobMeta),
    ) -> Option<JobMeta> {
        if let Some((_, mut meta)) = self.in_flight.remove(job_id) {
            mutator(&mut meta);
            self.pending.insert(job_id.clone(), meta.clone());
            return Some(meta);
        }
        None
    }

    /// Cap-respecting push into completed ring.
    pub fn push_completed(&self, meta: JobMeta) {
        let mut ring = self.completed.lock();
        if ring.len() >= self.completed_cap {
            ring.pop_front();
        }
        ring.push_back(meta);
    }

    /// Cap-respecting push into tombstone ring.
    pub fn push_tombstone(&self, meta: JobMeta) {
        let mut ring = self.tombstoned.lock();
        if ring.len() >= self.tombstoned_cap {
            ring.pop_front();
        }
        ring.push_back(meta);
    }

    /// Cap-respecting push into failed ring.
    pub fn push_failed(&self, meta: JobMeta) {
        let mut ring = self.failed.lock();
        if ring.len() >= self.failed_cap {
            ring.pop_front();
        }
        ring.push_back(meta);
    }

    /// Snapshot all rings for the viewer.
    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            pending: self.pending.iter().map(|e| e.value().clone()).collect(),
            in_flight: self.in_flight.iter().map(|e| e.value().clone()).collect(),
            completed: self.completed.lock().iter().cloned().collect(),
            failed: self.failed.lock().iter().cloned().collect(),
            tombstoned: self.tombstoned.lock().iter().cloned().collect(),
        }
    }

    /// Look up a specific job by id across all live + recent rings.
    pub fn find(&self, job_id: &JobId) -> Option<JobMeta> {
        if let Some(meta) = self.pending.get(job_id) {
            return Some(meta.value().clone());
        }
        if let Some(meta) = self.in_flight.get(job_id) {
            return Some(meta.value().clone());
        }
        for ring in [&self.completed, &self.failed, &self.tombstoned] {
            if let Some(meta) = ring.lock().iter().find(|m| &m.job_id == job_id) {
                return Some(meta.clone());
            }
        }
        None
    }

    /// Find a tombstoned/failed entry to feed `resubmit_failed`.
    pub fn find_dead_letter(&self, job_id: &JobId) -> Option<JobMeta> {
        for ring in [&self.failed, &self.tombstoned] {
            if let Some(meta) = ring.lock().iter().find(|m| &m.job_id == job_id) {
                return Some(meta.clone());
            }
        }
        None
    }
}

struct ProviderCapacityPermit {
    provider: crate::capability::LLMProviderKind,
    permit: OwnedSemaphorePermit,
}

/// Snapshot for the viewer endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub pending: Vec<JobMeta>,
    pub in_flight: Vec<JobMeta>,
    pub completed: Vec<JobMeta>,
    pub failed: Vec<JobMeta>,
    pub tombstoned: Vec<JobMeta>,
}
