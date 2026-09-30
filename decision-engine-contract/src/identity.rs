//! Which model answered.

use serde::{Deserialize, Serialize};

/// Adapter family + pinned model id, echoed by the model on every response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    /// e.g. `typesafe`, `systemone`, `generative`, `memory`.
    pub adapter: String,
    /// The exact model string the adapter dispatched to, e.g. `jev-1.13.0`.
    pub model: String,
}

impl ModelIdentity {
    pub fn new(adapter: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
            model: model.into(),
        }
    }
}
