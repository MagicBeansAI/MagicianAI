//! Declarative memory tier schemas for TRUE_AGENTS Phase 2.
//!
//! This module defines data contracts only. Consolidation execution and rendering
//! interpreters are deferred to Phase 3+.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory_record::V3MemoryTierRecord;

/// A memory tier definition, loaded from agent YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryTierDefinition {
    pub name: String,
    pub scope: TierScope,
    pub description: String,
    /// Ordered, not hashed, because this map is **serialized**.
    ///
    /// `std::collections::HashMap` iterates in an order derived from a
    /// per-process random seed, so serializing the same tier twice in two
    /// processes produced different bytes. Every byte-level comparison of a
    /// structure containing a tier therefore failed at random. One did:
    /// `read_task_binding` checks a sealed app-workflow sidecar against the
    /// registry's stored blob to catch format drift, and that check could not
    /// tell drift from a fresh hash seed — so every app workflow bound to an
    /// agent with tier schemas reported `CorruptBinding` and never launched.
    /// (Content digests were unaffected: they route through
    /// `canonical_json_bytes`, which sorts keys.)
    #[serde(default)]
    pub schema: BTreeMap<String, TierFieldSchema>,
    pub render: RenderConfig,
    pub retention: RetentionMode,
}

/// Scope determines storage path and lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TierScope {
    Agent,
    AgentGoal,
    User,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TierFieldSchema {
    Text {},
    Document {},
    DateTime {},
    KeyValueList {},
    Collection {
        #[serde(default)]
        max_items: Option<usize>,
        /// Ordered for the same reason as `MemoryTierDefinition::schema`.
        #[serde(default)]
        item_schema: Option<BTreeMap<String, TierFieldSchema>>,
    },
}

