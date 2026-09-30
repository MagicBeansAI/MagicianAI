//! Task 12 work-spine kit. Local task/execution/plan/pause/list-index
//! layouts stay canonical. Remote adapters and migration are test-only.

mod guard;
mod local;
mod migration;
mod owners;
mod remote;

pub use guard::work_owner_source_guard;
pub use local::{
    open_local_work_owner, persist_work_file, persist_work_file_sync, store_for_any_owner,
    store_for_existing_path, LocalWorkStore, WorkAccess,
};
pub use migration::WorkOwnerMigrationHandler;
pub use owners::WorkOwner;
pub use remote::{open_local_work_backend, RemoteWorkStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
