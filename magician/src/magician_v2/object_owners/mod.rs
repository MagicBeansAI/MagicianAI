//! Task 10 object-owner kit. Local directory layouts stay canonical.
//! Remote `ObjectStore` adapters and migration handlers are test-only.

mod blob;
mod guard;
mod migration;
mod object_backend;
mod owners;

pub use blob::{
    blob_digest, open_local_object_owner, persist_execution_recording, persist_object_file,
    persist_object_file_sync, read_object_file, store_for_any_owner, store_for_existing_path,
    BlobAccess, ExportedBlob, LocalTreeStore,
};
pub use guard::object_owner_source_guard;
pub use migration::ObjectOwnerMigrationHandler;
pub use object_backend::{open_local_object_backend, ObjectTreeStore};
pub use owners::ObjectOwner;

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
