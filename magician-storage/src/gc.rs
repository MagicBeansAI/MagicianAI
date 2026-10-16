//! Delayed, version-aware garbage collection.
//!
//! Logical delete leaves a tombstone and a retain file. Physical bytes stay
//! until the restore window elapses. GC never collects a referenced version
//! or the live replacement generation; unreferenced prior retains are
//! collected after the window even when a newer live object exists.

use std::time::Duration;

use crate::identifiers::StorageKey;
use crate::object::ObjectVersion;

/// Gate 2 object restore window: seven days of delayed physical collection.
pub const DEFAULT_TOMBSTONE_RETAIN: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectReference {
    pub key: StorageKey,
    pub version: ObjectVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcPolicy {
    pub tombstone_retain: Duration,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            tombstone_retain: DEFAULT_TOMBSTONE_RETAIN,
        }
    }
}

impl GcPolicy {
    pub fn immediate() -> Self {
        Self {
            tombstone_retain: Duration::from_secs(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcReport {
    pub scanned: u64,
    pub retained_referenced: u64,
    pub retained_fresh: u64,
    pub retained_live_replacement: u64,
    pub collected: u64,
}
