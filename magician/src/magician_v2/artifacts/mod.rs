//! # Artifact Lifecycle Control Plane
//!
//! Governs lifecycle decisions across existing artifact stores without forcing
//! storage unification. The control plane introduces:
//!
//! - **Artifact Catalog**: Logical registry of artifact metadata across domains.
//! - **Policy Engine**: Evaluates staleness, expiration, retention, and exposure.
//! - **Sanitization Gateway**: Generates consumer-specific projections.
//! - **Lifecycle Scheduler**: Runs mark-and-sweep cleanup with safety guards.
//! - **Domain Adapters**: Translate domain records into canonical catalog metadata.
//! - **Lifecycle Service**: Facade wrapping all components into one injectable service.
//!
//! Existing stores remain source-of-truth for payload bytes/content.
//! The catalog is source-of-truth for lifecycle control decisions.

pub mod adapters;
pub mod bridge;
pub mod catalog;
#[cfg(test)]
mod characterization;
pub mod durable_store;
mod guard;
pub use guard::durable_artifacts_source_guard;
pub mod migration;
pub mod object_backend;
pub mod policy;
pub mod sanitization;
pub mod scheduler;
pub mod service;
#[cfg(test)]
mod storage_packet;
pub mod types;
