//! The adapter trait every decision model implements.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use decision_engine_contract::identity::ModelIdentity;

use crate::error::DecisionError;
use crate::request::{DecisionRequest, DecisionResponse};

/// What an adapter can honestly promise. `calibrated` is true only for
/// adapters whose probabilities are trained-verified (TypeSafe Jev today);
/// composition must not treat an uncalibrated `confidence` as a calibrated
/// one, and thresholds are keyed per model id for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub calibrated: bool,
    pub text_only: bool,
    pub max_state_tokens: Option<u64>,
    /// Largest Choice option set the model answers well (Jev: 255; laya
    /// degrades past ~77). `None` = undeclared.
    pub max_choice_options: Option<usize>,
    /// Maximum number of heads in a single physical request. A shared chunk
    /// checks this bound independently before attempting each routed model.
    #[serde(default)]
    pub max_questions: Option<usize>,
    /// Whether Score questions are within the model's competence.
    pub supports_score: bool,
    /// Whether requests leave the box. Body-seeing operations refuse
    /// remote models in local mode.
    pub remote: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            calibrated: false,
            text_only: true,
            max_state_tokens: None,
            max_choice_options: None,
            max_questions: None,
            supports_score: true,
            // Unknown locality is treated as remote: fail closed.
            remote: true,
        }
    }
}

#[async_trait]
pub trait StructuredDecisionModel: Send + Sync {
    /// HTTP adapters instrument each retry themselves; other adapters are
    /// instrumented once by the runtime so future models cannot be omitted.
    fn records_attempts(&self) -> bool {
        false
    }
    fn identity(&self) -> ModelIdentity;

    fn capabilities(&self) -> ModelCapabilities;

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError>;
}