impl<'de> Deserialize<'de> for TierFieldSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum RawTierFieldSchema {
            Text {
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            Document {
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            DateTime {
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            KeyValueList {
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            Collection {
                #[serde(default)]
                max_items: Option<usize>,
                #[serde(default)]
                item_schema: Option<BTreeMap<String, TierFieldSchema>>,
                // `extra` stays a HashMap: it is deserialize-only scratch for
                // rejecting unknown fields and is never serialized.
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
        }

        fn reject_unknown_fields<E>(variant: &str, extra: HashMap<String, Value>) -> Result<(), E>
        where
            E: serde::de::Error,
        {
            if extra.is_empty() {
                return Ok(());
            }

            let mut keys = extra.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            Err(E::custom(format!(
                "unknown field(s) for tier schema variant `{variant}`: {}",
                keys.join(", ")
            )))
        }

        match RawTierFieldSchema::deserialize(deserializer)? {
            RawTierFieldSchema::Text { extra } => {
                reject_unknown_fields::<D::Error>("text", extra)?;
                Ok(Self::Text {})
            },
            RawTierFieldSchema::Document { extra } => {
                reject_unknown_fields::<D::Error>("document", extra)?;
                Ok(Self::Document {})
            },
            RawTierFieldSchema::DateTime { extra } => {
                reject_unknown_fields::<D::Error>("date_time", extra)?;
                Ok(Self::DateTime {})
            },
            RawTierFieldSchema::KeyValueList { extra } => {
                reject_unknown_fields::<D::Error>("key_value_list", extra)?;
                Ok(Self::KeyValueList {})
            },
            RawTierFieldSchema::Collection {
                max_items,
                item_schema,
                extra,
            } => {
                reject_unknown_fields::<D::Error>("collection", extra)?;
                Ok(Self::Collection {
                    max_items,
                    item_schema,
                })
            },
        }
    }
}

/// How a tier is rendered as natural text for the LLM prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    pub format: String,
    pub template: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionMode {
    GoalLifetime,
    Forever,
    Days(u32),
}

impl<'de> Deserialize<'de> for RetentionMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Preserve the derive-equivalent representation for binary formats;
        // compatibility is needed only for human-authored YAML/JSON stores.
        if !deserializer.is_human_readable() {
            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case")]
            enum CanonicalRetentionMode {
                GoalLifetime,
                Forever,
                Days(u32),
            }

            return CanonicalRetentionMode::deserialize(deserializer).map(|mode| match mode {
                CanonicalRetentionMode::GoalLifetime => Self::GoalLifetime,
                CanonicalRetentionMode::Forever => Self::Forever,
                CanonicalRetentionMode::Days(days) => Self::Days(days),
            });
        }

        // `RetentionMode` has historically been persisted in two forms. The
        // canonical representation is the externally-tagged enum (`!days 14`
        // in YAML), while definitions written before the tagged schema used a
        // bare integer (`14`). Accept the legacy representation on read so one
        // older agent definition cannot prevent the entire scoped catalog from
        // hydrating. Serialization remains canonical via the derive above.
        use serde::de::{EnumAccess, Error as _, MapAccess, VariantAccess, Visitor};
        use std::fmt;

        struct RetentionModeVisitor;

        impl RetentionModeVisitor {
            fn days_from_u64<E>(days: u64) -> Result<RetentionMode, E>
            where
                E: serde::de::Error,
            {
                u32::try_from(days)
                    .map(RetentionMode::Days)
                    .map_err(|_| E::custom("retention `days` exceeds the supported u32 range"))
            }

            fn from_name<E>(mode: &str) -> Result<RetentionMode, E>
            where
                E: serde::de::Error,
            {
                match mode {
                    "goal_lifetime" => Ok(RetentionMode::GoalLifetime),
                    "forever" => Ok(RetentionMode::Forever),
                    _ => Err(E::custom(
                        "retention must be `forever`, `goal_lifetime`, `!days N`, or a legacy integer",
                    )),
                }
            }
        }

        impl<'de> Visitor<'de> for RetentionModeVisitor {
            type Value = RetentionMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "`forever`, `goal_lifetime`, a tagged `days` value, or a legacy integer",
                )
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::days_from_u64(value)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let days = u64::try_from(value).map_err(|_| {
                    E::custom("retention `days` must contain a non-negative integer")
                })?;
                Self::days_from_u64(days)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::from_name(value)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::from_name(&value)
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: EnumAccess<'de>,
            {
                let (variant, payload) = data.variant::<String>()?;
                match variant.as_str() {
                    "days" => payload.newtype_variant::<u32>().map(RetentionMode::Days),
                    "goal_lifetime" => {
                        payload.unit_variant()?;
                        Ok(RetentionMode::GoalLifetime)
                    },
                    "forever" => {
                        payload.unit_variant()?;
                        Ok(RetentionMode::Forever)
                    },
                    _ => Err(A::Error::custom(format!(
                        "unknown retention variant `{variant}`"
                    ))),
                }
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let variant = map.next_key::<String>()?.ok_or_else(|| {
                    A::Error::custom("retention mapping must contain exactly one variant")
                })?;
                let result = match variant.as_str() {
                    "days" => map.next_value::<u32>().map(RetentionMode::Days),
                    _ => {
                        let _ = map.next_value::<serde::de::IgnoredAny>()?;
                        Err(A::Error::custom(format!(
                            "unknown retention variant `{variant}`"
                        )))
                    },
                }?;
                if map.next_key::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom(
                        "retention mapping must contain exactly one variant",
                    ));
                }
                Ok(result)
            }
        }

        deserializer.deserialize_any(RetentionModeVisitor)
    }
}

/// A memory consolidation rule loaded from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConsolidationRule {
    pub name: String,
    pub trigger: ConsolidationTrigger,
    pub source: String,
    pub target: String,
    pub transform: ConsolidationTransform,
}

#[derive(Debug, Clone)]
pub enum ConsolidationTrigger {
    CycleCompleted,
    /// Fires after each pipeline step completes (success only).
    /// Source data is the `StepResultContent` JSON from the just-completed step.
    StepCompleted,
    Batch {
        interval_hours: Option<u32>,
        interval_days: Option<u32>,
        min_episodes: Option<u32>,
        max_staleness_hours: Option<u32>,
    },
    RetentionExpiry,
}

impl Serialize for ConsolidationTrigger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::CycleCompleted => serializer.serialize_str("cycle_completed"),
            Self::StepCompleted => serializer.serialize_str("step_completed"),
            Self::RetentionExpiry => serializer.serialize_str("retention_expiry"),
            Self::Batch {
                interval_hours,
                interval_days,
                min_episodes,
                max_staleness_hours,
            } => {
                use serde::ser::SerializeMap;
                let mut outer = serializer.serialize_map(Some(1))?;
                // Build the inner payload as a map, omitting None fields.
                let mut inner = serde_json::Map::new();
                if let Some(v) = interval_hours {
                    inner.insert("interval_hours".into(), (*v).into());
                }
                if let Some(v) = interval_days {
                    inner.insert("interval_days".into(), (*v).into());
                }
                if let Some(v) = min_episodes {
                    inner.insert("min_episodes".into(), (*v).into());
                }
                if let Some(v) = max_staleness_hours {
                    inner.insert("max_staleness_hours".into(), (*v).into());
                }
                outer.serialize_entry("batch", &inner)?;
                outer.end()
            },
        }
    }
}

