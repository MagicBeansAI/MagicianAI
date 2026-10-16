//! Task 14 agent, memory, learning, and index kit. Local layouts stay
//! canonical. Remote adapters and migration are test-only.

mod guard;
mod local;
mod migration;
mod owners;
mod remote;

pub use guard::agent_owner_source_guard;
pub use local::{
    open_local_agent_owner, persist_agent_file, persist_agent_file_sync, store_for_any_owner,
    store_for_existing_path, AgentAccess, LocalAgentStore,
};
pub use migration::AgentOwnerMigrationHandler;
pub use owners::AgentOwner;
pub use remote::{open_local_agent_backend, RemoteAgentStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
