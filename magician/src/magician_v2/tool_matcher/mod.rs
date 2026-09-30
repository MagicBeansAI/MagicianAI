//! V2 Tool Matcher - Compact Router + LLM Disambiguation
//!
//! This module implements a simplified matching pipeline:
//! - Route inference over a compact tool surface (`browser`, `files`, `search`, `shell`)
//! - Lightweight lexical/semantic scoring
//! - Optional LLM disambiguation on top candidates
//!
//! ## Security Architecture
//! - ALL tool access goes through ToolCatalog (security-filtered)
//! - NEVER accesses RegistryService directly
//! - Security filtering happens BEFORE routing/scoring
//!
//! ## Expected Performance
//! - Very low overhead for local routing
//! - Bounded LLM calls (top-4 candidates)
//! - Deterministic fallback when LLM is unavailable

pub mod config;
pub mod types;

pub mod llm_evaluator;
pub mod service;

// Re-export types and service
pub use config::ToolMatcherConfig;
pub use service::V2ToolMatcher;
pub use types::{
    MatchingTier, TierScores, ToolCandidate, ToolMatchError, ToolMatchRequest, ToolMatchResult,
    ToolMetadata, ToolParameter,
};
