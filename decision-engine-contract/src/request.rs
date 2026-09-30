//! Decision requests and typed answers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::DecisionError;
use crate::identity::ModelIdentity;
use crate::primitives::{OptionId, Question, QuestionId};

/// The context a decision is made against: a string, object, or array of
/// text. Kept as raw JSON so hosts build it with their own sanitizer
/// upstream; this crate never invents state content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionState(pub serde_json::Value);

impl DecisionState {
    pub fn from_json(value: serde_json::Value) -> Self {
        Self(value)
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self(serde_json::Value::String(text.into()))
    }

    pub fn as_json(&self) -> &serde_json::Value {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// Operation the host is running, e.g. `tool_action_judge`.
    pub operation: String,
    pub pack_id: String,
    pub pack_version: String,
    pub state: DecisionState,
    /// Ordered, unique by id. Built from a pack; the host may instantiate
    /// dynamic Choice options (see [`set_choice_candidates`]) before
    /// dispatch.
    pub questions: Vec<Question>,
}

impl DecisionRequest {
    pub fn question(&self, id: &QuestionId) -> Option<&Question> {
        self.questions.iter().find(|q| q.id() == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    /// The adapter + exact model id that answered. Pin checks and threshold
    /// ownership key off this, never off the request's intent.
    pub model: ModelIdentity,
    pub pack_id: String,
    pub pack_version: String,
    pub answers: BTreeMap<QuestionId, Answer>,
    pub usage: Usage,
}

impl DecisionResponse {
    pub fn answer(&self, id: &QuestionId) -> Option<&Answer> {
        self.answers.get(id)
    }
}

/// A typed answer. Every variant keeps the distribution, not just the argmax:
/// composition gates on confidence and probability mass, and shadow compare
/// logs both sides' tokens with their numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        choice: OptionId,
        probabilities: BTreeMap<OptionId, f64>,
        confidence: f64,
    },
    Score {
        /// Probability-weighted mean over the rubric levels. Use for
        /// thresholds and ranking only — never interpolate a 1.4 as "40%
        /// between levels".
        score: f64,
        /// Probability per rubric level, indexed weakest-first. Dense on
        /// purpose: JSON object keys are strings, so an integer-keyed map
        /// cannot round-trip the wire; a Vec keeps the level order the
        /// only ordering that exists.
        probabilities: Vec<f64>,
        confidence: f64,
    },
    /// P(true) in [0, 1]. No separate confidence by design: distance from
    /// 0.5 is the uncertainty signal.
    Noul { noul: f64 },
}

impl Answer {
    pub fn noul_value(&self) -> Option<f64> {
        match self {
            Self::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    pub fn choice_confidence(&self) -> Option<f64> {
        match self {
            Self::Choice { confidence, .. } => Some(*confidence),
            Self::Score { confidence, .. } => Some(*confidence),
            Self::Noul { .. } => None,
        }
    }

    pub fn choice_option(&self) -> Option<&OptionId> {
        match self {
            Self::Choice { choice, .. } => Some(choice),
            _ => None,
        }
    }
}

/// Validate that every answer matches its question's declared options and
/// primitive kind. Called by adapters on the way in; the runtime re-checks
/// on the way out so no adapter bug can smuggle an undeclared label into
/// composition.
pub fn validate_answers(
    request: &DecisionRequest,
    response: &DecisionResponse,
) -> Result<(), DecisionError> {
    if response.pack_id != request.pack_id || response.pack_version != request.pack_version {
        return Err(DecisionError::InvalidResponse(
            "answer pack identity mismatch".into(),
        ));
    }
    if response
        .answers
        .keys()
        .any(|id| request.question(id).is_none())
    {
        return Err(DecisionError::InvalidResponse(
            "undeclared answer question".into(),
        ));
    }
    for question in &request.questions {
        let answer = response.answers.get(question.id()).ok_or_else(|| {
            DecisionError::InvalidResponse(format!(
                "missing answer for question '{}'",
                question.id().as_str()
            ))
        })?;
        let probability = |value: &f64| value.is_finite() && (0.0..=1.0).contains(value);
        let numeric_valid = match answer {
            Answer::Noul { noul } => probability(noul),
            Answer::Choice {
                confidence,
                probabilities,
                ..
            } => probability(confidence) && probabilities.values().all(probability),
            Answer::Score {
                score,
                confidence,
                probabilities,
            } => {
                score.is_finite()
                    && probability(confidence)
                    && probabilities.iter().all(probability)
            },
        };
        if !numeric_valid {
            return Err(DecisionError::InvalidResponse(
                "answer has invalid numeric values".into(),
            ));
        }
        match (question, answer) {
            (
                Question::Choice(q),
                Answer::Choice {
                    choice,
                    probabilities,
                    ..
                },
            ) => {
                if !q.criteria.contains_key(choice) {
                    return Err(DecisionError::UnknownOption {
                        question: q.id.clone(),
                        option: choice.clone(),
                    });
                }
                for option in probabilities.keys() {
                    if !q.criteria.contains_key(option) {
                        return Err(DecisionError::UnknownOption {
                            question: q.id.clone(),
                            option: option.clone(),
                        });
                    }
                }
            },
            (
                Question::Score(q),
                Answer::Score {
                    score,
                    probabilities,
                    ..
                },
            ) => {
                if q.levels.len() < 2 || !(0.0..=(q.levels.len() - 1) as f64).contains(score) {
                    return Err(DecisionError::InvalidResponse(
                        "score outside declared rubric range".into(),
                    ));
                }
                if probabilities.len() != q.levels.len() {
                    return Err(DecisionError::InvalidResponse(format!(
                        "score question '{}' returned {} level probabilities but the rubric has {} levels",
                        q.id.as_str(),
                        probabilities.len(),
                        q.levels.len()
                    )));
                }
            },
            (Question::Noul(_), Answer::Noul { .. }) => {},
            _ => {
                return Err(DecisionError::InvalidResponse(format!(
                    "answer kind does not match question '{}' kind",
                    question.id().as_str()
                )));
            },
        }
    }
    Ok(())
}

/// Instantiate (or replace) a Choice question's options from per-request
/// candidates.
///
/// Packs declare Choice questions with static option enums — taxonomies,
/// lanes, verbs. Browser element selection is the exception: its options
/// are the page's interactive elements, which exist only at request time.
/// The pack therefore ships the question as a template whose criteria map
/// is empty, and the host injects the extracted candidates here. Each
/// option's criteria text is the candidate's own label and nothing else:
/// the question's instruction rides the request once as `instructions`,
/// and repeating it per option cost the browser step judge 2–4× its
/// state — run 13 (2026-09-21) billed 15.9K input tokens for a 3.2K-token
/// state, ~60 tokens of instruction × (88 elements + 60 values).
pub fn set_choice_candidates(
    request: &mut DecisionRequest,
    question_id: &QuestionId,
    candidates: &[(OptionId, String)],
) -> Result<(), DecisionError> {
    let question = request
        .questions
        .iter_mut()
        .find(|q| q.id() == question_id)
        .ok_or_else(|| {
            DecisionError::InvalidResponse(format!(
                "candidate injection: question '{}' not present in request",
                question_id.as_str()
            ))
        })?;
    let choice = match question {
        Question::Choice(choice) => choice,
        _ => {
            return Err(DecisionError::InvalidResponse(format!(
                "candidate injection: question '{}' is not a Choice",
                question_id.as_str()
            )))
        },
    };
    if candidates.is_empty() {
        return Err(DecisionError::InvalidResponse(format!(
            "candidate injection: question '{}' got an empty candidate set",
            question_id.as_str()
        )));
    }
    choice.criteria = candidates
        .iter()
        .map(|(option, label)| {
            (
                option.clone(),
                crate::primitives::Criteria::Str(label.clone()),
            )
        })
        .collect();
    Ok(())
}
