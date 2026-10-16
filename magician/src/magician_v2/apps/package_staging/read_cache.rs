//! Bounded retention of immutable admitted bytes, never cached App authority.
use super::{AppDigest, AppPackageCandidate, AppScope, FileSnapshot};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 64 * 1024 * 1024;
type CacheKey = (AppScope, AppDigest);
pub(super) type Fingerprint = Vec<(String, FileSnapshot)>;

struct Entry {
    key: CacheKey,
    fingerprint: Fingerprint,
    index: Vec<u8>,
    candidate: AppPackageCandidate,
    bytes: usize,
}

#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct AppPackageReadStats {
    pub hits: u64,
    pub admissions: u64,
}

#[derive(Default)]
pub(super) struct VerifiedPackageReadCache {
    state: Mutex<State>,
    gates: Mutex<HashMap<CacheKey, Weak<tokio::sync::Mutex<()>>>>,
    hits: AtomicU64,
    admissions: AtomicU64,
}

impl VerifiedPackageReadCache {
    pub(super) fn gate(&self, scope: &AppScope, digest: &AppDigest) -> Arc<tokio::sync::Mutex<()>> {
        let key = (scope.clone(), digest.clone());
        let mut gates = self.gates.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
            return gate;
        }
        if gates.len() >= MAX_ENTRIES {
            gates.retain(|_, gate| gate.strong_count() != 0);
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        gates.insert(key, Arc::downgrade(&gate));
        gate
    }

    pub(super) fn stats(&self) -> AppPackageReadStats {
        AppPackageReadStats {
            hits: self.hits.load(Ordering::Relaxed),
            admissions: self.admissions.load(Ordering::Relaxed),
        }
    }

    /// Every hit first reopens the exact scoped directory and rechecks the
    /// complete descriptor-pinned metadata inventory plus exact index bytes.
    pub(super) fn get(
        &self,
        scope: &AppScope,
        digest: &AppDigest,
        fingerprint: &Fingerprint,
        index: &[u8],
    ) -> Option<AppPackageCandidate> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let position = state
            .entries
            .iter()
            .position(|entry| entry.key.0 == *scope && entry.key.1 == *digest)?;
        let entry = state.entries.remove(position)?;
        if entry.fingerprint != *fingerprint || entry.index != index {
            state.bytes = state.bytes.saturating_sub(entry.bytes);
            return None;
        }
        let candidate = entry.candidate.clone();
        state.entries.push_back(entry);
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(candidate)
    }

    pub(super) fn insert(
        &self,
        scope: &AppScope,
        digest: &AppDigest,
        fingerprint: Fingerprint,
        index: Vec<u8>,
        candidate: &AppPackageCandidate,
    ) {
        self.admissions.fetch_add(1, Ordering::Relaxed);
        let bytes = candidate
            .members()
            .iter()
            .map(|member| member.bytes().len())
            .sum::<usize>()
            + index.len();
        if bytes > MAX_BYTES {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(position) = state
            .entries
            .iter()
            .position(|entry| entry.key.0 == *scope && entry.key.1 == *digest)
        {
            let old = state.entries.remove(position).expect("entry observed");
            state.bytes = state.bytes.saturating_sub(old.bytes);
        }
        while state.entries.len() >= MAX_ENTRIES || state.bytes + bytes > MAX_BYTES {
            let Some(old) = state.entries.pop_front() else {
                break;
            };
            state.bytes = state.bytes.saturating_sub(old.bytes);
        }
        state.bytes += bytes;
        state.entries.push_back(Entry {
            key: (scope.clone(), digest.clone()),
            fingerprint,
            index,
            candidate: candidate.clone(),
            bytes,
        });
    }
}
