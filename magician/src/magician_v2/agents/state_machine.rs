//! Phase 4 declarative state machine interpreter.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MACHINE_CATALOG_NAME: &str = "__state_machine_catalog__";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateMachineDefinition {
    pub initial_state: String,
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub transitions: Vec<StateMachineTransition>,
    #[serde(default)]
    pub guards: HashMap<String, StateMachineGuard>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateMachineTransition {
    pub from: String,
    pub to: String,
    pub on: StateMachineTrigger,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub actions: Vec<StateMachineAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StateMachineTrigger {
    Single(String),
    Many(Vec<String>),
}

impl StateMachineTrigger {
    fn matches(&self, trigger: &str) -> bool {
        let trigger = trigger.trim();
        match self {
            Self::Single(value) => value.trim() == trigger,
            Self::Many(values) => values.iter().any(|value| value.trim() == trigger),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Single(value) => value.trim().is_empty(),
            Self::Many(values) => values.iter().all(|value| value.trim().is_empty()),
        }
    }

    fn has_blank_member(&self) -> bool {
        match self {
            Self::Single(value) => value.trim().is_empty(),
            Self::Many(values) => values.iter().any(|value| value.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateMachineGuard {
    pub expr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StateMachineAction {
    EmitEvent,
    ResumeCycle,
    FailEpisode { reason: String },
    DismissOtherDeliveries,
    PersistState,
}

impl StateMachineAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EmitEvent => "emit_event",
            Self::ResumeCycle => "resume_cycle",
            Self::FailEpisode { .. } => "fail_episode",
            Self::DismissOtherDeliveries => "dismiss_other_deliveries",
            Self::PersistState => "persist_state",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TransitionContext {
    values: HashMap<String, Value>,
}

impl TransitionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(mut self, key: impl Into<String>, value: Value) -> Self {
        self.values.insert(key.into(), value);
        self
    }

    pub fn insert_value(&mut self, key: impl Into<String>, value: Value) {
        self.values.insert(key.into(), value);
    }

    pub fn insert_string(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.insert_value(key, Value::String(value.into()));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key).or_else(|| {
            key.strip_prefix("context.")
                .and_then(|trimmed| self.values.get(trimmed))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMachineTransitionResolution {
    pub from: String,
    pub to: String,
    pub trigger: String,
    pub guard: Option<String>,
    pub actions: Vec<StateMachineAction>,
    pub used_fallback: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateMachineError {
    #[error("state machine `{machine}` not found")]
    UnknownMachine { machine: String },
    #[error("invalid state machine definition for `{machine}`: {reason}")]
    InvalidDefinition { machine: String, reason: String },
    #[error("invalid transition for `{machine}` from `{state}` on `{trigger}`")]
    InvalidTransition {
        machine: String,
        state: String,
        trigger: String,
    },
    #[error("guard evaluation failed in `{machine}` for `{guard}`: {reason}")]
    GuardEvaluation {
        machine: String,
        guard: String,
        reason: String,
    },
    #[error("guard rejected transition in `{machine}` from `{state}` on `{trigger}`")]
    GuardRejected {
        machine: String,
        state: String,
        trigger: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct StateMachineInterpreter {
    definitions: HashMap<String, StateMachineDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StateMachineCatalog {
    Wrapped {
        state_machines: HashMap<String, StateMachineDefinition>,
    },
    Bare(HashMap<String, StateMachineDefinition>),
}

impl StateMachineInterpreter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_machine(
        machine: impl Into<String>,
        definition: StateMachineDefinition,
    ) -> Result<Self, StateMachineError> {
        let mut this = Self::new();
        this.register_machine(machine, definition)?;
        Ok(this)
    }

    pub fn register_machine(
        &mut self,
        machine: impl Into<String>,
        definition: StateMachineDefinition,
    ) -> Result<(), StateMachineError> {
        let machine = machine.into();
        self.validate_machine_definition(&machine, &definition)?;
        self.definitions.insert(machine, definition);
        Ok(())
    }

    pub fn register_machines(
        &mut self,
        definitions: HashMap<String, StateMachineDefinition>,
    ) -> Result<(), StateMachineError> {
        if definitions.is_empty() {
            return Err(StateMachineError::InvalidDefinition {
                machine: MACHINE_CATALOG_NAME.to_string(),
                reason: "state machine catalog must not be empty".to_string(),
            });
        }

        let mut validated = Vec::with_capacity(definitions.len());
        for (machine, definition) in definitions {
            if machine.trim().is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: MACHINE_CATALOG_NAME.to_string(),
                    reason: "state machine names must not be empty".to_string(),
                });
            }
            self.validate_machine_definition(&machine, &definition)?;
            validated.push((machine, definition));
        }

        for (machine, definition) in validated {
            self.definitions.insert(machine, definition);
        }
        Ok(())
    }

    pub fn register_machines_from_values(
        &mut self,
        definitions: &HashMap<String, Value>,
    ) -> Result<(), StateMachineError> {
        if definitions.is_empty() {
            return Err(StateMachineError::InvalidDefinition {
                machine: MACHINE_CATALOG_NAME.to_string(),
                reason: "state machine catalog must not be empty".to_string(),
            });
        }

        let mut parsed = HashMap::with_capacity(definitions.len());
        for (machine, definition) in definitions {
            let parsed_definition: StateMachineDefinition =
                serde_json::from_value(definition.clone()).map_err(|err| {
                    StateMachineError::InvalidDefinition {
                        machine: machine.clone(),
                        reason: format!("failed to parse state machine JSON definition: {err}"),
                    }
                })?;
            parsed.insert(machine.clone(), parsed_definition);
        }

        self.register_machines(parsed)
    }

    pub fn register_machines_from_yaml_str(&mut self, yaml: &str) -> Result<(), StateMachineError> {
        let catalog: StateMachineCatalog =
            serde_yaml::from_str(yaml).map_err(|err| StateMachineError::InvalidDefinition {
                machine: MACHINE_CATALOG_NAME.to_string(),
                reason: format!("failed to parse state machine YAML catalog: {err}"),
            })?;

        let definitions = match catalog {
            StateMachineCatalog::Wrapped { state_machines } => state_machines,
            StateMachineCatalog::Bare(definitions) => definitions,
        };
        self.register_machines(definitions)
    }

    pub fn has_machine(&self, machine: &str) -> bool {
        self.definitions.contains_key(machine)
    }

    pub fn transition(
        &self,
        machine: &str,
        current_state: &str,
        trigger: &str,
        context: &TransitionContext,
    ) -> Result<StateMachineTransitionResolution, StateMachineError> {
        let trigger = trigger.trim();
        if trigger.is_empty() {
            return Err(StateMachineError::InvalidTransition {
                machine: machine.to_string(),
                state: current_state.to_string(),
                trigger: String::new(),
            });
        }

        let definition =
            self.definitions
                .get(machine)
                .ok_or_else(|| StateMachineError::UnknownMachine {
                    machine: machine.to_string(),
                })?;

        let primary = self.resolve_transition(machine, definition, current_state, trigger, context);
        if let Err(StateMachineError::GuardRejected { .. }) = &primary {
            let fallback_trigger = format!("!{trigger}");
            match self.resolve_transition(
                machine,
                definition,
                current_state,
                &fallback_trigger,
                context,
            ) {
                Ok(mut resolution) => {
                    resolution.used_fallback = true;
                    return Ok(resolution);
                },
                Err(StateMachineError::InvalidTransition { .. }) => {},
                Err(err) => return Err(err),
            }
        }
        primary
    }

    fn validate_machine_definition(
        &self,
        machine: &str,
        definition: &StateMachineDefinition,
    ) -> Result<(), StateMachineError> {
        if definition.initial_state.trim().is_empty() {
            return Err(StateMachineError::InvalidDefinition {
                machine: machine.to_string(),
                reason: "initial_state must not be empty".to_string(),
            });
        }
        if definition.transitions.is_empty() {
            return Err(StateMachineError::InvalidDefinition {
                machine: machine.to_string(),
                reason: "transitions must not be empty".to_string(),
            });
        }
        if !definition.states.is_empty() && !definition.states.contains(&definition.initial_state) {
            return Err(StateMachineError::InvalidDefinition {
                machine: machine.to_string(),
                reason: "initial_state must be present in states".to_string(),
            });
        }
        for (guard_name, guard) in &definition.guards {
            if guard_name.trim().is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: "guard names must not be empty".to_string(),
                });
            }
            if guard.expr.trim().is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: format!("guard `{guard_name}` expression must not be empty"),
                });
            }
        }

        for transition in &definition.transitions {
            if transition.from.trim().is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: "transition from must not be empty".to_string(),
                });
            }
            if transition.to.trim().is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: "transition to must not be empty".to_string(),
                });
            }
            if transition.on.is_empty() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: format!(
                        "transition `{}` -> `{}` must define at least one trigger",
                        transition.from, transition.to
                    ),
                });
            }
            if transition.on.has_blank_member() {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: format!(
                        "transition `{}` -> `{}` contains empty trigger entries",
                        transition.from, transition.to
                    ),
                });
            }
            if let Some(when) = transition.when.as_deref() {
                let when = when.trim();
                if when.is_empty() {
                    return Err(StateMachineError::InvalidDefinition {
                        machine: machine.to_string(),
                        reason: format!(
                            "transition `{}` -> `{}` guard expression must not be empty",
                            transition.from, transition.to
                        ),
                    });
                }
                if let Some(named_guard) = named_guard_reference_candidate(when) {
                    if !definition.guards.contains_key(named_guard) {
                        return Err(StateMachineError::InvalidDefinition {
                            machine: machine.to_string(),
                            reason: format!(
                                "transition `{}` -> `{}` references unknown guard `{}`",
                                transition.from, transition.to, named_guard
                            ),
                        });
                    }
                }
            }
            if !definition.states.is_empty()
                && (!definition.states.contains(&transition.from)
                    || !definition.states.contains(&transition.to))
            {
                return Err(StateMachineError::InvalidDefinition {
                    machine: machine.to_string(),
                    reason: format!(
                        "transition `{}` -> `{}` references state outside states list",
                        transition.from, transition.to
                    ),
                });
            }
        }

        Ok(())
    }

    fn resolve_transition(
        &self,
        machine: &str,
        definition: &StateMachineDefinition,
        current_state: &str,
        trigger: &str,
        context: &TransitionContext,
    ) -> Result<StateMachineTransitionResolution, StateMachineError> {
        let candidates = definition
            .transitions
            .iter()
            .filter(|transition| transition.from == current_state && transition.on.matches(trigger))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err(StateMachineError::InvalidTransition {
                machine: machine.to_string(),
                state: current_state.to_string(),
                trigger: trigger.to_string(),
            });
        }

        let mut had_guard = false;
        for transition in candidates {
            let guard_allows = if let Some(guard) = transition.when.as_deref() {
                had_guard = true;
                self.evaluate_guard(machine, definition, guard, context)?
            } else {
                true
            };
            if guard_allows {
                return Ok(StateMachineTransitionResolution {
                    from: transition.from.clone(),
                    to: transition.to.clone(),
                    trigger: trigger.to_string(),
                    guard: transition.when.clone(),
                    actions: transition.actions.clone(),
                    used_fallback: false,
                });
            }
        }

        if had_guard {
            Err(StateMachineError::GuardRejected {
                machine: machine.to_string(),
                state: current_state.to_string(),
                trigger: trigger.to_string(),
            })
        } else {
            Err(StateMachineError::InvalidTransition {
                machine: machine.to_string(),
                state: current_state.to_string(),
                trigger: trigger.to_string(),
            })
        }
    }

    fn evaluate_guard(
        &self,
        machine: &str,
        definition: &StateMachineDefinition,
        guard: &str,
        context: &TransitionContext,
    ) -> Result<bool, StateMachineError> {
        let mut visited = HashSet::new();
        evaluate_guard_expression(machine, definition, guard, context, &mut visited)
    }
}

