// Versioned Prompt Storage System for MagicianV2
// Trait-based design with version selection in code

pub mod constants;
pub mod json_storage;
pub mod manager;
pub mod storage;
pub mod types;

#[cfg(test)]
mod test_prompt_storage;

pub use constants::{names, versions};
pub use json_storage::JsonPromptStorage;
pub use manager::{
    global_prompt_manager, managed_prompt, rendered_prompt, rendered_prompt_or,
    set_global_prompt_manager, PromptManager,
};
pub use storage::{PromptStore, StorageConfig};
pub use types::{Prompt, PromptCategory, PromptMetadata, PromptVariable};
