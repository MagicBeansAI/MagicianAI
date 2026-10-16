//! Task 16A subprocess and skill storage kit. Local workdirs and skill
//! working directories stay ephemeral until accepted. Accepted outputs
//! publish through Task 10 object owners. Remote adapters and migration
//! are test-only. Default startup does not cut over to remote.

mod envelope;
mod guard;
mod local;
mod migration;
mod owners;
mod remote;
mod shim;

pub use envelope::{
    active_skill_working_lease, env_is_closed, forbidden_child_env_names,
    install_skill_working_envelope, install_skill_working_envelope_from_exec, load_envelope,
    materialize_input, publish_accepted_output, scavenge_lease, set_active_skill_working_lease,
    InputManifestEntry, OutputSlot, PublicationReceipt, ResultManifestEntry, ScratchCleanupPolicy,
    ScratchLeaseSpec, SecretDelivery, SecretRequest, SubprocessStorageEnvelope, ENVELOPE_FILE_NAME,
    ENVELOPE_VERSION,
};
pub use guard::subprocess_owner_source_guard;
pub use local::{
    open_local_subprocess_owner, persist_subprocess_file, persist_subprocess_file_sync,
    store_for_any_owner, store_for_existing_path, workdirs_root, LocalSubprocessStore,
    SubprocessAccess,
};
pub use migration::SubprocessOwnerMigrationHandler;
pub use owners::SubprocessOwner;
pub use remote::{open_local_subprocess_backend, RemoteSubprocessStore};
pub use shim::emit_compat_metric;

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
