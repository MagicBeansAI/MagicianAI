//! Question primitives: the vendor-neutral Choice / Score / Noul IR.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Identifier of a question inside a pack. Ours, not the vendor's: adapters
/// must not rely on it being forwarded as a model-facing hint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct QuestionId(pub String);

impl QuestionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QuestionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One declared option of a Choice question.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OptionId(pub String);

impl OptionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
    Choice,
    Score,
    Noul,
}

/// Instructions for a question. `Structured` exists so a pack can carry
/// objects/arrays where a vendor dialect accepts them; adapters may flatten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Instruction {
    Text(String),
    Structured(serde_json::Value),
}

impl Instruction {
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Structured(value) => value.to_string(),
        }
    }
}

/// Contrastive option criteria: what the option IS for, what it is NOT for,
/// and short examples. Contrast is what keeps a decision model from
/// collapsing two adjacent options; a bare label invites it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Criteria {
    /// Plain description — acceptable for self-evident options.
    Str(String),
    /// Contrastive form (`what` / `not_for` / `examples`).
    Contrastive {
        what: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_for: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        examples: Vec<String>,
    },
}

impl Criteria {
    pub fn what(&self) -> &str {
        match self {
            Self::Str(text) => text,
            Self::Contrastive { what, .. } => what,
        }
    }
}

/// Criteria for the two sides of a Noul.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    /// What makes the answer true.
    pub is_true: String,
    /// What looks similar but is NOT true — the contrastive guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_false: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceQuestion {
    pub id: QuestionId,
    pub instructions: Instruction,
    /// Contrastive criteria per option. Options are exactly these; a model
    /// answer outside the map is an error, not a coercion target.
    pub criteria: BTreeMap<OptionId, Criteria>,
}

impl ChoiceQuestion {
    pub fn option_ids(&self) -> Vec<OptionId> {
        self.criteria.keys().cloned().collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreQuestion {
    pub id: QuestionId,
    pub instructions: Instruction,
    /// Ordered rubric levels, weakest first. At least two.
    pub levels: Vec<Criteria>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulQuestion {
    pub id: QuestionId,
    pub instructions: Instruction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<NoulCriteria>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Question {
    Choice(ChoiceQuestion),
    Score(ScoreQuestion),
    Noul(NoulQuestion),
}

impl Question {
    pub fn id(&self) -> &QuestionId {
        match self {
            Self::Choice(q) => &q.id,
            Self::Score(q) => &q.id,
            Self::Noul(q) => &q.id,
        }
    }

    pub fn kind(&self) -> PrimitiveKind {
        match self {
            Self::Choice(_) => PrimitiveKind::Choice,
            Self::Score(_) => PrimitiveKind::Score,
            Self::Noul(_) => PrimitiveKind::Noul,
        }
    }
}
