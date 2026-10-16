//! Source adapters: each canonical store that owns owner intentions projects
//! its eligible entries into `intention.v1` and says what it covered.
//!
//! An adapter only reads. It may lower the authority it reads (P0 contracts
//! §6.2) and it reports anything it could not read as coverage, never as an
//! empty answer. Eligibility is structural — owner scope, lifecycle, validity
//! window — and never topical: no adapter filters by what the pending act is
//! about.

pub mod app_memory;
pub mod native;
pub mod taste_profile;

use super::intention::{Coverage, Intention};

/// What one source contributed: its intentions and what it covered.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceCollection {
    pub intentions: Vec<Intention>,
    pub coverage: Coverage,
}

impl SourceCollection {
    pub fn empty_complete() -> Self {
        Self {
            intentions: Vec::new(),
            coverage: Coverage::complete(),
        }
    }

    /// A source that could not be read. Never empty-and-complete.
    pub fn unavailable(source: &str) -> Self {
        Self {
            intentions: Vec::new(),
            coverage: Coverage {
                status: super::intention::CoverageStatus::Unavailable,
                omitted_ids: Vec::new(),
                unavailable_sources: vec![source.to_string()],
            },
        }
    }
}
