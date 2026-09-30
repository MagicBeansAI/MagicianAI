//! Phase 3 evaluation interpreter scaffolding.

use std::collections::HashSet;

use serde_json::Value;

use super::types::{CountConstraint, EvaluationCriterion};

#[derive(Debug, Clone, Default)]
pub struct EvaluationInput {
    pub structured_output: Option<Value>,
    pub had_error: bool,
    pub actions_taken: u32,
    pub updated_memory_keys: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationResult {
    Succeeded,
    Failed { reason: String },
    Inconclusive,
}

impl EvaluationResult {
    pub fn is_succeeded(&self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EvaluationInterpreter;

impl EvaluationInterpreter {
    pub fn evaluate(
        &self,
        criteria: &[EvaluationCriterion],
        input: &EvaluationInput,
    ) -> EvaluationResult {
        if criteria.is_empty() {
            return EvaluationResult::Inconclusive;
        }

        for criterion in criteria {
            match criterion {
                EvaluationCriterion::StructuredOutput {
                    required_fields,
                    min_count,
                } => {
                    let Some(obj) = input.structured_output.as_ref().and_then(Value::as_object)
                    else {
                        return EvaluationResult::Failed {
                            reason: "structured output missing object payload".to_string(),
                        };
                    };

                    for field in required_fields {
                        if !obj.contains_key(field) {
                            return EvaluationResult::Failed {
                                reason: format!("required field `{field}` missing"),
                            };
                        }
                    }

                    if let Some(min) = min_count {
                        if !satisfies_count_constraint(obj, min) {
                            return EvaluationResult::Failed {
                                reason: format!(
                                    "count constraint `{}` must be >= {}",
                                    min.field, min.value
                                ),
                            };
                        }
                    }
                },
                EvaluationCriterion::NoError => {
                    if input.had_error {
                        return EvaluationResult::Failed {
                            reason: "execution reported an error".to_string(),
                        };
                    }
                },
                EvaluationCriterion::UnderBudget { max_actions } => {
                    if input.actions_taken > *max_actions {
                        return EvaluationResult::Failed {
                            reason: format!(
                                "actions_taken={} exceeds budget {}",
                                input.actions_taken, max_actions
                            ),
                        };
                    }
                },
                EvaluationCriterion::MemoryUpdated { key } => {
                    if !input.updated_memory_keys.contains(key) {
                        return EvaluationResult::Failed {
                            reason: format!("memory key `{key}` was not updated"),
                        };
                    }
                },
            }
        }

        EvaluationResult::Succeeded
    }
}

fn satisfies_count_constraint(
    obj: &serde_json::Map<String, Value>,
    constraint: &CountConstraint,
) -> bool {
    let Some(value) = obj.get(&constraint.field) else {
        return false;
    };

    match value {
        Value::Array(items) => items.len() >= constraint.value,
        Value::Number(n) => n
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .is_some_and(|v| v >= constraint.value),
        _ => false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::EvaluationCriterion;
    use serde_json::json;

    #[test]
    fn evaluate_returns_inconclusive_when_no_criteria() {
        let result = EvaluationInterpreter.evaluate(&[], &EvaluationInput::default());
        assert_eq!(result, EvaluationResult::Inconclusive);
    }

    #[test]
    fn evaluate_structured_output_and_no_error_success() {
        let criteria = vec![
            EvaluationCriterion::StructuredOutput {
                required_fields: vec!["listings".to_string()],
                min_count: None,
            },
            EvaluationCriterion::NoError,
        ];
        let input = EvaluationInput {
            structured_output: Some(json!({ "listings": [1, 2] })),
            had_error: false,
            ..EvaluationInput::default()
        };
        assert_eq!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Succeeded
        );
    }

    #[test]
    fn evaluate_memory_updated_failure() {
        let criteria = vec![EvaluationCriterion::MemoryUpdated {
            key: "task_progress".to_string(),
        }];
        let input = EvaluationInput::default();
        assert_eq!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Failed {
                reason: "memory key `task_progress` was not updated".to_string(),
            }
        );
    }

    #[test]
    fn evaluate_memory_updated_success() {
        let criteria = vec![EvaluationCriterion::MemoryUpdated {
            key: "task_progress".to_string(),
        }];
        let input = EvaluationInput {
            updated_memory_keys: HashSet::from(["task_progress".to_string()]),
            ..EvaluationInput::default()
        };
        assert_eq!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Succeeded
        );
    }

    #[test]
    fn evaluate_no_error_fails_when_error_present() {
        let criteria = vec![EvaluationCriterion::NoError];
        let input = EvaluationInput {
            had_error: true,
            ..EvaluationInput::default()
        };
        let result = EvaluationInterpreter.evaluate(&criteria, &input);
        assert_eq!(
            result,
            EvaluationResult::Failed {
                reason: "execution reported an error".to_string(),
            }
        );
    }

    #[test]
    fn evaluate_min_count_array_input() {
        let criteria = vec![EvaluationCriterion::StructuredOutput {
            required_fields: vec!["listings".to_string()],
            min_count: Some(CountConstraint {
                field: "listings".to_string(),
                value: 3,
            }),
        }];

        // Exactly 3 items — passes.
        let input = EvaluationInput {
            structured_output: Some(json!({ "listings": [1, 2, 3] })),
            ..EvaluationInput::default()
        };
        assert_eq!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Succeeded
        );

        // Only 2 items — fails.
        let input = EvaluationInput {
            structured_output: Some(json!({ "listings": [1, 2] })),
            ..EvaluationInput::default()
        };
        assert!(matches!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Failed { .. }
        ));
    }

    #[test]
    fn evaluate_min_count_numeric_input() {
        let criteria = vec![EvaluationCriterion::StructuredOutput {
            required_fields: vec!["total".to_string()],
            min_count: Some(CountConstraint {
                field: "total".to_string(),
                value: 5,
            }),
        }];

        // Numeric value 5 satisfies min_count >= 5.
        let input = EvaluationInput {
            structured_output: Some(json!({ "total": 5 })),
            ..EvaluationInput::default()
        };
        assert_eq!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Succeeded
        );

        // Numeric value 4 does not satisfy.
        let input = EvaluationInput {
            structured_output: Some(json!({ "total": 4 })),
            ..EvaluationInput::default()
        };
        assert!(matches!(
            EvaluationInterpreter.evaluate(&criteria, &input),
            EvaluationResult::Failed { .. }
        ));
    }
}
