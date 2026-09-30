//! Strategy-based exploration system for MagicianV2
//!
//! This module implements adaptive query exploration with two strategies.
//!
//! ## Strategy Architecture:
//! - **AtomicComposition**: Default primary strategy using reasoning LLM.
//! - **Guided Search**: Fallback strategy with complexity-aware parameters.
//!
//! ## Key Features:
//! - Heuristic strategy selection (no LLM calls for selection)
//! - Category-guided tool space reduction (800 → 15 tools)
//! - Resource budget tracking and enforcement
//! - Adaptive failure handling with strategy fallback
//! - workflow_steps integration from query analysis

pub mod adaptive_selector;
pub mod atomic_composition;
pub mod atomic_filter;
pub mod decomposer;
pub mod early_termination;
pub mod entity_mapper;
pub mod error;
pub mod guided_search;
pub mod history;
pub mod parameter_utils;
pub mod plan;
pub mod plan_builder;
pub mod plan_validator;
pub mod tool_match_helper;
pub mod traits;
pub mod types;

// Re-export main types for easier access
pub use adaptive_selector::AdaptiveStrategySelector;
pub use atomic_composition::AtomicCompositionStrategy;
pub use decomposer::*;
pub use entity_mapper::{EntityMapper, MappingDetail, ParameterMappingResult, TypeConversion};
pub use error::StrategyError;
pub use guided_search::{GuidedSearchParams, GuidedSearchStrategy};
pub use plan::*;
pub use plan_builder::*;
pub use plan_validator::*;
pub use traits::*;
pub use types::*;
