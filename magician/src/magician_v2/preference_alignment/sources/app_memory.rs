//! Governed app memory destinations are not an alignment source in v1.
//!
//! The only read surface over a destination
//! (`AgentMemoryService::app_memory_contribution_state`) is a bounded,
//! owner-facing projection: it truncates to a row limit and a byte ceiling
//! and compacts terminal rows, so it cannot prove that every eligible entry
//! was read. App-governed entries that reach `knowledge.json` carry
//! `app_source_eligibility`, whose source eligibility must be re-resolved
//! before use; the native adapter lists those as omitted and marks its
//! coverage partial rather than presenting them unchecked.
//!
//! This source therefore contributes nothing and no `app_generation`. Making
//! app memory a source needs a complete, unbounded read of live destination
//! entries that returns `destination_generation` with them.

use super::SourceCollection;

/// What app memory contributes today: nothing, completely.
pub fn app_memory_intentions() -> (SourceCollection, Option<u64>) {
    (SourceCollection::empty_complete(), None)
}
