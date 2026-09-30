//! Stable, content-free relation registry for governed LLM analytics reads.
//!
//! The registry is deliberately independent of REST and tool schemas. Those
//! surfaces consume these exact relation/column contracts in Phase 2E instead
//! of each constructing a subtly different DuckDB view.

use std::{collections::BTreeMap, path::PathBuf};

use magicllm::LlmScope;
use serde::{Deserialize, Serialize};

use super::llm_trace_materializer::FACT_COLUMNS;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const LLM_FACT_REGISTRY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCanonicalDataset {
    Calls,
    ProviderAttempts,
    ToolCalls,
    CaptureGaps,
}

impl LlmCanonicalDataset {
    pub const ALL: [Self; 4] = [
        Self::Calls,
        Self::ProviderAttempts,
        Self::ToolCalls,
        Self::CaptureGaps,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "llm_calls",
            Self::ProviderAttempts => "llm_provider_attempts",
            Self::ToolCalls => "llm_tool_calls",
            Self::CaptureGaps => "llm_capture_gaps",
        }
    }

    pub const fn record_kind(self) -> &'static str {
        match self {
            Self::Calls => "call_fact",
            Self::ProviderAttempts => "provider_attempt",
            Self::ToolCalls => "tool_lineage",
            Self::CaptureGaps => "capture_gap",
        }
    }

    pub fn raw_file_prefix(self) -> String {
        format!("part-{}-", self.record_kind())
    }

    pub fn root(self, workspace: &ArtifactV2Workspace, scope: &LlmScope) -> PathBuf {
        match self {
            Self::Calls => workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace),
            Self::ProviderAttempts => {
                workspace.analytics_llm_provider_attempts_root(&scope.principal, &scope.workspace)
            },
            Self::ToolCalls => {
                workspace.analytics_llm_tool_calls_root(&scope.principal, &scope.workspace)
            },
            Self::CaptureGaps => {
                workspace.analytics_llm_capture_gaps_root(&scope.principal, &scope.workspace)
            },
        }
    }

    pub const fn revision_relation(self) -> LlmFactRelation {
        match self {
            Self::Calls => LlmFactRelation::CallRevisions,
            Self::ProviderAttempts => LlmFactRelation::ProviderAttemptRevisions,
            Self::ToolCalls => LlmFactRelation::ToolCallRevisions,
            Self::CaptureGaps => LlmFactRelation::CaptureGaps,
        }
    }

    pub const fn stable_relation(self) -> LlmFactRelation {
        match self {
            Self::Calls => LlmFactRelation::Calls,
            Self::ProviderAttempts => LlmFactRelation::ProviderAttempts,
            Self::ToolCalls => LlmFactRelation::ToolCalls,
            Self::CaptureGaps => LlmFactRelation::CaptureGaps,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFactRelation {
    Calls,
    ProviderAttempts,
    ToolCalls,
    CaptureGaps,
    CallRevisions,
    ProviderAttemptRevisions,
    ToolCallRevisions,
}

impl LlmFactRelation {
    pub const ALL: [Self; 7] = [
        Self::Calls,
        Self::ProviderAttempts,
        Self::ToolCalls,
        Self::CaptureGaps,
        Self::CallRevisions,
        Self::ProviderAttemptRevisions,
        Self::ToolCallRevisions,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "llm_calls",
            Self::ProviderAttempts => "llm_provider_attempts",
            Self::ToolCalls => "llm_tool_calls",
            Self::CaptureGaps => "llm_capture_gaps",
            Self::CallRevisions => "llm_call_revisions",
            Self::ProviderAttemptRevisions => "llm_provider_attempt_revisions",
            Self::ToolCallRevisions => "llm_tool_call_revisions",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|relation| relation.as_str() == value)
    }

    pub const fn is_stable(self) -> bool {
        matches!(
            self,
            Self::Calls | Self::ProviderAttempts | Self::ToolCalls | Self::CaptureGaps
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFactContentClass {
    FactOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactColumnDefinition {
    pub name: String,
    pub duckdb_type: String,
    pub nullable: bool,
    pub unit: Option<String>,
    pub content_class: LlmFactContentClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactDefinition {
    pub relation: LlmFactRelation,
    pub relation_name: String,
    pub stable: bool,
    pub content_class: LlmFactContentClass,
    pub source_dataset: LlmCanonicalDataset,
    pub description: String,
    pub columns: Vec<LlmFactColumnDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactRegistry {
    pub schema_version: u16,
    pub facts: BTreeMap<String, LlmFactDefinition>,
}

impl Default for LlmFactRegistry {
    fn default() -> Self {
        Self::canonical()
    }
}

impl LlmFactRegistry {
    pub fn canonical() -> Self {
        let columns = canonical_columns();
        let mut facts = BTreeMap::new();
        for relation in LlmFactRelation::ALL {
            let source_dataset = match relation {
                LlmFactRelation::Calls | LlmFactRelation::CallRevisions => {
                    LlmCanonicalDataset::Calls
                },
                LlmFactRelation::ProviderAttempts | LlmFactRelation::ProviderAttemptRevisions => {
                    LlmCanonicalDataset::ProviderAttempts
                },
                LlmFactRelation::ToolCalls | LlmFactRelation::ToolCallRevisions => {
                    LlmCanonicalDataset::ToolCalls
                },
                LlmFactRelation::CaptureGaps => LlmCanonicalDataset::CaptureGaps,
            };
            let description = match relation {
                LlmFactRelation::Calls => {
                    "One coalesced row per logical LLM call, enriched with its terminal provider attempt"
                },
                LlmFactRelation::ProviderAttempts => {
                    "One coalesced row per physical provider attempt"
                },
                LlmFactRelation::ToolCalls => {
                    "Immutable validated lifecycle stages for model-proposed tool executions"
                },
                LlmFactRelation::CaptureGaps => {
                    "Explicit content-free evidence for known missing trace facts"
                },
                LlmFactRelation::CallRevisions => {
                    "Immutable call-start and call-completion source revisions"
                },
                LlmFactRelation::ProviderAttemptRevisions => {
                    "Immutable attempt-start, first-token and completion source revisions"
                },
                LlmFactRelation::ToolCallRevisions => {
                    "Immutable source revisions for tool execution, consumption, branch and rollback lineage"
                },
            };
            let definition = LlmFactDefinition {
                relation,
                relation_name: relation.as_str().to_string(),
                stable: relation.is_stable(),
                content_class: LlmFactContentClass::FactOnly,
                source_dataset,
                description: description.to_string(),
                columns: columns.clone(),
            };
            facts.insert(relation.as_str().to_string(), definition);
        }
        Self {
            schema_version: LLM_FACT_REGISTRY_SCHEMA_VERSION,
            facts,
        }
    }

    pub fn resolve(&self, relation: LlmFactRelation) -> &LlmFactDefinition {
        self.facts
            .get(relation.as_str())
            .expect("canonical LLM fact relation is always registered")
    }

    pub fn resolve_name(&self, relation: &str) -> Option<&LlmFactDefinition> {
        self.facts.get(relation)
    }

    pub fn allows_column(&self, relation: LlmFactRelation, column: &str) -> bool {
        self.resolve(relation)
            .columns
            .iter()
            .any(|definition| definition.name == column)
    }
}

fn canonical_columns() -> Vec<LlmFactColumnDefinition> {
    FACT_COLUMNS
        .iter()
        .map(|(name, ty)| LlmFactColumnDefinition {
            name: (*name).to_string(),
            duckdb_type: (*ty).to_string(),
            nullable: !matches!(
                *name,
                "materialized_schema_version"
                    | "fact_schema_version"
                    | "journal_schema_version"
                    | "journal_sequence"
                    | "record_kind"
                    | "stable_id"
                    | "record_revision"
                    | "lifecycle_phase"
                    | "idempotency_key"
                    | "payload_checksum"
                    | "occurred_at_ms"
                    | "observed_at_ms"
                    | "timestamp_ms"
                    | "principal"
                    | "workspace"
                    | "operation"
                    | "capture_mode"
                    | "capture_status"
                    | "training_eligible_at_capture"
            ),
            unit: column_unit(name).map(str::to_string),
            content_class: LlmFactContentClass::FactOnly,
        })
        .collect()
}

fn column_unit(name: &str) -> Option<&'static str> {
    if name.ends_with("_at_ms") || name.ends_with("timestamp_ms") {
        Some("unix_milliseconds")
    } else if name.ends_with("_ms") {
        Some("milliseconds")
    } else if name.ends_with("_tokens") {
        Some("tokens")
    } else if name.ends_with("_cost_usd") || name == "cost_usd" {
        Some("usd")
    } else {
        None
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique_exact_and_content_free() {
        let registry = LlmFactRegistry::canonical();
        assert_eq!(registry.facts.len(), LlmFactRelation::ALL.len());
        for relation in LlmFactRelation::ALL {
            let definition = registry.resolve(relation);
            assert_eq!(definition.relation_name, relation.as_str());
            assert_eq!(definition.content_class, LlmFactContentClass::FactOnly);
            assert_eq!(definition.columns.len(), FACT_COLUMNS.len());
            for forbidden in [
                "prompt",
                "response",
                "messages",
                "tool_arguments",
                "tool_results",
                "attachments",
                "transcript",
            ] {
                assert!(!registry.allows_column(relation, forbidden));
            }
        }
    }

    #[test]
    fn registry_rejects_unknown_relations_and_columns() {
        let registry = LlmFactRegistry::canonical();
        assert!(registry.resolve_name("events").is_none());
        assert!(LlmFactRelation::parse("llm_calls; DROP TABLE events").is_none());
        assert!(!registry.allows_column(LlmFactRelation::Calls, "secret_payload"));
    }

    #[test]
    fn units_distinguish_timestamps_durations_tokens_and_cost() {
        let registry = LlmFactRegistry::canonical();
        let columns = &registry.resolve(LlmFactRelation::Calls).columns;
        let unit = |name: &str| {
            columns
                .iter()
                .find(|column| column.name == name)
                .and_then(|column| column.unit.as_deref())
        };
        assert_eq!(unit("completed_at_ms"), Some("unix_milliseconds"));
        assert_eq!(unit("latency_ms"), Some("milliseconds"));
        assert_eq!(unit("input_tokens"), Some("tokens"));
        assert_eq!(unit("cost_usd"), Some("usd"));
    }
}
