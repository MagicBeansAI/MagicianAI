//! Service-owned custody for sensitive answers that have no private oneshot
//! receiver (chat, agentic, forms). Material enters here from `respond_scoped`
//! after the durable resolution commits, and leaves only through one
//! in-process take that proves the request's scope. Nothing here is
//! serialized, published, or restored; a restart forgets it, which is the
//! intended failure mode — a reference never resurrects a value.
use super::*;

pub(super) struct SensitiveCustody {
    held: Mutex<HashMap<String, Held>>,
}

/// Material is held for at most this long after its deposit, however long the
/// request itself may wait for an answer. A consumer takes it within the
/// same turn; anything older is an orphan and is forgotten.
pub(super) const CUSTODY_HOLD_MAX_MS: i64 = 15 * 60 * 1000;

/// The custody deadline for a deposit: the collection deadline, capped by
/// the hold bound.
pub(super) fn custody_hold_deadline_ms(collection_deadline_ms: i64, now_ms: i64) -> i64 {
    collection_deadline_ms.min(now_ms.saturating_add(CUSTODY_HOLD_MAX_MS))
}

#[cfg_attr(not(test), allow(dead_code))]
struct Held {
    value: Zeroizing<String>,
    kind: SensitiveKind,
    request_id: String,
    principal: String,
    workspace: String,
    deadline_ms: i64,
}

/// A value-free note of a deposit the sweep dropped: enough to find the
/// history row whose `sensitive[]` entry still says `provided`.
pub(super) struct SweptDeposit {
    pub reference: String,
    pub request_id: String,
    pub principal: String,
    pub workspace: String,
}

/// A value-free description of one deposit, decided before the resolution is
/// persisted so the durable `UserResponse` can carry the reference.
pub(super) struct PendingDeposit {
    pub reference: String,
    pub kind: SensitiveKind,
    pub value: Zeroizing<String>,
}

impl SensitiveCustody {
    pub(super) fn new() -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn new_reference() -> String {
        format!("sr_{}", uuid::Uuid::new_v4().simple())
    }

    /// Hold `value` under an already-issued reference until `deadline_ms`.
    /// Expired entries are swept on every deposit so a scope that never takes
    /// its material does not accumulate it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn deposit(
        &self,
        reference: String,
        value: Zeroizing<String>,
        kind: SensitiveKind,
        request_id: &str,
        principal: &str,
        workspace: &str,
        deadline_ms: i64,
        now_ms: i64,
    ) {
        let mut held = self.held.lock().expect("sensitive custody lock poisoned");
        held.retain(|_, entry| entry.deadline_ms > now_ms);
        held.insert(
            reference,
            Held {
                value,
                kind,
                request_id: request_id.to_string(),
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                deadline_ms,
            },
        );
    }

    /// One-shot take. The caller proves the request and its scope; a
    /// mismatch returns `None` without consuming. An expired entry is dropped.
    pub(super) fn take(
        &self,
        reference: &str,
        request_id: &str,
        principal: &str,
        workspace: &str,
        now_ms: i64,
    ) -> Option<Zeroizing<String>> {
        let mut held = self.held.lock().expect("sensitive custody lock poisoned");
        let entry = held.get(reference)?;
        if entry.deadline_ms <= now_ms {
            held.remove(reference);
            return None;
        }
        if entry.request_id != request_id
            || entry.principal != principal
            || entry.workspace != workspace
        {
            return None;
        }
        held.remove(reference).map(|entry| entry.value)
    }

    /// Forget every deposit of a request: its owning execution is gone.
    ///
    /// Returns a value-free note of each drop, like [`Self::sweep_expired`],
    /// so the request's history row can stop saying `provided` for material
    /// that was discarded. A retired deposit is gone from custody, so the
    /// expiry sweep can never reach it — the row said `provided` forever, and
    /// an audit of "was this credential ever handed over?" read yes.
    pub(super) fn retire_request(&self, request_id: &str) -> Vec<SweptDeposit> {
        let mut held = self.held.lock().expect("sensitive custody lock poisoned");
        let mut retired = Vec::new();
        held.retain(|reference, entry| {
            if entry.request_id != request_id {
                return true;
            }
            retired.push(SweptDeposit {
                reference: reference.clone(),
                request_id: entry.request_id.clone(),
                principal: entry.principal.clone(),
                workspace: entry.workspace.clone(),
            });
            false
        });
        retired
    }

    /// Drop every deposit whose hold has ended, now rather than on the take
    /// that would have found it. Called on the service's own traffic (every
    /// accept and every response), so untaken material never outlives its
    /// deadline by more than the gap between two requests. Returns a
    /// value-free note of each drop so the request's history row can say
    /// `expired` where it said `provided`.
    pub(super) fn sweep_expired(&self, now_ms: i64) -> Vec<SweptDeposit> {
        let mut held = self.held.lock().expect("sensitive custody lock poisoned");
        let mut swept = Vec::new();
        held.retain(|reference, entry| {
            if entry.deadline_ms > now_ms {
                return true;
            }
            swept.push(SweptDeposit {
                reference: reference.clone(),
                request_id: entry.request_id.clone(),
                principal: entry.principal.clone(),
                workspace: entry.workspace.clone(),
            });
            false
        });
        swept
    }

    #[cfg(test)]
    pub(super) fn held_count(&self) -> usize {
        self.held
            .lock()
            .expect("sensitive custody lock poisoned")
            .len()
    }
}

/// Whether a submitted value is acceptable as credential material: bounded,
/// non-empty, and free of control characters.
pub(super) fn sensitive_value_is_acceptable(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}