fn evaluate_guard_expression(
    machine: &str,
    definition: &StateMachineDefinition,
    expression: &str,
    context: &TransitionContext,
    visited_guards: &mut HashSet<String>,
) -> Result<bool, StateMachineError> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Err(StateMachineError::GuardEvaluation {
            machine: machine.to_string(),
            guard: expression.to_string(),
            reason: "guard expression must not be empty".to_string(),
        });
    }

    if let Some(guard_def) = definition.guards.get(expression) {
        if !visited_guards.insert(expression.to_string()) {
            return Err(StateMachineError::GuardEvaluation {
                machine: machine.to_string(),
                guard: expression.to_string(),
                reason: "recursive guard definition detected".to_string(),
            });
        }
        let result = evaluate_guard_expression(
            machine,
            definition,
            &guard_def.expr,
            context,
            visited_guards,
        );
        visited_guards.remove(expression);
        return result;
    }

    if let Some(stripped) = expression.strip_prefix('!') {
        let value = evaluate_guard_expression(
            machine,
            definition,
            stripped.trim(),
            context,
            visited_guards,
        )?;
        return Ok(!value);
    }

    if expression.eq_ignore_ascii_case("true") {
        return Ok(true);
    }
    if expression.eq_ignore_ascii_case("false") {
        return Ok(false);
    }

    if let Some((lhs, op, rhs)) = split_binary_expression(expression) {
        let lhs_value = resolve_operand(machine, definition, lhs, context, visited_guards)?;
        let rhs_value = resolve_operand(machine, definition, rhs, context, visited_guards)?;
        return compare_operands(machine, expression, lhs_value, op, rhs_value);
    }

    if let Some(value) = context.get(expression) {
        return Ok(value_to_bool(value));
    }
    if let Some(path) = expression.strip_prefix("context.") {
        if let Some(value) = context.get(path) {
            return Ok(value_to_bool(value));
        }
    }

    Err(StateMachineError::GuardEvaluation {
        machine: machine.to_string(),
        guard: expression.to_string(),
        reason: "unsupported guard expression".to_string(),
    })
}

