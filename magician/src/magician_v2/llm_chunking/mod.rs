//! Magician-owned domain adapter registration and routing validation.
//!
//! `magicllm` owns generic planning. This module binds those mechanics to
//! Magician operation names without changing provider transports.

mod archive_checkpoint;
mod registry;
mod runner;

pub use archive_checkpoint::{archive_checkpoint_groups, MAX_ARCHIVE_GROUP_EPISODES};

pub use registry::ChunkReleaseCandidateSpec;
pub use registry::CHUNK_RELEASE_CANDIDATES;
pub use registry::{
    global_chunk_adapter_registry, register_global_chunk_adapter, validate_router_chunking_config,
    validate_router_chunking_config_with_registry, ChunkAdapterInventoryEntry,
    ChunkAdapterRegistryError, ChunkDomainAdapterRegistry,
};
pub use runner::{
    AggregateTokenUsage, LogicalChunkDispatch, LogicalChunkDispatchRequest, LogicalChunkError,
    LogicalChunkErrorContext, LogicalChunkExecutionContext, LogicalChunkMetadata,
    LogicalChunkRunner, LogicalChunkTelemetryEvent, LogicalChunkTelemetrySink, PhysicalChunkStage,
    TracingLogicalChunkTelemetry,
};
