//! Slot-graph kernel: the persisted slot vocabulary and the enrichment
//! pipeline. Extraction, elicitation, rewriting, and the router adapters stay
//! in `magician_v2` until their telemetry/clarifier/state-tracker couplings
//! are inverted; `magician_v2::slot_graph` re-exports everything here at the
//! historical paths.

pub mod enrichment;
pub mod types;

pub use enrichment::{
    EnrichmentContext, EnrichmentError, EnrichmentOutcome, EnrichmentPipeline, EnrichmentSummary,
    SlotEnricher,
};
pub use types::{ProvenanceRecord, ProvenanceSource, ProvisionalSlot, SlotRecord, SlotType};
