//! Runtime action risk support.
//!
//! Browser action selection and execution now live in the visible agent loop and
//! pack/direct-action dispatch. This module only retains the shared criticality
//! types still consumed by confirmation gating.

pub mod criticality;

pub use criticality::{CriticalityEvaluator, CriticalityLevel};
