//! Reviewed query and mutation shapes for the reusable native round node.
//! Produces ordinary App-store arguments, never grants store or model authority.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::contextual_round::AppRoundLimits;
use super::contextual_round_program::{
    AppRoundPreparedValues, AppRoundProgramError, AppRoundValueEnvironment, AppRoundValueId,
    AppRoundValueProgram, AppRoundValueSource,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundContextQuery {
    pub name: String,
    pub entity: String,
    pub per_participant: bool,
    /// In progressive mode, shared changing data can opt into each speaker's
    /// dispatch snapshot. Per-participant queries are always read at dispatch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_before_dispatch: bool,
    /// Only ordinary own-store select/predicate/order/limit arguments.
    pub parameters: BTreeMap<String, Value>,
    /// Bind a value inside a declared comparison; never its field or operator.
    pub bindings: BTreeMap<String, AppRoundValueId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundEligibilityRule {
    pub require: AppRoundValueId,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRoundRecordChange {
    Create {
        record_id: AppRoundValueId,
        fields: BTreeMap<String, AppRoundValueId>,
    },
    Update {
        record_id: AppRoundValueId,
        revision: AppRoundValueId,
        fields: BTreeMap<String, AppRoundValueId>,
    },
    Delete {
        record_id: AppRoundValueId,
        revision: AppRoundValueId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundMutationRule {
    pub entity: String,
    pub for_each: Option<AppRoundValueId>,
    pub when: Option<AppRoundValueId>,
    /// Distinguishes actual semantic results from cursor/mood/receipt updates.
    pub semantic_result: bool,
    pub change: AppRoundRecordChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundProgramDeclaration {
    pub values: AppRoundValueProgram,
    pub participant_id_pointer: String,
    /// Shared persisted cursor, evaluated before selecting participants.
    #[serde(default)]
    pub resume_after: Option<AppRoundValueId>,
    /// Progressive rounds refresh permitted store context before each speaker,
    /// after the previous speaker commits. Snapshot rounds stay concurrent.
    #[serde(default, skip_serializing_if = "AppRoundContextMode::is_snapshot")]
    pub context_mode: AppRoundContextMode,
    pub queries: Vec<AppRoundContextQuery>,
    pub eligibility: Vec<AppRoundEligibilityRule>,
    pub semantic_step: String,
    pub max_output_tokens: u32,
    pub limits: AppRoundLimits,
    pub draft_when: AppRoundValueId,
    pub quiet_when: AppRoundValueId,
    pub quiet_reason: AppRoundValueId,
    pub mutations: Vec<AppRoundMutationRule>,
    pub final_mutations: Vec<AppRoundMutationRule>,
    pub max_mutations_per_participant: u16,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRoundContextMode {
    #[default]
    Snapshot,
    Progressive,
}

impl AppRoundContextMode {
    fn is_snapshot(&self) -> bool {
        *self == Self::Snapshot
    }
}

pub struct AppRoundMutationPlan {
    pub operations: Vec<Value>,
    pub expected_record_revisions: Vec<Value>,
    pub semantic_record_ids: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppRoundSemanticOutcome {
    Draft,
    Quiet(String),
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn identity(value: &Value) -> Result<&str, AppRoundProgramError> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        })
        .ok_or(AppRoundProgramError::InvalidValue)
}

impl AppRoundProgramDeclaration {
    /// Retention covers source pages, shared preparation/final reads, and
    /// each discovered candidate's preparation and possible commit attempts.
    /// Callers supply only the row count from an attested source page.
    pub fn retained_query_slots(
        &self,
        source_rows: usize,
        source_pages: u16,
    ) -> Result<usize, AppRoundProgramError> {
        let per_candidate = self.queries.iter().filter(|q| q.per_participant).count();
        let shared = self.queries.len() - per_candidate;
        if self.context_mode == AppRoundContextMode::Progressive {
            let refresh = self
                .queries
                .iter()
                .filter(|q| q.per_participant || q.refresh_before_dispatch)
                .count();
            return source_rows
                .checked_mul(refresh)
                .and_then(|count| count.checked_add(shared * 2))
                .and_then(|count| count.checked_add(usize::from(source_pages)))
                .ok_or(AppRoundProgramError::InvalidProgram);
        }
        source_rows
            .checked_mul(per_candidate)
            .and_then(|count| {
                count.checked_mul(usize::from(self.limits.max_attempts_per_participant) + 1)
            })
            .and_then(|count| count.checked_add(shared * 2))
            .and_then(|count| count.checked_add(usize::from(source_pages)))
            .ok_or(AppRoundProgramError::InvalidProgram)
    }

    pub fn validate(&self) -> Result<(), AppRoundProgramError> {
        self.values.validate()?;
        self.limits
            .validate()
            .map_err(|_| AppRoundProgramError::InvalidProgram)?;
        if !name(&self.semantic_step)
            || (self.context_mode == AppRoundContextMode::Progressive
                && self.limits.max_concurrent != 1)
            || self.max_output_tokens == 0
            || u64::from(self.max_output_tokens) > self.limits.per_participant.tokens
            || self.max_mutations_per_participant == 0
            || self.queries.len() > 64
            || self.eligibility.len() > 64
            || self.mutations.len() > 256
            || self.final_mutations.len() > 256
            || self.participant_id_pointer.len() > 4096
            || !self.participant_id_pointer.starts_with('/')
        {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        self.values
            .sources([self.draft_when, self.quiet_when, self.quiet_reason])?;
        if let Some(cursor) = self.resume_after {
            self.values.require_pre_model([cursor])?;
            if self
                .values
                .sources([cursor])?
                .contains(&AppRoundValueSource::Participant)
            {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
        validate_context_queries(&self.values, &self.queries, false)?;
        for rule in &self.eligibility {
            if !name(&rule.reason) {
                return Err(AppRoundProgramError::InvalidProgram);
            }
            self.values.require_pre_model([rule.require])?;
        }
        validate_mutation_rules(&self.values, &self.mutations, false)?;
        validate_mutation_rules(&self.values, &self.final_mutations, true)?;
        Ok(())
    }

    pub fn query_parameters(
        &self,
        query: &AppRoundContextQuery,
        values: &AppRoundPreparedValues,
    ) -> Result<BTreeMap<String, Value>, AppRoundProgramError> {
        query_parameters(query, values)
    }

    pub fn exclusion(&self, values: &AppRoundPreparedValues) -> Option<String> {
        self.eligibility
            .iter()
            .find(|rule| !values.condition(rule.require))
            .map(|rule| rule.reason.clone())
    }

    pub fn semantic_outcome(
        &self,
        values: &AppRoundPreparedValues,
    ) -> Result<AppRoundSemanticOutcome, AppRoundProgramError> {
        match (
            values.condition(self.draft_when),
            values.condition(self.quiet_when),
        ) {
            (true, false) => Ok(AppRoundSemanticOutcome::Draft),
            (false, true) => {
                let reason = values
                    .get(self.quiet_reason)?
                    .as_str()
                    .filter(|reason| name(reason))
                    .ok_or(AppRoundProgramError::InvalidValue)?;
                Ok(AppRoundSemanticOutcome::Quiet(reason.to_owned()))
            },
            _ => Err(AppRoundProgramError::InvalidValue),
        }
    }

    pub fn plan_mutations(
        &self,
        final_phase: bool,
        environment: &AppRoundValueEnvironment<'_>,
    ) -> Result<AppRoundMutationPlan, AppRoundProgramError> {
        self.validate()?;
        plan_app_mutations(
            &self.values,
            if final_phase {
                &self.final_mutations
            } else {
                &self.mutations
            },
            environment,
            self.max_mutations_per_participant,
        )
    }

    pub fn mutation_entities(&self) -> BTreeSet<&str> {
        self.mutations
            .iter()
            .chain(&self.final_mutations)
            .map(|rule| rule.entity.as_str())
            .collect()
    }
}

pub(super) fn validate_context_queries(
    values: &AppRoundValueProgram,
    queries: &[AppRoundContextQuery],
    allow_context: bool,
) -> Result<(), AppRoundProgramError> {
    let mut query_names = BTreeSet::new();
    for query in queries {
        if !name(&query.name)
            || matches!(query.name.as_str(), "participant" | "round")
            || !name(&query.entity)
            || !query_names.insert(&query.name)
            || query
                .parameters
                .keys()
                .any(|key| !matches!(key.as_str(), "select" | "predicate" | "order" | "limit"))
            || query
                .parameters
                .get("limit")
                .and_then(Value::as_u64)
                .is_none_or(|limit| !(1..=100).contains(&limit))
            || query
                .parameters
                .get("select")
                .and_then(Value::as_array)
                .is_none_or(|fields| {
                    fields.is_empty()
                        || fields.len() > 256
                        || fields.iter().any(|field| field.as_str().is_none())
                })
        {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        let parameters = serde_json::to_value(&query.parameters)
            .map_err(|_| AppRoundProgramError::InvalidProgram)?;
        for (pointer, id) in &query.bindings {
            let segments: Vec<_> = pointer.split('/').collect();
            if segments.len() != 5
                || segments[0] != ""
                || segments[1] != "predicate"
                || segments[2] != "nodes"
                || !matches!(segments[4], "value" | "values")
                || segments[3].parse::<usize>().is_err()
                || parameters.pointer(pointer).is_none()
                || parameters
                    .pointer(&format!("/predicate/nodes/{}/kind", segments[3]))
                    .and_then(Value::as_str)
                    != Some(if segments[4] == "values" {
                        "in"
                    } else {
                        "compare"
                    })
            {
                return Err(AppRoundProgramError::InvalidProgram);
            }
            let sources = values.sources([*id])?;
            if sources.iter().any(|source| {
                matches!(
                    source,
                    AppRoundValueSource::Model | AppRoundValueSource::Item
                )
            }) || (!allow_context && sources.contains(&AppRoundValueSource::Context))
                || (!query.per_participant && sources.contains(&AppRoundValueSource::Participant))
            {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
    }
    Ok(())
}

pub(super) fn validate_mutation_rules(
    values: &AppRoundValueProgram,
    rules: &[AppRoundMutationRule],
    final_phase: bool,
) -> Result<(), AppRoundProgramError> {
    for rule in rules {
        if !name(&rule.entity) || (final_phase && rule.semantic_result) {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        let empty_fields = BTreeMap::new();
        let (record_id, fields, revision) = match &rule.change {
            AppRoundRecordChange::Create { record_id, fields } => (*record_id, fields, None),
            AppRoundRecordChange::Update {
                record_id,
                revision,
                fields,
            } => (*record_id, fields, Some(*revision)),
            AppRoundRecordChange::Delete {
                record_id,
                revision,
            } => (*record_id, &empty_fields, Some(*revision)),
        };
        if (fields.is_empty() && !matches!(rule.change, AppRoundRecordChange::Delete { .. }))
            || fields.len() > 256
            || fields.keys().any(|field| !name(field))
        {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        // Model content may fill reviewed fields, but cannot choose the
        // record to overwrite or the optimistic-concurrency revision.
        let identities = values.sources(std::iter::once(record_id).chain(revision))?;
        if identities.contains(&AppRoundValueSource::Model) {
            return Err(AppRoundProgramError::InvalidDependency);
        }
        if let Some(each) = rule.for_each {
            values.require_pre_model([each])?;
        }
        let all = values.sources(fields.values().copied().chain(rule.when))?;
        if final_phase && all.contains(&AppRoundValueSource::Model) {
            return Err(AppRoundProgramError::InvalidDependency);
        }
    }
    Ok(())
}

pub(super) fn query_parameters(
    query: &AppRoundContextQuery,
    values: &AppRoundPreparedValues,
) -> Result<BTreeMap<String, Value>, AppRoundProgramError> {
    let mut result = serde_json::to_value(&query.parameters)
        .map_err(|_| AppRoundProgramError::InvalidProgram)?;
    for (pointer, id) in &query.bindings {
        let value = values.get(*id)?;
        let scalar = |value: &Value| !value.is_object() && !value.is_array();
        if if pointer.ends_with("/values") {
            value.as_array().is_none_or(|values| {
                values.is_empty() || values.len() > 256 || !values.iter().all(scalar)
            })
        } else {
            !scalar(value)
        } {
            return Err(AppRoundProgramError::InvalidValue);
        }
        *result
            .pointer_mut(pointer)
            .ok_or(AppRoundProgramError::InvalidProgram)? = value.clone();
    }
    let mut result: BTreeMap<String, Value> =
        serde_json::from_value(result).map_err(|_| AppRoundProgramError::InvalidProgram)?;
    result.insert("entity".to_owned(), json!(query.entity));
    Ok(result)
}

pub(super) fn plan_app_mutations(
    values: &AppRoundValueProgram,
    rules: &[AppRoundMutationRule],
    environment: &AppRoundValueEnvironment<'_>,
    max_mutations: u16,
) -> Result<AppRoundMutationPlan, AppRoundProgramError> {
    let outer = values.evaluate(environment)?;
    let mut plan = AppRoundMutationPlan {
        operations: vec![],
        expected_record_revisions: vec![],
        semantic_record_ids: vec![],
    };
    let mut seen = BTreeSet::new();
    for (rule_index, rule) in rules.iter().enumerate() {
        let items: Vec<Option<&Value>> = match rule.for_each {
            Some(each) => outer
                .get(each)?
                .as_array()
                .ok_or(AppRoundProgramError::InvalidValue)?
                .iter()
                .map(Some)
                .collect(),
            None => vec![None],
        };
        for (item_index, item) in items.into_iter().enumerate() {
            let values = values.evaluate(&AppRoundValueEnvironment {
                item,
                ..*environment
            })?;
            if rule.when.is_some_and(|when| !values.condition(when)) {
                continue;
            }
            if plan.operations.len() >= usize::from(max_mutations) {
                return Err(AppRoundProgramError::ByteBudget);
            }
            let (record_id, operation) = match &rule.change {
                AppRoundRecordChange::Create { record_id, fields } => {
                    let record_id = identity(values.get(*record_id)?)?.to_owned();
                    let operation = json!({"kind":"create","entity":rule.entity,"temporary_id":format!("round_{rule_index}_{item_index}"),"record_id":record_id,"payload":values.fields(fields)?});
                    (record_id, operation)
                },
                AppRoundRecordChange::Update {
                    record_id,
                    revision,
                    fields,
                } => {
                    let record_id = identity(values.get(*record_id)?)?.to_owned();
                    let revision = values
                        .get(*revision)?
                        .as_u64()
                        .filter(|value| *value > 0)
                        .ok_or(AppRoundProgramError::InvalidValue)?;
                    plan.expected_record_revisions.push(
                        json!({"entity":rule.entity,"record_id":record_id,"revision":revision}),
                    );
                    let operation = json!({"kind":"update","entity":rule.entity,"record_id":record_id,"patch":values.fields(fields)?});
                    (record_id, operation)
                },
                AppRoundRecordChange::Delete {
                    record_id,
                    revision,
                } => {
                    let record_id = identity(values.get(*record_id)?)?.to_owned();
                    let revision = values
                        .get(*revision)?
                        .as_u64()
                        .filter(|value| *value > 0)
                        .ok_or(AppRoundProgramError::InvalidValue)?;
                    plan.expected_record_revisions.push(
                        json!({"entity":rule.entity,"record_id":record_id,"revision":revision}),
                    );
                    let operation =
                        json!({"kind":"delete","entity":rule.entity,"record_id":record_id});
                    (record_id, operation)
                },
            };
            if !seen.insert((rule.entity.clone(), record_id.clone())) {
                return Err(AppRoundProgramError::InvalidValue);
            }
            if rule.semantic_result {
                plan.semantic_record_ids
                    .push(format!("{}:{record_id}", rule.entity));
            }
            plan.operations.push(operation);
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::super::contextual_round::AppRoundUsage;
    use super::super::contextual_round_program::AppRoundValueExpression::*;
    use super::*;
    use chrono::{DateTime, Utc};

    fn id(value: u16) -> AppRoundValueId {
        AppRoundValueId(value)
    }

    #[test]
    fn shared_cursor_and_reserved_context_names_cannot_select_a_participant() {
        let mut declared = declaration();
        declared.resume_after = Some(id(10)); // shared persisted context
        declared.validate().unwrap();
        for dependent in [id(0), id(1), id(17)] {
            declared.resume_after = Some(dependent);
            assert_eq!(
                declared.validate(),
                Err(AppRoundProgramError::InvalidDependency),
                "a shared cursor cannot use participant, model or item data"
            );
        }
        declared.resume_after = None;
        for reserved in ["participant", "round"] {
            declared.queries[0].name = reserved.to_owned();
            assert_eq!(
                declared.validate(),
                Err(AppRoundProgramError::InvalidProgram)
            );
        }
    }

    fn declaration() -> AppRoundProgramDeclaration {
        AppRoundProgramDeclaration {
            context_mode: AppRoundContextMode::Snapshot,
            values: AppRoundValueProgram {
                expressions: vec![
                    ParticipantId, // 0
                    Read {
                        source: AppRoundValueSource::Model,
                        pointer: "/kind".into(),
                    },
                    Literal {
                        value: json!("draft"),
                    },
                    Equal {
                        left: id(1),
                        right: id(2),
                    },
                    Literal {
                        value: json!("quiet"),
                    },
                    Equal {
                        left: id(1),
                        right: id(4),
                    },
                    Read {
                        source: AppRoundValueSource::Model,
                        pointer: "/reason".into(),
                    },
                    RunId,
                    StableId {
                        prefix: "entry".into(),
                        parts: vec![id(0), id(7)],
                    },
                    Read {
                        source: AppRoundValueSource::Model,
                        pointer: "/body".into(),
                    },
                    Read {
                        source: AppRoundValueSource::Context,
                        pointer: "/record/record_id".into(),
                    },
                    Read {
                        source: AppRoundValueSource::Context,
                        pointer: "/record/record_revision".into(),
                    },
                    Read {
                        source: AppRoundValueSource::Context,
                        pointer: "/record/fields/count".into(),
                    },
                    Literal { value: json!(1) },
                    Add {
                        left: id(12),
                        right: id(13),
                    },
                    Literal { value: json!(true) },
                    Read {
                        source: AppRoundValueSource::Context,
                        pointer: "/items".into(),
                    },
                    Read {
                        source: AppRoundValueSource::Item,
                        pointer: "/record_id".into(),
                    },
                    Read {
                        source: AppRoundValueSource::Item,
                        pointer: "/record_revision".into(),
                    },
                ],
            },
            participant_id_pointer: "/agent_id".into(),
            resume_after: None,
            queries: vec![AppRoundContextQuery {
                refresh_before_dispatch: false,
                name: "member".into(),
                entity: "member".into(),
                per_participant: true,
                parameters: BTreeMap::from([
                    ("select".into(), json!(["member_id", "enrolled"])),
                    ("limit".into(), json!(1)),
                    (
                        "predicate".into(),
                        json!({"root":0,"nodes":[{"kind":"compare","field":"member_id","operator":"equal","value":null}]}),
                    ),
                ]),
                bindings: BTreeMap::from([("/predicate/nodes/0/value".into(), id(0))]),
            }],
            eligibility: vec![AppRoundEligibilityRule {
                require: id(15),
                reason: "disabled".into(),
            }],
            semantic_step: "compose".into(),
            max_output_tokens: 50,
            limits: AppRoundLimits {
                max_participants: 32,
                max_concurrent: 4,
                max_attempts_per_participant: 2,
                aggregate: AppRoundUsage {
                    tokens: 10000,
                    micro_usd: 10000,
                },
                per_participant: AppRoundUsage {
                    tokens: 100,
                    micro_usd: 100,
                },
            },
            draft_when: id(3),
            quiet_when: id(5),
            quiet_reason: id(6),
            mutations: vec![AppRoundMutationRule {
                entity: "entry".into(),
                for_each: None,
                when: Some(id(3)),
                semantic_result: true,
                change: AppRoundRecordChange::Create {
                    record_id: id(8),
                    fields: BTreeMap::from([("author_id".into(), id(0)), ("body".into(), id(9))]),
                },
            }],
            final_mutations: vec![],
            max_mutations_per_participant: 64,
        }
    }
    fn environment<'a>(
        model: Option<&'a Value>,
        context: &'a Value,
        participant_id: &'a str,
    ) -> AppRoundValueEnvironment<'a> {
        AppRoundValueEnvironment {
            input: &Value::Null,
            participant: &Value::Null,
            context,
            model,
            item: None,
            now: DateTime::parse_from_rfc3339("2026-09-09T15:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            run_id: "round-one",
            participant_id,
        }
    }

    #[test]
    fn query_bindings_cannot_change_authority_or_depend_on_a_model() {
        let program = declaration();
        program.validate().unwrap();
        let values = program
            .values
            .evaluate(&environment(None, &Value::Null, "agent-a"))
            .unwrap();
        let parameters = program
            .query_parameters(&program.queries[0], &values)
            .unwrap();
        assert_eq!(parameters["entity"], "member");
        assert_eq!(parameters["predicate"]["nodes"][0]["value"], "agent-a");
        for pointer in [
            "/entity",
            "/predicate/nodes/0/field",
            "/predicate/nodes/0/operator",
        ] {
            let mut invalid = program.clone();
            invalid.queries[0].bindings = BTreeMap::from([(pointer.into(), id(0))]);
            assert!(invalid.validate().is_err());
        }
        let mut model_bound = program.clone();
        model_bound.queries[0]
            .bindings
            .values_mut()
            .for_each(|value| *value = id(9));
        assert_eq!(
            model_bound.validate(),
            Err(AppRoundProgramError::InvalidDependency)
        );
        let mut falsely_shared = program;
        falsely_shared.queries[0].per_participant = false;
        assert_eq!(
            falsely_shared.validate(),
            Err(AppRoundProgramError::InvalidDependency)
        );
    }

    #[test]
    fn separate_authors_get_separate_plans_and_model_cannot_choose_an_overwrite_target() {
        let program = declaration();
        let model = json!({"kind":"draft","body":"Same source topic, independently composed."});
        let a = program
            .plan_mutations(false, &environment(Some(&model), &Value::Null, "a"))
            .unwrap();
        let b = program
            .plan_mutations(false, &environment(Some(&model), &Value::Null, "b"))
            .unwrap();
        assert_eq!(a.operations.len(), 1);
        assert_eq!(b.operations.len(), 1);
        assert_ne!(a.operations[0]["record_id"], b.operations[0]["record_id"]);
        assert_eq!(a.operations[0]["payload"]["author_id"], "a");
        assert_eq!(b.operations[0]["payload"]["author_id"], "b");
        assert_eq!(a.semantic_record_ids.len(), 1);
        let mut invalid = program;
        if let AppRoundRecordChange::Create { record_id, .. } = &mut invalid.mutations[0].change {
            *record_id = id(9);
        }
        assert_eq!(
            invalid.validate(),
            Err(AppRoundProgramError::InvalidDependency)
        );
    }

    #[test]
    fn quiet_bookkeeping_never_counts_as_a_semantic_result() {
        let mut program = declaration();
        program.mutations.push(AppRoundMutationRule {
            entity: "state".into(),
            for_each: None,
            when: None,
            semantic_result: false,
            change: AppRoundRecordChange::Update {
                record_id: id(10),
                revision: id(11),
                fields: BTreeMap::from([("count".into(), id(14))]),
            },
        });
        let model = json!({"kind":"quiet","reason":"nothing_to_add"});
        let context =
            json!({"record":{"record_id":"state-a","record_revision":7,"fields":{"count":2}}});
        let env = environment(Some(&model), &context, "a");
        let values = program.values.evaluate(&env).unwrap();
        assert_eq!(
            program.semantic_outcome(&values).unwrap(),
            AppRoundSemanticOutcome::Quiet("nothing_to_add".into())
        );
        let plan = program.plan_mutations(false, &env).unwrap();
        assert_eq!(plan.operations.len(), 1);
        assert!(plan.semantic_record_ids.is_empty());
        assert_eq!(plan.operations[0]["patch"]["count"], 3);
        assert_eq!(plan.expected_record_revisions[0]["revision"], 7);
        let malformed = json!({"kind":"draft-or-quiet"});
        let values = program
            .values
            .evaluate(&environment(Some(&malformed), &context, "a"))
            .unwrap();
        assert_eq!(
            program.semantic_outcome(&values),
            Err(AppRoundProgramError::InvalidValue)
        );
    }

    #[test]
    fn per_item_updates_keep_revisions_and_refuse_duplicates_or_partial_overflow() {
        let mut program = declaration();
        program.mutations = vec![AppRoundMutationRule {
            entity: "pending".into(),
            for_each: Some(id(16)),
            when: None,
            semantic_result: false,
            change: AppRoundRecordChange::Update {
                record_id: id(17),
                revision: id(18),
                fields: BTreeMap::from([("handled".into(), id(15))]),
            },
        }];
        let context = json!({"items":[{"record_id":"one","record_revision":3},{"record_id":"two","record_revision":9}]});
        let plan = program
            .plan_mutations(false, &environment(None, &context, "a"))
            .unwrap();
        assert_eq!(plan.operations.len(), 2);
        assert_eq!(plan.expected_record_revisions[1]["revision"], 9);
        let duplicate = json!({"items":[{"record_id":"one","record_revision":3},{"record_id":"one","record_revision":3}]});
        assert!(program
            .plan_mutations(false, &environment(None, &duplicate, "a"))
            .is_err());
        program.max_mutations_per_participant = 1;
        assert!(program
            .plan_mutations(false, &environment(None, &context, "a"))
            .is_err());
    }
}