impl<'de> Deserialize<'de> for ConsolidationTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct BatchTriggerPayload {
            #[serde(default)]
            interval_hours: Option<u32>,
            #[serde(default)]
            interval_days: Option<u32>,
            #[serde(default)]
            min_episodes: Option<u32>,
            #[serde(default)]
            max_staleness_hours: Option<u32>,
        }

        fn ensure_unit_payload<D>(variant: &str, payload: Value) -> Result<(), D>
        where
            D: serde::de::Error,
        {
            match payload {
                Value::Null => Ok(()),
                Value::Object(map) if map.is_empty() => Ok(()),
                _ => Err(D::custom(format!(
                    "consolidation trigger `{variant}` does not accept parameters"
                ))),
            }
        }

        fn validate_batch_payload<D>(payload: &BatchTriggerPayload) -> Result<(), D>
        where
            D: serde::de::Error,
        {
            if payload.interval_hours.is_some_and(|v| v == 0) {
                return Err(D::custom(
                    "consolidation trigger `batch.interval_hours` must be > 0 when provided",
                ));
            }
            if payload.interval_days.is_some_and(|v| v == 0) {
                return Err(D::custom(
                    "consolidation trigger `batch.interval_days` must be > 0 when provided",
                ));
            }
            if payload.min_episodes.is_some_and(|v| v == 0) {
                return Err(D::custom(
                    "consolidation trigger `batch.min_episodes` must be > 0 when provided",
                ));
            }
            if payload.max_staleness_hours.is_some_and(|v| v == 0) {
                return Err(D::custom(
                    "consolidation trigger `batch.max_staleness_hours` must be > 0 when provided",
                ));
            }
            if payload.interval_hours.is_none()
                && payload.interval_days.is_none()
                && payload.min_episodes.is_none()
                && payload.max_staleness_hours.is_none()
            {
                return Err(D::custom(
                    "consolidation trigger `batch` requires at least one of interval_hours, interval_days, min_episodes, or max_staleness_hours",
                ));
            }
            Ok(())
        }

        let raw = Value::deserialize(deserializer)?;
        match raw {
            Value::String(unit) => match unit.as_str() {
                "cycle_completed" => Ok(Self::CycleCompleted),
                "step_completed" => Ok(Self::StepCompleted),
                "retention_expiry" => Ok(Self::RetentionExpiry),
                other => Err(serde::de::Error::custom(format!(
                    "consolidation trigger must be `cycle_completed`, `step_completed`, `retention_expiry`, or `batch`; got `{other}`"
                ))),
            },
            Value::Object(map) => {
                if map.len() != 1 {
                    return Err(serde::de::Error::custom(
                        "consolidation trigger must define exactly one variant key",
                    ));
                }
                let (variant, payload) = map.into_iter().next().ok_or_else(|| {
                    serde::de::Error::custom("consolidation trigger mapping must not be empty")
                })?;
                match variant.as_str() {
                    "batch" => {
                        let parsed: BatchTriggerPayload =
                            BatchTriggerPayload::deserialize(payload)
                                .map_err(serde::de::Error::custom)?;
                        validate_batch_payload::<D::Error>(&parsed)?;
                        Ok(Self::Batch {
                            interval_hours: parsed.interval_hours,
                            interval_days: parsed.interval_days,
                            min_episodes: parsed.min_episodes,
                            max_staleness_hours: parsed.max_staleness_hours,
                        })
                    },
                    "cycle_completed" => {
                        ensure_unit_payload::<D::Error>("cycle_completed", payload)?;
                        Ok(Self::CycleCompleted)
                    },
                    "step_completed" => {
                        ensure_unit_payload::<D::Error>("step_completed", payload)?;
                        Ok(Self::StepCompleted)
                    },
                    "retention_expiry" => {
                        ensure_unit_payload::<D::Error>("retention_expiry", payload)?;
                        Ok(Self::RetentionExpiry)
                    },
                    other => Err(serde::de::Error::custom(format!(
                        "consolidation trigger must be `cycle_completed`, `step_completed`, `retention_expiry`, or `batch`; got `{other}`"
                    ))),
                }
            },
            _ => Err(serde::de::Error::custom(
                "consolidation trigger must be a string or a single-key mapping",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsolidationTransform {
    Structured {
        builtin: BuiltinTransform,
    },
    Llm {
        prompt: String,
        /// Explicit operation used by the runtime router. Older definitions may
        /// omit this when their resolved system prompt carries an operation tag,
        /// but new inline transforms should set it so routing is not inferred
        /// from untrusted model instructions or a generic fallback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<MemoryConsolidationOperation>,
        #[serde(default)]
        system_prompt: Option<String>,
        #[serde(default)]
        merge: Option<MergeStrategy>,
    },
    Render {
        template: String,
    },
}

impl<'de> Deserialize<'de> for ConsolidationTransform {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum RawConsolidationTransform {
            Structured {
                builtin: BuiltinTransform,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            Llm {
                prompt: String,
                #[serde(default)]
                operation: Option<MemoryConsolidationOperation>,
                #[serde(default)]
                system_prompt: Option<String>,
                #[serde(default)]
                merge: Option<MergeStrategy>,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            Render {
                template: String,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
        }

        fn reject_unknown_fields<E>(variant: &str, extra: HashMap<String, Value>) -> Result<(), E>
        where
            E: serde::de::Error,
        {
            if extra.is_empty() {
                return Ok(());
            }

            let mut keys = extra.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            Err(E::custom(format!(
                "unknown field(s) for consolidation transform `{variant}`: {}",
                keys.join(", ")
            )))
        }

        match RawConsolidationTransform::deserialize(deserializer)? {
            RawConsolidationTransform::Structured { builtin, extra } => {
                reject_unknown_fields::<D::Error>("structured", extra)?;
                Ok(Self::Structured { builtin })
            },
            RawConsolidationTransform::Llm {
                prompt,
                operation,
                system_prompt,
                merge,
                extra,
            } => {
                reject_unknown_fields::<D::Error>("llm", extra)?;
                Ok(Self::Llm {
                    prompt,
                    operation,
                    system_prompt,
                    merge,
                })
            },
            RawConsolidationTransform::Render { template, extra } => {
                reject_unknown_fields::<D::Error>("render", extra)?;
                Ok(Self::Render { template })
            },
        }
    }
}

/// Router operations permitted for declarative memory-consolidation LLM
/// transforms. Keeping this enum in the schema crate makes invalid operation
/// names a definition-load error instead of silently routing them as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConsolidationOperation {
    MemoryEntityExtraction,
    MemoryEnvironmentKnowledgeExtraction,
    MemoryInsightDistillation,
    MemoryUserPromotion,
    MemoryArchiveSummary,
}

impl MemoryConsolidationOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemoryEntityExtraction => "memory_entity_extraction",
            Self::MemoryEnvironmentKnowledgeExtraction => "memory_environment_knowledge_extraction",
            Self::MemoryInsightDistillation => "memory_insight_distillation",
            Self::MemoryUserPromotion => "memory_user_promotion",
            Self::MemoryArchiveSummary => "memory_archive_summary",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinTransform {
    MapEpisodeToTask,
    AppendStrategyRecord,
    AppendArchiveSummary,
    PromoteSharedInsights,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    UpsertByName,
    UpsertByNamePerSource,
    UpsertBySimilarity,
    AppendPeriod,
}

/// Renders a memory tier as natural text for prompt injection.
/// Implementations are provided in Phase 3's interpreter layer.
pub trait MemoryRenderer: std::fmt::Debug {
    fn render(&self, tier: &MemoryTierDefinition, data: &V3MemoryTierRecord) -> String;
}

/// Parsed cross-tier source reference used by consolidation interpreters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTierRef {
    pub agent: Option<String>,
    pub tier_name: String,
}

/// Parsed consolidation source reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRef {
    Episodes {
        goal_id: Option<String>,
        limit: Option<usize>,
        unprocessed: bool,
    },
    Tiers {
        tier_refs: Vec<ParsedTierRef>,
    },
}

impl SourceRef {
    /// Parse a source expression into a typed contract.
    /// Supported forms:
    /// - `episodes(goal_id, limit=30)`
    /// - `episodes(unprocessed=true)`
    /// - `tiers(task_progress, web-scraper.price_tracking, team.alpha.task_progress)`
    pub fn parse(source: &str) -> Option<Self> {
        let source = source.trim();
        if let Some(inner) = source
            .strip_prefix("episodes(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let mut goal_id = None;
            let mut limit = None;
            let mut unprocessed = false;
            let mut saw_unprocessed = false;

            for token in inner.split(',').map(str::trim) {
                if token.is_empty() {
                    return None;
                }

                if let Some((key, value)) = token.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();
                    if key.is_empty() || value.is_empty() {
                        return None;
                    }

                    match key.trim() {
                        "limit" => {
                            if limit.is_some() {
                                return None;
                            }
                            let parsed = value.parse::<usize>().ok()?;
                            if parsed == 0 {
                                return None;
                            }
                            limit = Some(parsed);
                        },
                        "unprocessed" => {
                            if saw_unprocessed {
                                return None;
                            }
                            let parsed = value.parse::<bool>().ok()?;
                            unprocessed = parsed;
                            saw_unprocessed = true;
                        },
                        "goal_id" => {
                            if goal_id.is_some() {
                                return None;
                            }
                            goal_id = Some(value.to_string());
                        },
                        _ => return None,
                    }
                    continue;
                }

                if goal_id.is_some() {
                    return None;
                }
                if !token.is_empty() {
                    goal_id = Some(token.to_string());
                }
            }

            return Some(Self::Episodes {
                goal_id,
                limit,
                unprocessed,
            });
        }

        if let Some(inner) = source
            .strip_prefix("tiers(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let mut tier_refs = Vec::new();
            for token in inner.split(',').map(str::trim) {
                if token.is_empty() {
                    return None;
                }

                // Parse `agent_id.tier_name` from the last dot so dotted agent IDs
                // remain valid (`team.alpha.task_progress` => `team.alpha`, `task_progress`).
                // Tier names cannot contain dots (validated in `AgentDefinition::validate`).
                let (agent, tier_name) = match token.rsplit_once('.') {
                    Some((agent, tier_name)) => {
                        let agent = agent.trim();
                        let tier_name = tier_name.trim();
                        if agent.is_empty() || tier_name.is_empty() {
                            return None;
                        }
                        (Some(agent.to_string()), tier_name.to_string())
                    },
                    _ => (None, token.to_string()),
                };

                if tier_name.trim().is_empty() {
                    return None;
                }
                tier_refs.push(ParsedTierRef { agent, tier_name });
            }

            if tier_refs.is_empty() {
                return None;
            }

            return Some(Self::Tiers { tier_refs });
        }

        None
    }
}

/// Source data resolved for a consolidation transform (Phase 3).
///
/// The Phase 3 interpreter resolves `SourceRef` entries to concrete data before
/// passing them to builtin or LLM transforms.
///
/// Generic over the episode record type `E` so this crate stays free of
/// magician-side coupling: magician parameterizes the alias with
/// `V3EpisodeRecord`.
#[derive(Debug, Clone)]
pub enum ConsolidationInput<E> {
    Episodes(Vec<E>),
    Tiers(HashMap<String, V3MemoryTierRecord>),
    /// Result data from a completed pipeline step.
    StepResult {
        step_id: String,
        result: Value,
    },
}

/// Output from a consolidation transform (Phase 3).
///
/// Transforms produce either structured data (merged into the target tier via
/// `MergeStrategy`) or a pre-rendered text fragment.
#[derive(Debug, Clone)]
pub enum TransformOutput {
    Data {
        value: Value,
        merge: Option<MergeStrategy>,
    },
    Rendered(String),
}

/// Records what changed in memory during a cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDelta {
    pub tier_name: String,
    pub field_path: String,
    pub operation: DeltaOperation,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaOperation {
    Added {
        #[serde(default)]
        key: Option<String>,
    },
    Updated {
        key: String,
    },
    Removed {
        key: String,
    },
    Replaced,
}

/// Summary of an action taken during a cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSummary {
    pub action_type: String,
    pub description: String,
    pub tool: String,
    pub succeeded: bool,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

// ── Concrete default tier schemas (Phase 2 contracts) ─────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskProgressTier {
    #[serde(default)]
    pub context_summary: String,
    #[serde(default)]
    pub items: Vec<TaskProgressItem>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProgressItem {
    pub id: String,
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntitiesTier {
    #[serde(default)]
    pub entities: Vec<EntityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    pub name: String,
    pub entity_type: String,
    #[serde(default)]
    pub facts: HashMap<String, Value>,
    #[serde(default)]
    pub first_seen: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_referenced: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeTier {
    #[serde(default)]
    pub strategy_records: Vec<StrategyRecord>,
    #[serde(default)]
    pub insights: Vec<InsightRecord>,
    #[serde(default)]
    pub patterns: Vec<PatternRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecord {
    pub goal_id: String,
    pub strategy_type: String,
    pub succeeded: bool,
    pub execution_time_ms: u64,
    pub actions_count: usize,
    pub timestamp: DateTime<Utc>,
}

/// Aggregated strategy effectiveness for an agent, derived from `StrategyRecord` entries.
/// Returns a neutral prior of 0.5 when fewer than 5 data points exist (P6-10 Loop 4).
#[derive(Debug, Clone, Default)]
pub struct StrategyEffectiveness {
    entries: Vec<StrategyRecord>,
}

impl StrategyEffectiveness {
    pub fn record(&mut self, r: StrategyRecord) {
        self.entries.push(r);
    }

    /// Success rate for a (goal_id, strategy_type) pair.
    /// Returns 0.5 (neutral prior) when fewer than 5 data points exist.
    pub fn success_rate(&self, goal_id: &str, strategy_type: &str) -> f64 {
        let relevant: Vec<_> = self
            .entries
            .iter()
            .filter(|r| r.goal_id == goal_id && r.strategy_type == strategy_type)
            .collect();
        if relevant.len() < 5 {
            return 0.5;
        }
        let successes = relevant.iter().filter(|r| r.succeeded).count();
        successes as f64 / relevant.len() as f64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightRecord {
    pub description: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternRecord {
    pub description: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub period: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchiveTier {
    #[serde(default)]
    pub summaries: Vec<ArchiveSummary>,
    #[serde(default)]
    pub statistics: HashMap<String, Value>,
    #[serde(default)]
    pub milestones: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSummary {
    pub period: String,
    pub summary: String,
    #[serde(default)]
    pub total_cycles: Option<u32>,
    #[serde(default)]
    pub success_rate: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_field_schema_tagged_deserialize() {
        let yaml = r#"
name: entities
scope: agent
description: test
schema:
  entities:
    type: collection
    max_items: 20
render:
  format: compact_summary
  template: "{entities}"
retention: forever
"#;

        let tier: MemoryTierDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(tier.name, "entities");
        assert!(matches!(tier.scope, TierScope::Agent));
        assert!(tier.schema.contains_key("entities"));
    }

    #[test]
    fn retention_mode_accepts_legacy_integer_and_serializes_canonically() {
        let retention: RetentionMode = serde_yaml::from_str("14\n").unwrap();
        assert!(matches!(retention, RetentionMode::Days(14)));

        let yaml = serde_yaml::to_string(&retention).unwrap();
        assert_eq!(yaml, "!days 14\n");
        let round_trip: RetentionMode = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(round_trip, RetentionMode::Days(14)));

        let json = serde_json::to_string(&retention).unwrap();
        assert_eq!(json, r#"{"days":14}"#);
        let json_round_trip: RetentionMode = serde_json::from_str(&json).unwrap();
        assert!(matches!(json_round_trip, RetentionMode::Days(14)));
    }

    #[test]
    fn memory_tier_definition_accepts_legacy_integer_retention() {
        let yaml = r#"
name: research_notes
scope: agent
description: test
schema: {}
render:
  format: compact_summary
  template: "{research_notes}"
retention: 14
"#;

        let tier: MemoryTierDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(tier.retention, RetentionMode::Days(14)));
    }

    #[test]
    fn consolidation_transform_llm_with_merge_deserializes() {
        let yaml = r#"
name: update_prices
trigger: cycle_completed
source: episodes(goal_id, limit=1)
target: price_tracking
transform:
  type: llm
  prompt: extract
  operation: memory_insight_distillation
  merge: upsert_by_name_per_source
"#;

        let rule: MemoryConsolidationRule = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(rule.name, "update_prices");
        match rule.transform {
            ConsolidationTransform::Llm {
                operation, merge, ..
            } => {
                assert_eq!(
                    operation,
                    Some(MemoryConsolidationOperation::MemoryInsightDistillation)
                );
                assert!(matches!(merge, Some(MergeStrategy::UpsertByNamePerSource)));
            },
            other => panic!("unexpected transform: {other:?}"),
        }
    }

    #[test]
    fn consolidation_transform_rejects_unknown_memory_operation() {
        let yaml = r#"
name: update_prices
trigger: cycle_completed
source: episodes(goal_id, limit=1)
target: price_tracking
transform:
  type: llm
  prompt: extract
  operation: query_analysis
"#;
        let error = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
        assert!(error.to_string().contains("query_analysis"));
    }

    #[test]
    fn consolidation_trigger_batch_deserializes_declarative_shape() {
        let yaml = r#"
name: extract_entities
trigger:
  batch:
    interval_hours: 24
    min_episodes: 10
source: episodes(unprocessed=true)
target: entities
transform:
  type: llm
  prompt: extract
"#;

        let rule: MemoryConsolidationRule = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(rule.name, "extract_entities");
        match rule.trigger {
            ConsolidationTrigger::Batch {
                interval_hours,
                interval_days,
                min_episodes,
                max_staleness_hours,
            } => {
                assert_eq!(interval_hours, Some(24));
                assert_eq!(interval_days, None);
                assert_eq!(min_episodes, Some(10));
                assert_eq!(max_staleness_hours, None);
            },
            other => panic!("unexpected trigger: {other:?}"),
        }
    }

    #[test]
    fn consolidation_trigger_batch_rejects_unknown_field() {
        let yaml = r#"
name: extract_entities
trigger:
  batch:
    interval_hours: 24
    typo: true
source: episodes(unprocessed=true)
target: entities
transform:
  type: llm
  prompt: extract
"#;

        let err = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("typo"));
    }

    #[test]
    fn consolidation_trigger_batch_rejects_empty_payload() {
        let yaml = r#"
name: extract_entities
trigger:
  batch: {}
source: episodes(unprocessed=true)
target: entities
transform:
  type: llm
  prompt: extract
"#;

        let err = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(err.to_string().contains("requires at least one"));
    }

    #[test]
    fn consolidation_trigger_batch_rejects_zero_values() {
        let yaml = r#"
name: extract_entities
trigger:
  batch:
    interval_hours: 0
source: episodes(unprocessed=true)
target: entities
transform:
  type: llm
  prompt: extract
"#;

        let err = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(err.to_string().contains("interval_hours"));
        assert!(err.to_string().contains("must be > 0"));
    }

    #[test]
    fn source_ref_parse_episodes() {
        let source = SourceRef::parse("episodes(goal_1, limit=30, unprocessed=true)").unwrap();
        assert_eq!(
            source,
            SourceRef::Episodes {
                goal_id: Some("goal_1".to_string()),
                limit: Some(30),
                unprocessed: true,
            }
        );
    }

    #[test]
    fn source_ref_parse_tiers() {
        let source = SourceRef::parse(
            "tiers(task_progress, web-scraper.price_tracking, team.alpha.task_progress)",
        )
        .unwrap();
        assert_eq!(
            source,
            SourceRef::Tiers {
                tier_refs: vec![
                    ParsedTierRef {
                        agent: None,
                        tier_name: "task_progress".to_string(),
                    },
                    ParsedTierRef {
                        agent: Some("web-scraper".to_string()),
                        tier_name: "price_tracking".to_string(),
                    },
                    ParsedTierRef {
                        agent: Some("team.alpha".to_string()),
                        tier_name: "task_progress".to_string(),
                    },
                ],
            }
        );
    }

    #[test]
    fn source_ref_parse_rejects_invalid_input() {
        assert!(SourceRef::parse("tiers()").is_none());
        assert!(SourceRef::parse("episodes(").is_none());
        assert!(SourceRef::parse("unknown(source)").is_none());
        assert!(SourceRef::parse("episodes(goal_1, limit=abc)").is_none());
        assert!(SourceRef::parse("episodes(goal_1, foo=bar)").is_none());
        assert!(SourceRef::parse("episodes(goal_1, goal_2)").is_none());
        assert!(SourceRef::parse("episodes(goal_id=)").is_none());
        assert!(SourceRef::parse("episodes(limit=0)").is_none());
        assert!(SourceRef::parse("episodes(goal_id=a, goal_id=b)").is_none());
        assert!(SourceRef::parse("episodes(limit=1, limit=2)").is_none());
        assert!(SourceRef::parse("episodes(unprocessed=true, unprocessed=false)").is_none());
        assert!(SourceRef::parse("tiers(task_progress,)").is_none());
        assert!(SourceRef::parse("tiers(.task_progress)").is_none());
        assert!(SourceRef::parse("tiers(agent.)").is_none());
    }

    #[test]
    fn memory_tier_definition_rejects_unknown_field() {
        let yaml = r#"
name: entities
scope: agent
description: test
schema:
  entities:
    type: collection
render:
  format: compact_summary
  template: "{entities}"
retention: forever
unknown_field: true
"#;

        let err = serde_yaml::from_str::<MemoryTierDefinition>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("unknown_field"));
    }

    #[test]
    fn memory_consolidation_rule_rejects_unknown_field() {
        let yaml = r#"
name: update_prices
trigger: cycle_completed
source: episodes(goal_id, limit=1)
target: price_tracking
transform:
  type: llm
  prompt: extract
extra: oops
"#;

        let err = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("extra"));
    }

    #[test]
    fn memory_tier_definition_rejects_unknown_tagged_schema_variant_field() {
        let yaml = r#"
name: entities
scope: agent
description: test
schema:
  entities:
    type: collection
    max_items: 20
    typo_inner: true
render:
  format: compact_summary
  template: "{entities}"
retention: forever
"#;

        let err = serde_yaml::from_str::<MemoryTierDefinition>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("typo_inner"));
    }

    #[test]
    fn memory_consolidation_rule_rejects_unknown_transform_variant_field() {
        let yaml = r#"
name: update_prices
trigger: cycle_completed
source: episodes(goal_id, limit=1)
target: price_tracking
transform:
  type: llm
  prompt: extract
  merge: upsert_by_name_per_source
  typo_merge: true
"#;

        let err = serde_yaml::from_str::<MemoryConsolidationRule>(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("typo_merge"));
    }

    fn make_record(goal_id: &str, strategy_type: &str, succeeded: bool) -> StrategyRecord {
        StrategyRecord {
            goal_id: goal_id.to_string(),
            strategy_type: strategy_type.to_string(),
            succeeded,
            execution_time_ms: 500,
            actions_count: 3,
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn strategy_effectiveness_neutral_prior_with_fewer_than_5_points() {
        let mut eff = StrategyEffectiveness::default();
        eff.record(make_record("g1", "GuidedSearch", true));
        assert_eq!(
            eff.success_rate("g1", "GuidedSearch"),
            0.5,
            "fewer than 5 points → neutral prior 0.5"
        );
    }

    #[test]
    fn strategy_effectiveness_correct_rate_with_5_or_more_points() {
        let mut eff = StrategyEffectiveness::default();
        for i in 0..5_u64 {
            eff.record(make_record("g1", "GuidedSearch", i < 4)); // 4 successes, 1 failure
        }
        let rate = eff.success_rate("g1", "GuidedSearch");
        assert!((rate - 0.8).abs() < f64::EPSILON, "4/5 = 0.8, got {rate}");
    }

    #[test]
    fn strategy_effectiveness_isolates_by_goal_and_strategy_type() {
        let mut eff = StrategyEffectiveness::default();
        for _ in 0..5 {
            eff.record(make_record("g1", "GuidedSearch", true));
            eff.record(make_record("g2", "GuidedSearch", false));
        }
        assert_eq!(eff.success_rate("g1", "GuidedSearch"), 1.0);
        assert_eq!(eff.success_rate("g2", "GuidedSearch"), 0.0);
    }

    #[test]
    fn consolidation_trigger_batch_round_trips_through_yaml() {
        let rule = MemoryConsolidationRule {
            name: "round_trip".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(24),
                interval_days: None,
                min_episodes: Some(10),
                max_staleness_hours: Some(72),
            },
            source: "episodes(unprocessed=true)".to_string(),
            target: "entities".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        };

        let yaml = serde_yaml::to_string(&rule).unwrap();
        let back: MemoryConsolidationRule = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(back.name, "round_trip");
        match back.trigger {
            ConsolidationTrigger::Batch {
                interval_hours,
                interval_days,
                min_episodes,
                max_staleness_hours,
            } => {
                assert_eq!(interval_hours, Some(24));
                assert_eq!(interval_days, None);
                assert_eq!(min_episodes, Some(10));
                assert_eq!(max_staleness_hours, Some(72));
            },
            other => panic!("expected Batch, got {other:?}"),
        }
    }

    #[test]
    fn consolidation_trigger_unit_variants_round_trip_through_yaml() {
        for (trigger, expected_str) in [
            (ConsolidationTrigger::CycleCompleted, "cycle_completed"),
            (ConsolidationTrigger::StepCompleted, "step_completed"),
            (ConsolidationTrigger::RetentionExpiry, "retention_expiry"),
        ] {
            let yaml = serde_yaml::to_string(&trigger).unwrap();
            assert!(
                yaml.contains(expected_str),
                "expected `{expected_str}` in: {yaml}"
            );
            let back: ConsolidationTrigger = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(
                std::mem::discriminant(&trigger),
                std::mem::discriminant(&back),
            );
        }
    }
}
