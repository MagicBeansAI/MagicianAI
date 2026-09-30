//! Task 13 chat and progress kit. Local session/transcript/progress
//! layouts stay canonical. Remote adapters and migration are test-only.

mod guard;
mod local;
mod migration;
mod owners;
mod remote;

pub use guard::chat_owner_source_guard;
pub use local::{
    open_local_chat_owner, persist_chat_file, persist_chat_file_sync, store_for_any_owner,
    store_for_existing_path, ChatAccess, LocalChatStore,
};
pub use migration::ChatOwnerMigrationHandler;
pub use owners::ChatOwner;
pub use remote::{open_local_chat_backend, RemoteChatStore};

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
