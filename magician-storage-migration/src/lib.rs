//! Dormant owner-closure and migration coordinator.
//!
//! `magician-bin` must not depend on this crate. Normal startup does not
//! inventory, plan, or cut over any owner.

mod closure;
mod coordinator;
mod handler;
mod ledger;
mod source_guard;
mod synthetic;

pub use closure::{ClosureCoordinator, ClosurePacket};
pub use coordinator::{CrashSpec, MigrationCoordinator, PlanRequest, ResumeStatus};
pub use handler::{
    Checkpoint, ExportBlob, ImportReceipt, OwnerInventory, OwnerMigrationHandler,
    OwnerMigrationRegistry, RollbackReceipt, VerifyReport,
};
pub use ledger::{sanitize_json, LedgerEntry, PhaseEvidence};
pub use source_guard::SourceGuard;
pub use synthetic::{InMemoryRepository, SyntheticItem, SyntheticOwner, SYNTHETIC_OWNER_ID};

pub const SYNTHETIC_OWNER_MODULE: &str = "magician-storage-migration/src/synthetic.rs";
