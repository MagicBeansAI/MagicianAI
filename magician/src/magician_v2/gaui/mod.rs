pub mod muij;

// Delta emitter (WebSocket broadcasting of MuijDelta events) — GA-B04.
pub mod emitter;

// JSONPath query engine for data extraction — GB-B01.
pub mod query;

// Shared snapshot materialization path (REST + WS parity).
pub mod snapshot;

// Event coalescing layer for multi-agent fan-out — GB-B02.
pub mod coalesce;

pub use coalesce::{CoalescedBatch, CoalescerInput, MuijCoalescer};
pub use emitter::MuijDocumentCache;
pub use muij::{
    DefaultComponentRegistry, MuijComponent, MuijDelta, MuijDocument, MuijGraphEdge,
    MuijGraphLayout, MuijGraphMetaValue, MuijGraphNode, MuijGraphSpec, MuijStorage,
    MuijStorageError, MuijValidationError,
};
pub use query::{materialize_component_queries, MuijQueryEngine};
pub use snapshot::{
    agent_snapshot_cache_key, load_materialized_snapshot_document,
    load_materialized_surface_snapshot_document, SnapshotLoadError,
};