fn split_binary_expression(expression: &str) -> Option<(&str, &'static str, &str)> {
    const OPERATORS: [&str; 6] = ["==", "!=", ">=", "<=", ">", "<"];
    for op in OPERATORS {
        if let Some(index) = find_operator_outside_quotes(expression, op) {
            let lhs = expression[..index].trim();
            let rhs = expression[index + op.len()..].trim();
            if lhs.is_empty() || rhs.is_empty() {
                continue;
            }
            return Some((lhs, op, rhs));
        }
    }
    None
}

fn find_operator_outside_quotes(expression: &str, operator: &str) -> Option<usize> {
    let bytes = expression.as_bytes();
    let op_bytes = operator.as_bytes();
    let mut index = 0usize;
    let mut in_quote: Option<u8> = None;
    let mut escaped = false;

    while index + op_bytes.len() <= bytes.len() {
        let ch = bytes[index];
        if let Some(quote) = in_quote {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == quote {
                in_quote = None;
            }
            index += 1;
            continue;
        }

        if ch == b'\'' || ch == b'"' {
            in_quote = Some(ch);
            index += 1;
            continue;
        }

        if &bytes[index..index + op_bytes.len()] == op_bytes {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn resolve_operand(
    machine: &str,
    definition: &StateMachineDefinition,
    token: &str,
    context: &TransitionContext,
    visited_guards: &mut HashSet<String>,
) -> Result<Value, StateMachineError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(StateMachineError::GuardEvaluation {
            machine: machine.to_string(),
            guard: token.to_string(),
            reason: "operand must not be empty".to_string(),
        });
    }

    if let Some(value) = parse_quoted_string(token) {
        return Ok(Value::String(value));
    }
    if token.eq_ignore_ascii_case("true") {
        return Ok(Value::Bool(true));
    }
    if token.eq_ignore_ascii_case("false") {
        return Ok(Value::Bool(false));
    }
    if token.eq_ignore_ascii_case("null") {
        return Ok(Value::Null);
    }
    if let Ok(value) = token.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(value) {
            return Ok(Value::Number(number));
        }
    }
    if let Some(value) = context.get(token) {
        return Ok(value.clone());
    }
    if let Some(path) = token.strip_prefix("context.") {
        if let Some(value) = context.get(path) {
            return Ok(value.clone());
        }
    }
    if definition.guards.contains_key(token) {
        return Ok(Value::Bool(evaluate_guard_expression(
            machine,
            definition,
            token,
            context,
            visited_guards,
        )?));
    }

    Err(StateMachineError::GuardEvaluation {
        machine: machine.to_string(),
        guard: token.to_string(),
        reason: "unable to resolve operand".to_string(),
    })
}

