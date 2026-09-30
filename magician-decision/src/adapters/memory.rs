//! Scripted in-memory adapter — the test double and the first proof the
//! trait round-trips.
//!
//! It answers every question of a request from a preloaded answer map,
//! echoing an error for questions it has no scripted answer for, and
//! records the last request so tests can assert on what the host built.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::primitives::QuestionId;
use crate::request::{Answer, DecisionRequest, DecisionResponse, Usage};

pub struct MemoryDecisionModel {
    identity: ModelIdentity,
    capabilities: ModelCapabilities,
    answers: BTreeMap<QuestionId, Answer>,
    last_request: Mutex<Option<DecisionRequest>>,
}

impl MemoryDecisionModel {
    pub fn new(adapter: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            identity: ModelIdentity::new(adapter, model),
            capabilities: ModelCapabilities::default(),
            answers: BTreeMap::new(),
            last_request: Mutex::new(None),
        }
    }

    pub fn with_answer(mut self, question: impl Into<String>, answer: Answer) -> Self {
        self.answers.insert(QuestionId::new(question), answer);
        self
    }

    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn set_calibrated(mut self, calibrated: bool) -> Self {
        self.capabilities.calibrated = calibrated;
        self
    }

    pub fn last_request(&self) -> Option<DecisionRequest> {
        self.last_request.lock().ok().and_then(|g| g.clone())
    }
}

#[async_trait]
impl StructuredDecisionModel for MemoryDecisionModel {
    fn identity(&self) -> ModelIdentity {
        self.identity.clone()
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let mut answers = BTreeMap::new();
        for question in &request.questions {
            let answer = self.answers.get(question.id()).cloned().ok_or_else(|| {
                DecisionError::InvalidResponse(format!(
                    "memory model has no scripted answer for '{}'",
                    question.id().as_str()
                ))
            })?;
            answers.insert(question.id().clone(), answer);
        }
        if let Ok(mut guard) = self.last_request.lock() {
            *guard = Some(request.clone());
        }
        Ok(DecisionResponse {
            model: self.identity.clone(),
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            answers,
            usage: Usage {
                input_tokens: 64,
                output_tokens: 0,
            },
        })
    }
}
