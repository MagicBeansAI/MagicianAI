//! Task 15 SQLite/DuckDB kit. Local database files stay canonical.
//! Live Connection::open / DuckDB stay specialized. Remote adapters
//! and migration are test-only closed-file snapshots.

mod guard;
mod local;
mod migration;
mod owners;
mod remote;

pub use guard::database_owner_source_guard;
pub use local::{
    database_file_path, host_database_path, open_local_database_owner, persist_database_file,
    persist_database_file_sync, store_for_any_owner, store_for_existing_path, DatabaseAccess,
    LocalDatabaseStore,
};
pub use migration::DatabaseOwnerMigrationHandler;
pub use owners::DatabaseOwner;
pub use remote::{open_local_database_backend, RemoteDatabaseStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
