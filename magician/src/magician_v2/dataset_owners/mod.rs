//! Task 11 Parquet dataset kit. Local `dt=*` layouts stay canonical.
//! Remote `DatasetStore` adapters and migration are test-only.

mod family;
mod guard;
mod local;
mod migration;
mod remote;

pub use family::{family_parquet_glob, family_read_glob, DatasetFamily};
pub use guard::dataset_owner_source_guard;
pub use local::{
    open_local_dataset_family, part_digest, persist_dataset_file, persist_dataset_file_sync,
    publish_written_parquet, store_for_any_owner, store_for_existing_path, DatasetAccess,
    ExportedPart, LocalFamilyStore,
};
pub use migration::DatasetFamilyMigrationHandler;
pub use remote::{open_local_dataset_backend, RemoteFamilyStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