fn parse_quoted_string(token: &str) -> Option<String> {
    if token.len() < 2 {
        return None;
    }
    let bytes = token.as_bytes();
    let quote = bytes[0];
    if (quote == b'"' || quote == b'\'') && bytes[token.len() - 1] == quote {
        return Some(token[1..token.len() - 1].to_string());
    }
    None
}

fn named_guard_reference_candidate(expression: &str) -> Option<&str> {
    let trimmed = expression.trim();
    let candidate = trimmed.strip_prefix('!').map(str::trim).unwrap_or(trimmed);
    if candidate.is_empty()
        || candidate.starts_with("context.")
        || is_guard_literal(candidate)
        || !is_bare_identifier(candidate)
    {
        return None;
    }
    Some(candidate)
}

fn is_guard_literal(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "false" | "null"
    ) || value.parse::<f64>().is_ok()
}

fn is_bare_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn compare_operands(
    machine: &str,
    expression: &str,
    lhs: Value,
    op: &str,
    rhs: Value,
) -> Result<bool, StateMachineError> {
    match op {
        "==" => Ok(values_equal(&lhs, &rhs)),
        "!=" => Ok(!values_equal(&lhs, &rhs)),
        ">" | "<" | ">=" | "<=" => compare_ordered(machine, expression, &lhs, op, &rhs),
        _ => Err(StateMachineError::GuardEvaluation {
            machine: machine.to_string(),
            guard: expression.to_string(),
            reason: format!("unsupported operator `{op}`"),
        }),
    }
}

