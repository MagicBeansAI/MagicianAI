//! Core types and service contracts shared across runtime crates and Magician V2.
//!
//! This crate contains only the data structures and traits Magician V2 depends on.

pub mod context;
pub mod cua;
pub mod discovery;
pub mod edge;
pub mod fair_queue;
pub mod prelude;
pub mod process;
pub mod prompts;
pub mod runtime;
pub mod services;
pub mod storage;
pub mod tooling;

pub use context::{ExecutionContext, UserPreferences};
pub use discovery::ToolDiscovery;
pub use process::{resolve_program, resolve_program_str};
pub use prompts::{Prompt, PromptCategory, PromptMetadata, PromptStore, PromptVariable};
pub use runtime::{
    FileSandboxConfig, FileSandboxMode, OnViolation, RuntimeConfig, ShellSandboxConfig,
    ShellSandboxMode,
};
pub use services::{SemanticSearch, ToolCatalog, ToolMatching, ToolServices};
pub use storage::{
    PaginatedResult, PaginationInfo, PaginationParams, V2ConversationStore, WorkAuthorityGrant,
};
pub use tooling::{
    MultipleToolMatchResult, ParameterDefinition, ParameterMapping, ParameterSource,
    ParameterValue, SemanticMatch, SuccessMetrics, ToolInfo, ToolMatch, ToolMatchResult,
    ToolMetadata,
};
