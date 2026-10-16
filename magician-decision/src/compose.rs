//! Composition helpers: threshold gates and shadow comparison.
//!
//! Composition stays in code — the pack gives typed answers, this module
//! gives the small vocabulary for acting on them. Thresholds are always
//! supplied by the host, keyed per model id; nothing here encodes a number.

use serde::{Deserialize, Serialize};

use crate::primitives::QuestionId;
use crate::request::{Answer, DecisionResponse};

/// Noul is at or above a threshold.
pub fn noul_at_least(response: &DecisionResponse, id: &QuestionId, threshold: f64) -> Option<bool> {
    response
        .answer(id)
        .and_then(Answer::noul_value)
        .map(|v| v >= threshold)
}

/// Noul is at or below a threshold (interrupt-style questions where the
/// interesting direction is down).
pub fn noul_at_most(response: &DecisionResponse, id: &QuestionId, threshold: f64) -> Option<bool> {
    response
        .answer(id)
        .and_then(Answer::noul_value)
        .map(|v| v <= threshold)
}

/// Choice confidence is at or above a threshold.
pub fn choice_confidence_at_least(
    response: &DecisionResponse,
    id: &QuestionId,
    threshold: f64,
) -> Option<bool> {
    response
        .answer(id)
        .and_then(Answer::choice_confidence)
        .map(|c| c >= threshold)
}

/// Content-free record of one question where a shadow model disagreed with
/// the primary. Never carries state, bodies, or page text — question id,
/// tokens, and numbers only, so the ring can be exposed without a privacy
/// review (same contract as the plan's shadow lanes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowComparison {
    pub question: QuestionId,
    pub primary_token: Option<String>,
    pub shadow_token: Option<String>,
    pub primary_confidence: Option<f64>,
    pub shadow_confidence: Option<f64>,
}

impl ShadowComparison {
    /// Token of an answer for mismatch purposes: the chosen option id, the
    /// noul bucketed to the same granularity composition acts on, or the
    /// score rounded to the rubric level.
    pub fn token_of(answer: Option<&Answer>) -> Option<String> {
        match answer? {
            Answer::Choice { choice, .. } => Some(choice.as_str().to_string()),
            Answer::Noul { noul } => Some(format!("{noul:.2}")),
            Answer::Score { score, .. } => Some(format!("{score:.1}")),
        }
    }

    /// Compare two responses question by question and return the records
    /// where the tokens differ. A question answered by only one side is a
    /// mismatch on that side's token.
    pub fn compare(
        operation: &str,
        primary: &DecisionResponse,
        shadow: &DecisionResponse,
    ) -> Vec<ShadowComparison> {
        let _ = operation;
        let mut mismatches = Vec::new();
        let mut ids: Vec<&QuestionId> = primary.answers.keys().collect();
        for id in shadow.answers.keys() {
            if !primary.answers.contains_key(id) {
                ids.push(id);
            }
        }
        for id in ids {
            let (p, s) = (primary.answers.get(id), shadow.answers.get(id));
            let (p_token, s_token) = (Self::token_of(p), Self::token_of(s));
            if p_token != s_token {
                mismatches.push(ShadowComparison {
                    question: id.clone(),
                    primary_token: p_token,
                    shadow_token: s_token,
                    primary_confidence: p.and_then(Answer::choice_confidence),
                    shadow_confidence: s.and_then(Answer::choice_confidence),
                });
            }
        }
        mismatches
    }
}
