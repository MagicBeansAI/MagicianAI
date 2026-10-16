//! Deterministic, reviewed App-owned record transactions.
//!
//! The declaration can bind ordinary own-store reads to input scalars and map
//! their results into exact record operations. It cannot call a model, select a
//! capability or change the authenticated scope. The workflow/entity owners
//! still authorize every read, revision fence and atomic commit.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::contextual_round_declaration::{
    plan_app_mutations, query_parameters, validate_context_queries, validate_mutation_rules,
    AppRoundContextQuery, AppRoundEligibilityRule, AppRoundMutationPlan, AppRoundMutationRule,
};
use super::contextual_round_program::{
    AppRoundPreparedValues, AppRoundProgramError, AppRoundValueEnvironment, AppRoundValueId,
    AppRoundValueProgram, AppRoundValueSource,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppStoreTransactionDeclaration {
    pub values: AppRoundValueProgram,
    pub queries: Vec<AppRoundContextQuery>,
    /// Queries whose complete cursor stream is required. Other queries are
    /// intentionally bounded windows (for example the newest feed entries).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub scan_queries: BTreeSet<String>,
    /// An absent optional input can suppress its lookup. The host supplies an
    /// empty window for a false condition; unknown/non-boolean conditions fail.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query_when: BTreeMap<String, AppRoundValueId>,
    /// Input-only overrides for already declared public scalar host parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_parameters: BTreeMap<String, BTreeMap<String, AppRoundValueId>>,
    /// Bounded, deterministic source-page metadata retained with the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<AppRoundValueId>,
    pub guards: Vec<AppRoundEligibilityRule>,
    pub mutations: Vec<AppRoundMutationRule>,
    pub max_mutations: u16,
    /// Each own-store request retains its ordinary page limit and cursor.
    /// This is the reviewed per-query scan budget, not an App record limit.
    pub max_query_pages: u16,
}

impl AppStoreTransactionDeclaration {
    pub fn validate(&self) -> Result<(), AppRoundProgramError> {
        self.values.validate()?;
        if self.max_mutations == 0
            || self.max_mutations > 256
            || self.max_query_pages == 0
            || self.queries.len() > 64
            || self.guards.len() > 64
            || self.mutations.len() > 256
            || self.queries.iter().any(|query| query.per_participant)
            || self
                .scan_queries
                .iter()
                .any(|name| !self.queries.iter().any(|query| &query.name == name))
        {
            return Err(AppRoundProgramError::InvalidProgram);
        }
        let sources = self.values.sources(
            (0..self.values.expressions.len()).map(|index| AppRoundValueId(index as u16)),
        )?;
        if sources.contains(&AppRoundValueSource::Model)
            || sources.contains(&AppRoundValueSource::Participant)
        {
            return Err(AppRoundProgramError::InvalidDependency);
        }
        validate_context_queries(&self.values, &self.queries, true)?;
        validate_mutation_rules(&self.values, &self.mutations, true)?;
        for (name, condition) in &self.query_when {
            if !self.queries.iter().any(|query| &query.name == name) {
                return Err(AppRoundProgramError::InvalidProgram);
            }
            if self
                .values
                .sources([*condition])?
                .contains(&AppRoundValueSource::Item)
            {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
        for parameters in self.source_parameters.values() {
            if parameters.len() > 16
                || self
                    .values
                    .sources(parameters.values().copied())?
                    .iter()
                    .any(|source| *source != AppRoundValueSource::Input)
            {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
        if let Some(summary) = self.summary {
            if self
                .values
                .sources([summary])?
                .contains(&AppRoundValueSource::Item)
            {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
        for guard in &self.guards {
            if guard.reason.is_empty()
                || guard.reason.len() > 64
                || !guard
                    .reason
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(AppRoundProgramError::InvalidProgram);
            }
            let sources = self.values.sources([guard.require])?;
            if sources.contains(&AppRoundValueSource::Item) {
                return Err(AppRoundProgramError::InvalidDependency);
            }
        }
        Ok(())
    }

    pub fn summary(&self, values: &AppRoundPreparedValues) -> Result<String, AppRoundProgramError> {
        match self.summary {
            None => Ok("Applied the reviewed App record operation.".to_owned()),
            Some(id) => values
                .get(id)?
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= 4096)
                .map(str::to_owned)
                .ok_or(AppRoundProgramError::InvalidValue),
        }
    }

    pub fn query_parameters(
        &self,
        query: &AppRoundContextQuery,
        values: &AppRoundPreparedValues,
    ) -> Result<BTreeMap<String, Value>, AppRoundProgramError> {
        query_parameters(query, values)
    }

    pub fn query_enabled(
        &self,
        name: &str,
        values: &AppRoundPreparedValues,
    ) -> Result<bool, AppRoundProgramError> {
        match self.query_when.get(name) {
            None => Ok(true),
            Some(condition) => values
                .get(*condition)?
                .as_bool()
                .ok_or(AppRoundProgramError::InvalidValue),
        }
    }

    pub fn refusal(&self, values: &AppRoundPreparedValues) -> Option<&str> {
        self.guards
            .iter()
            .find(|guard| !values.condition(guard.require))
            .map(|guard| guard.reason.as_str())
    }

    pub fn plan(
        &self,
        environment: &AppRoundValueEnvironment<'_>,
    ) -> Result<AppRoundMutationPlan, AppRoundProgramError> {
        self.validate()?;
        let values = self.values.evaluate(environment)?;
        if self.refusal(&values).is_some() {
            return Err(AppRoundProgramError::InvalidValue);
        }
        plan_app_mutations(
            &self.values,
            &self.mutations,
            environment,
            self.max_mutations,
        )
    }

    pub fn mutation_entities(&self) -> BTreeSet<&str> {
        self.mutations
            .iter()
            .map(|rule| rule.entity.as_str())
            .collect()
    }
}
