//! Pure value preparation for reviewed contextual rounds.
//!
//! This flat, bounded expression table cannot read a store, dispatch a tool or
//! call a model. The native round owner supplies already-authorized values and
//! sends planned mutations through its existing App mutation owner. Model
//! output cannot choose another entity, query, capability or program.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_EXPRESSIONS: usize = 1024;
const MAX_EVALUATED_BYTES: usize = 8 * 1024 * 1024;
const MAX_LITERAL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppRoundValueId(pub u16);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppRoundValueSource {
    Input,
    Participant,
    Context,
    Model,
    Item,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRoundValueExpression {
    Literal {
        value: Value,
    },
    Read {
        source: AppRoundValueSource,
        pointer: String,
    },
    Timestamp,
    RunId,
    ParticipantId,
    Object {
        fields: BTreeMap<String, AppRoundValueId>,
    },
    Array {
        values: Vec<AppRoundValueId>,
    },
    Join {
        value: AppRoundValueId,
        separator: String,
    },
    ByteLength {
        value: AppRoundValueId,
    },
    LinkedTextRows {
        value: AppRoundValueId,
        schema: super::linked_text_rows::AppLinkedTextRowsSchema,
    },
    CanonicalJson {
        value: AppRoundValueId,
    },
    Split {
        value: AppRoundValueId,
        separator: String,
    },
    Slice {
        value: AppRoundValueId,
        limit: AppRoundValueId,
    },
    Project {
        rows: AppRoundValueId,
        pointer: String,
        #[serde(default)]
        skip_missing: bool,
    },
    Coalesce {
        values: Vec<AppRoundValueId>,
    },
    Equal {
        left: AppRoundValueId,
        right: AppRoundValueId,
    },
    LessThan {
        left: AppRoundValueId,
        right: AppRoundValueId,
    },
    All {
        values: Vec<AppRoundValueId>,
    },
    Any {
        values: Vec<AppRoundValueId>,
    },
    Not {
        value: AppRoundValueId,
    },
    Present {
        value: AppRoundValueId,
    },
    If {
        condition: AppRoundValueId,
        then_value: AppRoundValueId,
        else_value: AppRoundValueId,
    },
    Add {
        left: AppRoundValueId,
        right: AppRoundValueId,
    },
    Subtract {
        left: AppRoundValueId,
        right: AppRoundValueId,
    },
    Multiply {
        left: AppRoundValueId,
        right: AppRoundValueId,
    },
    Clamp {
        value: AppRoundValueId,
        minimum: AppRoundValueId,
        maximum: AppRoundValueId,
    },
    Length {
        value: AppRoundValueId,
    },
    Trim {
        value: AppRoundValueId,
    },
    Truncate {
        value: AppRoundValueId,
        max_chars: AppRoundValueId,
    },
    Contains {
        value: AppRoundValueId,
        member: AppRoundValueId,
    },
    ElapsedSeconds {
        timestamp: AppRoundValueId,
    },
    Lookup {
        rows: AppRoundValueId,
        key_pointer: String,
        key: AppRoundValueId,
        value_pointer: String,
    },
    StableId {
        prefix: String,
        parts: Vec<AppRoundValueId>,
    },
}

impl AppRoundValueExpression {
    fn references(&self) -> Vec<AppRoundValueId> {
        use AppRoundValueExpression::*;
        match self {
            Literal { .. } | Read { .. } | Timestamp | RunId | ParticipantId => vec![],
            Object { fields } => fields.values().copied().collect(),
            Array { values } => values.clone(),
            CanonicalJson { value }
            | Split { value, .. }
            | Join { value, .. }
            | ByteLength { value }
            | LinkedTextRows { value, .. } => vec![*value],
            Slice { value, limit } => vec![*value, *limit],
            Project { rows, .. } => vec![*rows],
            Coalesce { values } | All { values } | Any { values } => values.clone(),
            Equal { left, right }
            | LessThan { left, right }
            | Add { left, right }
            | Subtract { left, right }
            | Multiply { left, right } => vec![*left, *right],
            Not { value } | Present { value } | Length { value } | Trim { value } => vec![*value],
            If {
                condition,
                then_value,
                else_value,
            } => vec![*condition, *then_value, *else_value],
            Clamp {
                value,
                minimum,
                maximum,
            } => vec![*value, *minimum, *maximum],
            Truncate { value, max_chars } => vec![*value, *max_chars],
            Contains { value, member } => vec![*value, *member],
            ElapsedSeconds { timestamp } => vec![*timestamp],
            Lookup { rows, key, .. } => vec![*rows, *key],
            StableId { parts, .. } => parts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AppRoundValueExpression::*;

    fn id(index: u16) -> AppRoundValueId {
        AppRoundValueId(index)
    }
    fn run(
        program: &AppRoundValueProgram,
        context: &Value,
        model: Option<&Value>,
        participant_id: &str,
        run_id: &str,
    ) -> Result<AppRoundPreparedValues, AppRoundProgramError> {
        program.evaluate(&AppRoundValueEnvironment {
            input: &Value::Null,
            participant: &Value::Null,
            context,
            model,
            item: None,
            now: DateTime::parse_from_rfc3339("2026-09-09T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            participant_id,
            run_id,
        })
    }

    #[test]
    fn missing_policy_cannot_turn_into_affirmative_eligibility() {
        let program = AppRoundValueProgram {
            expressions: vec![
                Read {
                    source: AppRoundValueSource::Context,
                    pointer: "/policy/enabled".into(),
                },
                Literal { value: json!(true) },
                Equal {
                    left: id(0),
                    right: id(1),
                },
                All {
                    values: vec![id(1), id(2)],
                },
                Not { value: id(3) },
            ],
        };
        program.require_pre_model([id(3)]).unwrap();
        let missing = run(&program, &json!({}), None, "a", "round").unwrap();
        assert!(!missing.condition(id(3)));
        assert!(
            !missing.condition(id(4)),
            "unknown must remain unknown through negation"
        );
        assert!(run(
            &program,
            &json!({"policy":{"enabled":true}}),
            None,
            "a",
            "round"
        )
        .unwrap()
        .condition(id(3)));
        assert!(!run(
            &program,
            &json!({"policy":{"enabled":false}}),
            None,
            "a",
            "round"
        )
        .unwrap()
        .condition(id(3)));
    }

    #[test]
    fn preparation_refuses_forward_cycles_and_hidden_model_dependencies() {
        let invalid = AppRoundValueProgram {
            expressions: vec![Not { value: id(0) }],
        };
        assert_eq!(
            invalid.validate(),
            Err(AppRoundProgramError::InvalidProgram)
        );
        let program = AppRoundValueProgram {
            expressions: vec![
                Literal { value: json!(true) },
                Read {
                    source: AppRoundValueSource::Model,
                    pointer: "/draft".into(),
                },
                If {
                    condition: id(0),
                    then_value: id(0),
                    else_value: id(1),
                },
            ],
        };
        assert_eq!(
            program.require_pre_model([id(2)]),
            Err(AppRoundProgramError::InvalidDependency)
        );
        assert_eq!(
            program.require_pre_model([id(3)]),
            Err(AppRoundProgramError::InvalidProgram)
        );
    }

    #[test]
    fn counters_keep_integer_precision_and_overflow_is_refused() {
        let program = AppRoundValueProgram {
            expressions: vec![
                Literal {
                    value: json!(9_007_199_254_740_993_u64),
                },
                Literal { value: json!(1) },
                Add {
                    left: id(0),
                    right: id(1),
                },
                LessThan {
                    left: id(0),
                    right: id(2),
                },
                Literal {
                    value: json!(u64::MAX - 1),
                },
                Add {
                    left: id(4),
                    right: id(1),
                },
            ],
        };
        let values = run(&program, &Value::Null, None, "a", "round").unwrap();
        assert_eq!(
            values.get(id(2)).unwrap(),
            &json!(9_007_199_254_740_994_u64)
        );
        assert!(values.condition(id(3)));
        assert_eq!(values.get(id(5)).unwrap(), &json!(u64::MAX));
        let mut overflow = program;
        overflow.expressions.push(Add {
            left: id(5),
            right: id(1),
        });
        assert!(matches!(
            run(&overflow, &Value::Null, None, "a", "round"),
            Err(AppRoundProgramError::InvalidValue)
        ));
    }

    #[test]
    fn stable_record_identity_is_replayable_and_separates_authors_and_rounds() {
        let program = AppRoundValueProgram {
            expressions: vec![
                RunId,
                ParticipantId,
                StableId {
                    prefix: "post".into(),
                    parts: vec![id(0), id(1)],
                },
            ],
        };
        let identity = |author, round| {
            run(&program, &Value::Null, None, author, round)
                .unwrap()
                .get(id(2))
                .unwrap()
                .clone()
        };
        assert_eq!(identity("a", "one"), identity("a", "one"));
        assert_ne!(identity("a", "one"), identity("b", "one"));
        assert_ne!(identity("a", "one"), identity("a", "two"));
    }

    #[test]
    fn unicode_body_and_cooldown_are_processed_without_a_model() {
        let program = AppRoundValueProgram {
            expressions: vec![
                Literal {
                    value: json!("  café 🌱 hello  "),
                },
                Trim { value: id(0) },
                Literal { value: json!(6) },
                Truncate {
                    value: id(1),
                    max_chars: id(2),
                },
                Literal {
                    value: json!("2026-09-09T14:59:00Z"),
                },
                ElapsedSeconds { timestamp: id(4) },
                Literal {
                    value: json!("2026-09-09T15:01:00Z"),
                },
                ElapsedSeconds { timestamp: id(6) },
            ],
        };
        let values = run(&program, &Value::Null, None, "a", "round").unwrap();
        assert_eq!(values.get(id(3)).unwrap(), &json!("café 🌱"));
        assert_eq!(values.get(id(5)).unwrap(), &json!(60));
        assert_eq!(values.get(id(7)).unwrap(), &json!(0));
    }

    #[test]
    fn context_lookup_refuses_ambiguous_source_identity() {
        let program = AppRoundValueProgram {
            expressions: vec![
                Read {
                    source: AppRoundValueSource::Context,
                    pointer: "/rows".into(),
                },
                ParticipantId,
                Lookup {
                    rows: id(0),
                    key_pointer: "/id".into(),
                    key: id(1),
                    value_pointer: "/name".into(),
                },
            ],
        };
        let values = run(
            &program,
            &json!({"rows":[{"id":"a","name":"Agent A"}]}),
            None,
            "a",
            "round",
        )
        .unwrap();
        assert_eq!(values.get(id(2)).unwrap(), &json!("Agent A"));
        assert!(matches!(
            run(
                &program,
                &json!({"rows":[{"id":"a","name":"Agent A"},{"id":"a","name":"Someone else"}]}),
                None,
                "a",
                "round"
            ),
            Err(AppRoundProgramError::InvalidValue)
        ));
    }

    #[test]
    fn flat_program_handles_deep_dependencies_and_enforces_retained_byte_budget() {
        let mut program = AppRoundValueProgram {
            expressions: vec![Literal { value: json!(true) }],
        };
        for index in 1..1000 {
            program.expressions.push(Not {
                value: id(index - 1),
            });
        }
        program.require_pre_model([id(999)]).unwrap();
        assert!(!run(&program, &Value::Null, None, "a", "round")
            .unwrap()
            .condition(id(999)));
        let oversized = AppRoundValueProgram {
            expressions: vec![Read {
                source: AppRoundValueSource::Context,
                pointer: "".into(),
            }],
        };
        assert!(matches!(
            run(
                &oversized,
                &json!("x".repeat(MAX_EVALUATED_BYTES)),
                None,
                "a",
                "round"
            ),
            Err(AppRoundProgramError::ByteBudget)
        ));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundValueProgram {
    pub expressions: Vec<AppRoundValueExpression>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AppRoundProgramError {
    #[error("invalid or oversized contextual round value program")]
    InvalidProgram,
    #[error("round preparation depends on unavailable model or iteration data")]
    InvalidDependency,
    #[error("required contextual round value is missing or has the wrong type")]
    InvalidValue,
    #[error("contextual round value preparation exceeds its byte budget")]
    ByteBudget,
}

pub struct AppRoundValueEnvironment<'a> {
    pub input: &'a Value,
    pub participant: &'a Value,
    pub context: &'a Value,
    pub model: Option<&'a Value>,
    pub item: Option<&'a Value>,
    pub now: DateTime<Utc>,
    pub run_id: &'a str,
    pub participant_id: &'a str,
}

/// Missing and explicit null stay distinct. In particular an absent policy
/// value cannot satisfy equality with a declared null or an affirmative gate.
pub struct AppRoundPreparedValues(Vec<Option<Value>>);

impl AppRoundPreparedValues {
    pub fn get(&self, id: AppRoundValueId) -> Result<&Value, AppRoundProgramError> {
        self.0
            .get(usize::from(id.0))
            .and_then(Option::as_ref)
            .ok_or(AppRoundProgramError::InvalidValue)
    }

    pub fn condition(&self, id: AppRoundValueId) -> bool {
        self.get(id).ok().and_then(Value::as_bool) == Some(true)
    }

    pub fn fields(
        &self,
        fields: &BTreeMap<String, AppRoundValueId>,
    ) -> Result<BTreeMap<String, Value>, AppRoundProgramError> {
        fields
            .iter()
            .map(|(name, id)| Ok((name.clone(), self.get(*id)?.clone())))
            .collect()
    }
}

fn pointer(value: &str) -> bool {
    value.len() <= 4096
        && (value.is_empty() || value.starts_with('/'))
        && !value
            .as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
        && !value.ends_with('~')
}

fn canonical_json(value: &Value, depth: usize) -> Result<String, AppRoundProgramError> {
    if depth > 64 {
        return Err(AppRoundProgramError::ByteBudget);
    }
    match value {
        Value::Object(fields) => {
            let sorted: BTreeMap<_, _> = fields.iter().collect();
            let members = sorted
                .into_iter()
                .map(|(name, value)| {
                    let name = serde_json::to_string(name)
                        .map_err(|_| AppRoundProgramError::InvalidValue)?;
                    Ok(format!("{name}:{}", canonical_json(value, depth + 1)?))
                })
                .collect::<Result<Vec<_>, AppRoundProgramError>>()?;
            Ok(format!("{{{}}}", members.join(",")))
        },
        Value::Array(rows) => {
            let rows = rows
                .iter()
                .map(|row| canonical_json(row, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", rows.join(",")))
        },
        _ => serde_json::to_string(value).map_err(|_| AppRoundProgramError::InvalidValue),
    }
}

fn integer(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn number_order(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        _ => left.as_f64()?.partial_cmp(&right.as_f64()?),
    }
}

impl AppRoundValueProgram {
    pub fn sources(
        &self,
        roots: impl IntoIterator<Item = AppRoundValueId>,
    ) -> Result<BTreeSet<AppRoundValueSource>, AppRoundProgramError> {
        self.validate()?;
        let mut pending: Vec<_> = roots.into_iter().collect();
        let mut visited = BTreeSet::new();
        let mut sources = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let expression = self
                .expressions
                .get(usize::from(id.0))
                .ok_or(AppRoundProgramError::InvalidProgram)?;
            match expression {
                AppRoundValueExpression::Read { source, .. } => {
                    sources.insert(*source);
                },
                AppRoundValueExpression::ParticipantId => {
                    sources.insert(AppRoundValueSource::Participant);
                },
                _ => {},
            }
            pending.extend(expression.references());
        }
        Ok(sources)
    }

    pub fn validate(&self) -> Result<(), AppRoundProgramError> {
        use AppRoundValueExpression::*;
        if self.expressions.is_empty() || self.expressions.len() > MAX_EXPRESSIONS {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        let mut literal_bytes = 0usize;
        for (index, expression) in self.expressions.iter().enumerate() {
            let refs = expression.references();
            if refs.len() > MAX_EXPRESSIONS || refs.iter().any(|id| usize::from(id.0) >= index) {
                return Err(AppRoundProgramError::InvalidProgram);
            }
            match expression {
                Literal { value } => {
                    literal_bytes = literal_bytes
                        .checked_add(
                            serde_json::to_vec(value)
                                .map_err(|_| AppRoundProgramError::InvalidProgram)?
                                .len(),
                        )
                        .filter(|bytes| *bytes <= MAX_LITERAL_BYTES)
                        .ok_or(AppRoundProgramError::InvalidProgram)?;
                },
                Read { pointer: path, .. } if !pointer(path) => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Project { pointer: path, .. } if !pointer(path) => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Object { fields }
                    if fields.len() > 256
                        || fields.keys().any(|key| {
                            key.is_empty() || key.len() > 256 || key.chars().any(char::is_control)
                        }) =>
                {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                LinkedTextRows { schema, .. } if !schema.valid_declaration() => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Join { separator, .. } if separator.len() > 64 => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Split { separator, .. } if separator.is_empty() || separator.len() > 64 => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Lookup {
                    key_pointer,
                    value_pointer,
                    ..
                } if !pointer(key_pointer) || !pointer(value_pointer) => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                StableId { prefix, parts }
                    if parts.is_empty()
                        || prefix.is_empty()
                        || prefix.len() > 64
                        || !prefix.bytes().all(|value| {
                            value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-')
                        }) =>
                {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                Coalesce { values } | All { values } | Any { values } if values.is_empty() => {
                    return Err(AppRoundProgramError::InvalidProgram)
                },
                _ => {},
            }
        }
        Ok(())
    }

    /// Used at package admission for query parameters and eligibility gates.
    /// The dependency walk is iterative and checks transitive references,
    /// including an unavailable branch hidden behind a constant condition.
    pub fn require_pre_model(
        &self,
        roots: impl IntoIterator<Item = AppRoundValueId>,
    ) -> Result<(), AppRoundProgramError> {
        self.validate()?;
        let mut pending: Vec<_> = roots.into_iter().collect();
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let expression = self
                .expressions
                .get(usize::from(id.0))
                .ok_or(AppRoundProgramError::InvalidProgram)?;
            if matches!(
                expression,
                AppRoundValueExpression::Read {
                    source: AppRoundValueSource::Model | AppRoundValueSource::Item,
                    ..
                }
            ) {
                return Err(AppRoundProgramError::InvalidDependency);
            }
            pending.extend(expression.references());
        }
        Ok(())
    }

    pub fn evaluate(
        &self,
        environment: &AppRoundValueEnvironment<'_>,
    ) -> Result<AppRoundPreparedValues, AppRoundProgramError> {
        self.validate()?;
        let mut values = Vec::with_capacity(self.expressions.len());
        let mut retained_bytes = 0usize;
        for expression in &self.expressions {
            let value = expression.evaluate(&values, environment)?;
            if let Some(value) = &value {
                retained_bytes = retained_bytes
                    .checked_add(
                        serde_json::to_vec(value)
                            .map_err(|_| AppRoundProgramError::InvalidValue)?
                            .len(),
                    )
                    .filter(|size| *size <= MAX_EVALUATED_BYTES)
                    .ok_or(AppRoundProgramError::ByteBudget)?;
            }
            values.push(value);
        }
        Ok(AppRoundPreparedValues(values))
    }
}

impl AppRoundValueExpression {
    fn evaluate(
        &self,
        values: &[Option<Value>],
        environment: &AppRoundValueEnvironment<'_>,
    ) -> Result<Option<Value>, AppRoundProgramError> {
        use AppRoundValueExpression::*;
        let get = |id: AppRoundValueId| values.get(usize::from(id.0)).and_then(Option::as_ref);
        let bool_at = |id| get(id).and_then(Value::as_bool);
        Ok(match self {
            Literal { value } => Some(value.clone()),
            Read { source, pointer } => {
                let source = match source {
                    AppRoundValueSource::Input => Some(environment.input),
                    AppRoundValueSource::Participant => Some(environment.participant),
                    AppRoundValueSource::Context => Some(environment.context),
                    AppRoundValueSource::Model => environment.model,
                    AppRoundValueSource::Item => environment.item,
                };
                source.and_then(|value| value.pointer(pointer)).cloned()
            },
            Timestamp => Some(json!(environment.now.to_rfc3339())),
            RunId => Some(json!(environment.run_id)),
            ParticipantId => Some(json!(environment.participant_id)),
            Object { fields } => {
                let fields: Option<serde_json::Map<String, Value>> = fields
                    .iter()
                    .map(|(name, id)| get(*id).map(|value| (name.clone(), value.clone())))
                    .collect();
                fields.map(Value::Object)
            },
            Array { values: ids } => ids
                .iter()
                .map(|id| get(*id).cloned())
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Join { value, separator } => get(*value)
                .and_then(Value::as_array)
                .and_then(|rows| rows.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .map(|rows| Value::String(rows.join(separator))),
            ByteLength { value } => get(*value)
                .and_then(Value::as_str)
                .map(|text| json!(text.len())),
            LinkedTextRows { value, schema } => get(*value)
                .and_then(Value::as_str)
                .map(|text| json!(schema.accepts(text))),
            CanonicalJson { value } => get(*value)
                .map(|value| canonical_json(value, 0).map(Value::String))
                .transpose()?,
            Split { value, separator } => get(*value).and_then(Value::as_str).map(|text| {
                json!(text
                    .split(separator)
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>())
            }),
            Slice { value, limit } => match (
                get(*value).and_then(Value::as_array),
                get(*limit).and_then(Value::as_u64),
            ) {
                (Some(rows), Some(limit)) => Some(Value::Array(
                    rows.iter()
                        .take(usize::try_from(limit).unwrap_or(usize::MAX))
                        .cloned()
                        .collect(),
                )),
                _ => None,
            },
            Project {
                rows,
                pointer,
                skip_missing,
            } => get(*rows)
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| match row.pointer(pointer) {
                            None | Some(Value::Null) if *skip_missing => None,
                            value => Some(value.cloned().ok_or(AppRoundProgramError::InvalidValue)),
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(Value::Array)
                })
                .transpose()?,
            Coalesce { values: candidates } => candidates
                .iter()
                .filter_map(|id| get(*id))
                .find(|value| !value.is_null())
                .cloned(),
            Equal { left, right } => match (get(*left), get(*right)) {
                (Some(left), Some(right)) => Some(json!(left == right)),
                _ => None,
            },
            LessThan { left, right } => get(*left)
                .zip(get(*right))
                .and_then(|(left, right)| number_order(left, right))
                .map(|order| json!(order.is_lt())),
            All { values } => {
                if values.iter().any(|id| bool_at(*id) == Some(false)) {
                    Some(json!(false))
                } else if values.iter().all(|id| bool_at(*id) == Some(true)) {
                    Some(json!(true))
                } else {
                    None
                }
            },
            Any { values } => {
                if values.iter().any(|id| bool_at(*id) == Some(true)) {
                    Some(json!(true))
                } else if values.iter().all(|id| bool_at(*id) == Some(false)) {
                    Some(json!(false))
                } else {
                    None
                }
            },
            Not { value } => bool_at(*value).map(|value| json!(!value)),
            Present { value } => Some(json!(get(*value).is_some_and(|value| !value.is_null()))),
            If {
                condition,
                then_value,
                else_value,
            } => match bool_at(*condition) {
                Some(true) => get(*then_value).cloned(),
                Some(false) => get(*else_value).cloned(),
                None => None,
            },
            Add { left, right } | Subtract { left, right } | Multiply { left, right } => {
                match (get(*left), get(*right)) {
                    (Some(left), Some(right)) => {
                        // Preserve integer identity, including values above f64's
                        // exact range. Overflow never wraps a record counter.
                        if let (Some(left), Some(right)) = (integer(left), integer(right)) {
                            let value = match self {
                                Add { .. } => left.checked_add(right),
                                Subtract { .. } => left.checked_sub(right),
                                _ => left.checked_mul(right),
                            }
                            .ok_or(AppRoundProgramError::InvalidValue)?;
                            let number = if let Ok(value) = i64::try_from(value) {
                                value.into()
                            } else {
                                u64::try_from(value)
                                    .map_err(|_| AppRoundProgramError::InvalidValue)?
                                    .into()
                            };
                            Some(Value::Number(number))
                        } else if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
                            let value = match self {
                                Add { .. } => left + right,
                                Subtract { .. } => left - right,
                                _ => left * right,
                            };
                            Some(Value::Number(
                                serde_json::Number::from_f64(value)
                                    .ok_or(AppRoundProgramError::InvalidValue)?,
                            ))
                        } else {
                            None
                        }
                    },
                    _ => None,
                }
            },
            Clamp {
                value,
                minimum,
                maximum,
            } => match (get(*value), get(*minimum), get(*maximum)) {
                (Some(value), Some(min), Some(max)) => match (
                    number_order(min, max),
                    number_order(value, min),
                    number_order(value, max),
                ) {
                    (Some(order), _, _) if order.is_gt() => {
                        return Err(AppRoundProgramError::InvalidValue)
                    },
                    (Some(_), Some(low), Some(high)) => Some(
                        if low.is_lt() {
                            min
                        } else if high.is_gt() {
                            max
                        } else {
                            value
                        }
                        .clone(),
                    ),
                    _ => None,
                },
                _ => None,
            },
            Length { value } => get(*value).and_then(|value| match value {
                Value::Array(items) => Some(json!(items.len())),
                Value::String(text) => Some(json!(text.chars().count())),
                _ => None,
            }),
            Trim { value } => get(*value)
                .and_then(Value::as_str)
                .map(|value| json!(value.trim())),
            Truncate { value, max_chars } => match (
                get(*value).and_then(Value::as_str),
                get(*max_chars).and_then(Value::as_u64),
            ) {
                (Some(value), Some(limit)) => Some(json!(value
                    .chars()
                    .take(usize::try_from(limit).unwrap_or(usize::MAX))
                    .collect::<String>())),
                _ => None,
            },
            Contains { value, member } => match (get(*value), get(*member)) {
                (Some(Value::Array(values)), Some(member)) => Some(json!(values.contains(member))),
                (Some(Value::String(value)), Some(Value::String(member))) => {
                    Some(json!(!member.is_empty() && value.contains(member)))
                },
                _ => None,
            },
            ElapsedSeconds { timestamp } => get(*timestamp)
                .and_then(Value::as_str)
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|time| {
                    json!(environment
                        .now
                        .signed_duration_since(time)
                        .num_seconds()
                        .max(0))
                }),
            Lookup {
                rows,
                key_pointer,
                key,
                value_pointer,
            } => match (get(*rows).and_then(Value::as_array), get(*key)) {
                (Some(rows), Some(key)) => {
                    let mut matches = rows
                        .iter()
                        .filter(|row| row.pointer(key_pointer) == Some(key));
                    let found = matches
                        .next()
                        .and_then(|row| row.pointer(value_pointer))
                        .cloned();
                    if matches.next().is_some() {
                        return Err(AppRoundProgramError::InvalidValue);
                    }
                    found
                },
                _ => None,
            },
            StableId { prefix, parts } => {
                let parts: Option<Vec<&Value>> = parts.iter().map(|id| get(*id)).collect();
                parts.map(|parts| {
                    if parts.iter().any(|part| part.is_object() || part.is_array()) {
                        return Err(AppRoundProgramError::InvalidValue);
                    }
                    let bytes = serde_json::to_vec(&json!({"domain":"app-round-record.v1", "prefix":prefix, "parts":parts}))
                        .map_err(|_| AppRoundProgramError::InvalidValue)?;
                    Ok(json!(format!("{prefix}-{}", blake3::hash(&bytes))))
                }).transpose()?
            },
        })
    }
}