fn compare_ordered(
    machine: &str,
    expression: &str,
    lhs: &Value,
    op: &str,
    rhs: &Value,
) -> Result<bool, StateMachineError> {
    if let (Some(lhs_num), Some(rhs_num)) = (value_to_number(lhs), value_to_number(rhs)) {
        return Ok(match op {
            ">" => lhs_num > rhs_num,
            "<" => lhs_num < rhs_num,
            ">=" => lhs_num >= rhs_num,
            "<=" => lhs_num <= rhs_num,
            _ => false,
        });
    }

    if let (Some(lhs_str), Some(rhs_str)) = (lhs.as_str(), rhs.as_str()) {
        return Ok(match op {
            ">" => lhs_str > rhs_str,
            "<" => lhs_str < rhs_str,
            ">=" => lhs_str >= rhs_str,
            "<=" => lhs_str <= rhs_str,
            _ => false,
        });
    }

    Err(StateMachineError::GuardEvaluation {
        machine: machine.to_string(),
        guard: expression.to_string(),
        reason: "ordered comparison requires numeric or string operands".to_string(),
    })
}

fn value_to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

fn values_equal(lhs: &Value, rhs: &Value) -> bool {
    if let (Some(lhs_num), Some(rhs_num)) = (value_to_number(lhs), value_to_number(rhs)) {
        return (lhs_num - rhs_num).abs() <= f64::EPSILON;
    }
    lhs == rhs
}

