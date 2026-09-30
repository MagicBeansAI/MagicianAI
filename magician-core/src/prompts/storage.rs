//! Prompt storage utilities specific to Magician V2.

use anyhow::Result;

pub use runtime_core::PromptStore;

/// Storage configuration trait used by concrete backends.
pub trait StorageConfig {
    fn validate(&self) -> Result<()>;
    fn storage_type(&self) -> &str;
}
