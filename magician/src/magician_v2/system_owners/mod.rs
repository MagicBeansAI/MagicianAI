//! Task 16 system, device, and secret kit. Local layouts stay canonical.
//! Secret export is metadata-only. Device-local stores are not migrated.
//! Remote adapters and migration are test-only.

mod guard;
mod local;
mod migration;
mod owners;
mod remote;

pub use guard::system_owner_source_guard;
pub use local::{
    host_system_path, open_local_system_owner, persist_system_file, persist_system_file_sync,
    store_for_any_owner, store_for_existing_path, system_digest, system_file_path, ExportedSystem,
    LocalSystemStore, SystemAccess,
};
pub use migration::SystemOwnerMigrationHandler;
pub use owners::SystemOwner;
pub use remote::{open_local_system_backend, RemoteSystemStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
