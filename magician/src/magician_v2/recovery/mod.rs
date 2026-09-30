//! Task 17 cross-owner backup, restore, integrity, and delayed GC.
//!
//! Not invoked at default startup. Local layouts stay canonical. Remote
//! logical export reuses owner kits already qualified in Tasks 10–16.

mod gate2;
mod integrity;
mod snapshot;

pub use gate2::{
    accepted_targets, DrillVerdict, Gate2Targets, RecoveryClass, RecoverySlo, ACCEPTED_AT,
    GATE2_OWNER,
};
pub use integrity::{IntegrityReport, RecoveryManifest, ReferencedObject};
pub use snapshot::{
    restore_representative_scope, snapshot_representative_scope, BackupEvidence, OwnerSnapshot,
    ProfileSnapshot, SignedSnapshot,
};

#[cfg(test)]
mod drills;
#[cfg(test)]
mod guard;
