//! Test utilities for magician_v2 module.
//!
//! This module provides shared mock implementations and test helpers
//! to avoid duplication across test modules.

pub mod mock_llm;

pub use mock_llm::{
    ConfigurableMockLlm, MockLlmPreset, CONTENT_PAGE_RESPONSE, LOGIN_PAGE_RESPONSE,
};