fn value_to_bool(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Number(number) => number
            .as_i64()
            .map(|value| value != 0)
            .or_else(|| number.as_u64().map(|value| value != 0))
            .or_else(|| number.as_f64().map(|value| value.abs() > f64::EPSILON))
            .unwrap_or(false),
        Value::String(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !(normalized.is_empty() || normalized == "false" || normalized == "0")
        },
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Null => false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn approval_machine() -> StateMachineDefinition {
        StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec![
                "pending".to_string(),
                "validating".to_string(),
                "approved".to_string(),
                "rejected".to_string(),
                "expired".to_string(),
            ],
            transitions: vec![
                StateMachineTransition {
                    from: "pending".to_string(),
                    to: "validating".to_string(),
                    on: StateMachineTrigger::Single("decision_approve".to_string()),
                    when: None,
                    actions: vec![StateMachineAction::PersistState],
                },
                StateMachineTransition {
                    from: "validating".to_string(),
                    to: "approved".to_string(),
                    on: StateMachineTrigger::Single("decision_approve".to_string()),
                    when: Some("plan_hash_valid".to_string()),
                    actions: vec![StateMachineAction::PersistState],
                },
                StateMachineTransition {
                    from: "validating".to_string(),
                    to: "rejected".to_string(),
                    on: StateMachineTrigger::Single("decision_approve".to_string()),
                    when: Some("plan_hash_invalid".to_string()),
                    actions: vec![StateMachineAction::PersistState],
                },
                StateMachineTransition {
                    from: "pending".to_string(),
                    to: "rejected".to_string(),
                    on: StateMachineTrigger::Single("decision_reject".to_string()),
                    when: None,
                    actions: vec![StateMachineAction::PersistState],
                },
                StateMachineTransition {
                    from: "pending".to_string(),
                    to: "rejected".to_string(),
                    on: StateMachineTrigger::Single("!decision_approve".to_string()),
                    when: None,
                    actions: vec![StateMachineAction::PersistState],
                },
            ],
            guards: HashMap::from([
                (
                    "plan_hash_valid".to_string(),
                    StateMachineGuard {
                        expr: "context.current_plan_hash == context.approval.plan_hash".to_string(),
                    },
                ),
                (
                    "plan_hash_invalid".to_string(),
                    StateMachineGuard {
                        expr: "context.current_plan_hash != context.approval.plan_hash".to_string(),
                    },
                ),
            ]),
        }
    }

    #[test]
    fn transition_resolves_named_guard() {
        let interpreter =
            StateMachineInterpreter::with_machine("approval", approval_machine()).unwrap();
        let mut context = TransitionContext::new();
        context.insert_string("current_plan_hash", "abc");
        context.insert_string("approval.plan_hash", "abc");

        let resolved = interpreter
            .transition("approval", "validating", "decision_approve", &context)
            .unwrap();
        assert_eq!(resolved.to, "approved");
        assert_eq!(resolved.guard.as_deref(), Some("plan_hash_valid"));
        assert!(!resolved.used_fallback);
    }

    #[test]
    fn transition_resolves_guard_fallback_trigger() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec![
                "pending".to_string(),
                "approved".to_string(),
                "rejected".to_string(),
            ],
            transitions: vec![
                StateMachineTransition {
                    from: "pending".to_string(),
                    to: "approved".to_string(),
                    on: StateMachineTrigger::Single("approve".to_string()),
                    when: Some("context.allow == true".to_string()),
                    actions: vec![StateMachineAction::PersistState],
                },
                StateMachineTransition {
                    from: "pending".to_string(),
                    to: "rejected".to_string(),
                    on: StateMachineTrigger::Single("!approve".to_string()),
                    when: None,
                    actions: vec![StateMachineAction::PersistState],
                },
            ],
            guards: HashMap::new(),
        };
        let interpreter = StateMachineInterpreter::with_machine("approval", machine).unwrap();
        let context = TransitionContext::new().with_value("allow", Value::Bool(false));

        let resolved = interpreter
            .transition("approval", "pending", "approve", &context)
            .unwrap();
        assert_eq!(resolved.to, "rejected");
        assert_eq!(resolved.trigger, "!approve");
        assert!(resolved.used_fallback);
    }

    #[test]
    fn transition_returns_guard_rejected_without_fallback() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "approved".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "approved".to_string(),
                on: StateMachineTrigger::Single("approve".to_string()),
                when: Some("context.allow == true".to_string()),
                actions: vec![StateMachineAction::PersistState],
            }],
            guards: HashMap::new(),
        };
        let interpreter = StateMachineInterpreter::with_machine("test", machine).unwrap();

        let context = TransitionContext::new().with_value("allow", Value::Bool(false));
        let err = interpreter
            .transition("test", "pending", "approve", &context)
            .unwrap_err();
        assert!(matches!(err, StateMachineError::GuardRejected { .. }));
    }

    #[test]
    fn guard_expression_supports_numeric_and_boolean_comparisons() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "done".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "done".to_string(),
                on: StateMachineTrigger::Single("complete".to_string()),
                when: Some("context.remaining_iterations > 0".to_string()),
                actions: vec![StateMachineAction::PersistState],
            }],
            guards: HashMap::new(),
        };
        let interpreter = StateMachineInterpreter::with_machine("test", machine).unwrap();
        let mut context = TransitionContext::new();
        context.insert_value("remaining_iterations", serde_json::json!(2));
        context.insert_value("has_budget", Value::Bool(true));

        let resolved = interpreter
            .transition("test", "pending", "complete", &context)
            .unwrap();
        assert_eq!(resolved.to, "done");
    }

    #[test]
    fn transition_resolution_preserves_action_order() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "done".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "done".to_string(),
                on: StateMachineTrigger::Single("complete".to_string()),
                when: None,
                actions: vec![
                    StateMachineAction::EmitEvent,
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            }],
            guards: HashMap::new(),
        };
        let interpreter = StateMachineInterpreter::with_machine("test", machine).unwrap();

        let resolved = interpreter
            .transition("test", "pending", "complete", &TransitionContext::default())
            .unwrap();
        assert_eq!(
            resolved.actions,
            vec![
                StateMachineAction::EmitEvent,
                StateMachineAction::PersistState,
                StateMachineAction::DismissOtherDeliveries,
            ]
        );
    }

    #[test]
    fn transition_reports_invalid_transition() {
        let interpreter =
            StateMachineInterpreter::with_machine("approval", approval_machine()).unwrap();
        let err = interpreter
            .transition(
                "approval",
                "pending",
                "nonexistent_trigger",
                &TransitionContext::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StateMachineError::InvalidTransition { .. }));
    }

    #[test]
    fn register_machine_rejects_empty_transition_trigger() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "done".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "done".to_string(),
                on: StateMachineTrigger::Single(" ".to_string()),
                when: None,
                actions: vec![StateMachineAction::PersistState],
            }],
            guards: HashMap::new(),
        };

        let err = StateMachineInterpreter::with_machine("test", machine).unwrap_err();
        assert!(matches!(err, StateMachineError::InvalidDefinition { .. }));
    }

    #[test]
    fn register_machine_rejects_blank_member_in_multi_trigger() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "done".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "done".to_string(),
                on: StateMachineTrigger::Many(vec!["complete".to_string(), " ".to_string()]),
                when: None,
                actions: vec![StateMachineAction::PersistState],
            }],
            guards: HashMap::new(),
        };

        let err = StateMachineInterpreter::with_machine("test", machine).unwrap_err();
        assert!(matches!(err, StateMachineError::InvalidDefinition { .. }));
    }

    #[test]
    fn register_machine_rejects_unknown_named_guard_reference() {
        let machine = StateMachineDefinition {
            initial_state: "pending".to_string(),
            states: vec!["pending".to_string(), "done".to_string()],
            transitions: vec![StateMachineTransition {
                from: "pending".to_string(),
                to: "done".to_string(),
                on: StateMachineTrigger::Single("complete".to_string()),
                when: Some("missing_guard".to_string()),
                actions: vec![StateMachineAction::PersistState],
            }],
            guards: HashMap::new(),
        };

        let err = StateMachineInterpreter::with_machine("test", machine).unwrap_err();
        assert!(matches!(err, StateMachineError::InvalidDefinition { .. }));
    }

    #[test]
    fn register_machines_from_yaml_supports_wrapped_catalog() {
        let yaml = r#"
state_machines:
  approval:
    initial_state: pending
    states: [pending, approved]
    transitions:
      - from: pending
        to: approved
        on: approve
        actions: [persist_state]
"#;
        let mut interpreter = StateMachineInterpreter::new();
        interpreter.register_machines_from_yaml_str(yaml).unwrap();
        assert!(interpreter.has_machine("approval"));
    }

    #[test]
    fn register_machines_from_values_supports_agent_definition_shape() {
        let mut interpreter = StateMachineInterpreter::new();
        let definitions = HashMap::from([(
            "approval".to_string(),
            serde_json::json!({
                "initial_state": "pending",
                "states": ["pending", "approved"],
                "transitions": [
                    {
                        "from": "pending",
                        "to": "approved",
                        "on": "approve",
                        "actions": ["persist_state"]
                    }
                ]
            }),
        )]);

        interpreter
            .register_machines_from_values(&definitions)
            .unwrap();
        assert!(interpreter.has_machine("approval"));
    }

    #[test]
    fn transition_reports_unknown_machine() {
        let interpreter = StateMachineInterpreter::new();
        let err = interpreter
            .transition(
                "missing",
                "pending",
                "approve",
                &TransitionContext::default(),
            )
            .unwrap_err();
        assert!(matches!(err, StateMachineError::UnknownMachine { .. }));
    }
}
