//! Canonical, content-free LLM call and provider-attempt fact contracts.
//!
//! Phase 2 routes every producer through [`LlmTraceRecorder`] instead of
//! letting call sites invent persistence rows. Records are immutable revisions
//! identified by `(record_kind, stable_id, revision)`, which is also the
//! journal/materializer idempotency key.

use std::{collections::HashSet, sync::Arc};

use magicllm::{LlmScope, LlmTraceContext};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const LLM_TRACE_FACT_SCHEMA_VERSION: u16 = 2;
pub const LLM_TRACE_JOURNAL_SCHEMA_VERSION: u16 = 1;
pub const LLM_RESTRICTED_CONTENT_SCHEMA_VERSION: u16 = 1;
/// Hard ceiling for a single canonical fact before it enters any in-memory
/// lane or durable journal. Queue record-count limits alone cannot bound
/// memory when an identity-like string is unexpectedly enormous.
pub const MAX_SERIALIZED_LLM_TRACE_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_SERIALIZED_LLM_RESTRICTED_RECORD_BYTES: usize = 640 * 1024;
const MAX_MACHINE_CATEGORY_BYTES: usize = 128;
const PRICING_ROW_VERSION_PREFIX: &str = "pricing-row-v1:";
/// A logical chunk parent summarizes already-attributed physical child calls.
/// It reports zero provider attempts because physical children own its costs.
pub const LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND: &str = "logical_chunk_summary";
/// A managed CLI reports aggregate usage without physical provider receipts.
pub const LLM_HARNESS_AGGREGATE_RESPONSE_KIND: &str = "harness_aggregate";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCaptureMode {
    Off,
    Metadata,
    Sanitized,
    FullLocalEncrypted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCaptureStatus {
    Complete,
    MetadataOnly,
    Redacted,
    SampledOut,
    PolicyDenied,
    Oversize,
    BackpressureDegraded,
    WriteFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCostSource {
    Provider,
    Computed,
    Estimated,
    Unknown,
    /// Ran on local hardware; there was no vendor to charge.
    ///
    /// Distinct from a `Computed` or `Provider` cost that happens to be zero.
    /// Both render as "0" in dollars, but they answer different questions: a
    /// zero from a vendor means the call was free THIS TIME, while this means
    /// money was never the unit. A cost view that cannot tell them apart shows
    /// local work as either suspiciously free spend or, if the price is absent
    /// entirely, as nothing at all.
    ///
    /// Deliberately a cost SOURCE and not a provider name: the question this
    /// answers is "where did this number come from", which is exactly what
    /// this enum is for. Adding provider identity to the completion record to
    /// infer the same thing would carry a second fact to re-derive a
    /// conclusion the pricing step already holds.
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallTerminalState {
    Succeeded,
    Failed,
    Cancelled,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmAttemptTerminalState {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderAttemptPhase {
    Started,
    FirstToken,
    Completed,
}

impl LlmProviderAttemptPhase {
    pub const fn revision(self) -> u32 {
        match self {
            Self::Started => 1,
            Self::FirstToken => 2,
            Self::Completed => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmTraceRecordKind {
    CallFact,
    ProviderAttempt,
    CaptureGap,
    ToolLineage,
    CallIo,
    ContextBlock,
    ContentTombstone,
    ContentAccessAudit,
}

/// Buffer priority is part of the capture contract even though Phase 2A only
/// defines critical, content-free facts. Later lineage and restricted-payload
/// records can opt into their lower-priority lanes without changing the sink
/// or weakening critical-fact durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmTraceBufferClass {
    Critical,
    Lineage,
    RestrictedPayload,
}

impl LlmTraceRecordKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CallFact => "call_fact",
            Self::ProviderAttempt => "provider_attempt",
            Self::CaptureGap => "capture_gap",
            Self::ToolLineage => "tool_lineage",
            Self::CallIo => "call_io",
            Self::ContextBlock => "context_block",
            Self::ContentTombstone => "content_tombstone",
            Self::ContentAccessAudit => "content_access_audit",
        }
    }
}

/// Immutable lifecycle checkpoints for one model-proposed tool execution.
///
/// Stages are deliberately independent: a policy denial is not a transport
/// failure, a zero exit status is not result validation, and a result is not
/// considered useful merely because a later request happened to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolLineageStage {
    Proposed,
    NameValidated,
    ArgumentsParsed,
    SchemaValidated,
    AuthorizationResolved,
    ApprovalResolved,
    ExecutionStarted,
    ExecutionFinished,
    ResultValidated,
    ResultConsumed,
    BranchMaterialized,
    RollbackStarted,
    RollbackFinished,
    LinkageGap,
}

impl LlmToolLineageStage {
    /// Stable revision ranges leave room for execution attempts and repeated
    /// result consumers without making ingestion order part of identity.
    pub fn revision(self, stage_index: u32) -> Result<u32, LlmTraceRecordError> {
        let (base, indexed) = match self {
            Self::Proposed => (1, false),
            Self::NameValidated => (10, false),
            Self::ArgumentsParsed => (20, false),
            Self::SchemaValidated => (30, false),
            Self::AuthorizationResolved => (40, false),
            Self::ApprovalResolved => (50, false),
            Self::ExecutionStarted => (1_000, true),
            Self::ExecutionFinished => (2_000, true),
            Self::ResultValidated => (3_000, false),
            Self::ResultConsumed => (4_000, true),
            Self::BranchMaterialized => (5_000, false),
            Self::RollbackStarted => (6_000, true),
            Self::RollbackFinished => (7_000, true),
            Self::LinkageGap => (8_000, true),
        };
        if indexed {
            if !(1..=999).contains(&stage_index) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "indexed tool-lineage stage requires stage_index in 1..=999".to_string(),
                ));
            }
            Ok(base + stage_index)
        } else if stage_index == 0 {
            Ok(base)
        } else {
            Err(LlmTraceRecordError::InvalidRecord(
                "singleton tool-lineage stage requires stage_index=0".to_string(),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolLineageOutcome {
    Pending,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    TimedOut,
    Abandoned,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolFailureOwner {
    Model,
    Arguments,
    Schema,
    Policy,
    ResourceAuthority,
    Approval,
    Runtime,
    Tool,
    ResultValidation,
    ExternalProvider,
    Cancellation,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolSideEffectState {
    None,
    Unknown,
    Pending,
    Remained,
    Reversed,
    RollbackFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolBranchState {
    Active,
    Successful,
    Abandoned,
    Superseded,
    Unknown,
}

/// Content-free tool/action lineage. Arguments and result bodies never enter
/// this dataset; only scoped fingerprints and canonical artifact/event refs do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolLineageRecord {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub model_tool_call_id: String,
    pub tool_execution_id: String,
    pub branch_id: String,
    pub operation: String,
    pub source_surface: String,
    pub tool_name: String,
    pub tool_family: Option<String>,
    pub stage: LlmToolLineageStage,
    /// Attempt or consumption ordinal for repeatable stages; zero for
    /// singleton stages. See [`LlmToolLineageStage::revision`].
    pub stage_index: u32,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub arguments_fingerprint: Option<String>,
    pub result_ref: Option<String>,
    pub canonical_event_ref: Option<String>,
    /// Child/specialist execution identities created by delegation or other
    /// cross-execution tools. IDs only; no delegated prompt/result content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_execution_ids: Vec<String>,
    pub consumed_by_call_id: Option<String>,
    pub name_known: Option<bool>,
    pub arguments_parsed: Option<bool>,
    pub schema_matched: Option<bool>,
    pub policy_allowed: Option<bool>,
    pub approval_required: Option<bool>,
    pub approval_obtained: Option<bool>,
    pub transport_ran: Option<bool>,
    pub tool_reported_success: Option<bool>,
    pub result_validation_success: Option<bool>,
    pub outcome: LlmToolLineageOutcome,
    pub failure_owner: Option<LlmToolFailureOwner>,
    pub failure_code: Option<String>,
    pub side_effect_state: LlmToolSideEffectState,
    pub branch_state: LlmToolBranchState,
    pub on_successful_path: Option<bool>,
    pub same_tool_arguments_count: u32,
    pub observation_action_cycle_count: u32,
    pub recovered_after_failure: bool,
    pub linkage_gap: Option<String>,
}

pub const MAX_TOOL_RELATED_EXECUTION_IDS: usize = 64;
pub const MAX_TOOL_RELATED_EXECUTION_ID_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallIoPhase {
    LogicalRequest,
    EffectiveRequest,
    NormalizedResponse,
}

impl LlmCallIoPhase {
    pub fn revision(self, provider_attempt_index: Option<u32>) -> Result<u32, LlmTraceRecordError> {
        match self {
            Self::LogicalRequest => Ok(1),
            Self::EffectiveRequest => provider_attempt_index
                .filter(|value| *value > 0 && *value <= 100_000)
                .map(|value| 1_000 + value)
                .ok_or_else(|| {
                    LlmTraceRecordError::InvalidRecord(
                        "effective request requires provider_attempt_index in 1..=100000"
                            .to_string(),
                    )
                }),
            Self::NormalizedResponse => provider_attempt_index
                .filter(|value| *value > 0 && *value <= 100_000)
                .map(|value| 200_000 + value)
                .ok_or_else(|| {
                    LlmTraceRecordError::InvalidRecord(
                        "normalized response requires provider_attempt_index in 1..=100000"
                            .to_string(),
                    )
                }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LlmRedactionReport {
    pub policy_version: String,
    pub redaction_count: u32,
    pub categories: Vec<String>,
    pub truncated_block_count: u32,
    pub reference_only_block_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCallIoRecord {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub operation: String,
    pub phase: LlmCallIoPhase,
    pub provider_attempt_index: Option<u32>,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub capture: LlmCaptureFact,
    pub retention_class: String,
    pub redaction: LlmRedactionReport,
    pub logical_request_fingerprint: Option<String>,
    pub effective_request_fingerprint: Option<String>,
    pub response_fingerprint: Option<String>,
    pub original_bytes: u64,
    pub sanitized_bytes: u64,
    /// Sanitized normalized request/response envelope. This value is legal
    /// only in the restricted dataset and is never projected into fact SQL.
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmContextBlockRecord {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub operation: String,
    pub context_block_id: String,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub position: u32,
    pub message_index: u32,
    pub content_index: u32,
    pub role: String,
    pub block_kind: String,
    pub source_kind: String,
    pub source_id: Option<String>,
    pub required: bool,
    pub original_chars: u64,
    pub effective_chars: u64,
    pub original_fingerprint: String,
    pub effective_fingerprint: String,
    pub transformation: String,
    pub truncation_reason: Option<String>,
    pub redaction_categories: Vec<String>,
    pub payload_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmContentTombstone {
    pub schema_version: u16,
    pub tombstone_id: String,
    pub scope: LlmScope,
    pub target_kind: String,
    pub target_id: String,
    pub reason: String,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmContentAccessAudit {
    pub schema_version: u16,
    pub audit_id: String,
    pub scope: LlmScope,
    pub actor_id: String,
    pub execution_id: Option<String>,
    pub target_kind: String,
    pub target_id: String,
    pub content_kind: String,
    pub reason: String,
    pub redaction_version: String,
    pub bytes_returned: u64,
    pub outcome: String,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LlmTraceRecordKey {
    pub record_kind: LlmTraceRecordKind,
    pub stable_id: String,
    pub revision: u32,
}

impl LlmTraceRecordKey {
    pub fn new(
        record_kind: LlmTraceRecordKind,
        stable_id: impl Into<String>,
        revision: u32,
    ) -> Self {
        Self {
            record_kind,
            stable_id: stable_id.into(),
            revision,
        }
    }

    pub fn idempotency_key(&self) -> String {
        format!(
            "{}:{}:r{}",
            self.record_kind.as_str(),
            self.stable_id,
            self.revision
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTokenUsageFact {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub audio_input_tokens: Option<u64>,
    pub audio_output_tokens: Option<u64>,
    pub audio_cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl LlmTokenUsageFact {
    fn validate(&self) -> Result<(), LlmTraceRecordError> {
        if let (Some(input), Some(output), Some(total)) =
            (self.input_tokens, self.output_tokens, self.total_tokens)
        {
            if input.checked_add(output) != Some(total) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "total_tokens must equal input_tokens plus output_tokens when all are populated"
                        .to_string(),
                ));
            }
        }
        if let (Some(reasoning), Some(output)) = (self.reasoning_tokens, self.output_tokens) {
            if reasoning > output {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "reasoning_tokens must not exceed output_tokens".to_string(),
                ));
            }
        }
        for (name, value) in [
            ("cache_read_tokens", self.cache_read_tokens),
            ("cache_creation_tokens", self.cache_creation_tokens),
        ] {
            if value
                .zip(self.input_tokens)
                .is_some_and(|(value, input)| value > input)
            {
                return Err(LlmTraceRecordError::InvalidRecord(format!(
                    "{name} must not exceed input_tokens"
                )));
            }
        }
        if self
            .cache_read_tokens
            .zip(self.cache_creation_tokens)
            .zip(self.input_tokens)
            .is_some_and(|((read, creation), input)| {
                read.checked_add(creation)
                    .is_none_or(|cached| cached > input)
            })
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "cache_read_tokens plus cache_creation_tokens must not exceed input_tokens"
                    .to_string(),
            ));
        }
        for (name, value, bound_name, bound) in [
            (
                "audio_input_tokens",
                self.audio_input_tokens,
                "input_tokens",
                self.input_tokens,
            ),
            (
                "audio_output_tokens",
                self.audio_output_tokens,
                "output_tokens",
                self.output_tokens,
            ),
            (
                "audio_cached_tokens",
                self.audio_cached_tokens,
                "cache_read_tokens",
                self.cache_read_tokens,
            ),
        ] {
            if let (Some(value), Some(bound)) = (value, bound) {
                if value > bound {
                    return Err(LlmTraceRecordError::InvalidRecord(format!(
                        "{name} must not exceed {bound_name}"
                    )));
                }
            }
        }
        let has_audio_split = self.audio_input_tokens.is_some()
            || self.audio_output_tokens.is_some()
            || self.audio_cached_tokens.is_some();
        if has_audio_split
            && (self.audio_input_tokens.is_none()
                || self.audio_output_tokens.is_none()
                || self.audio_cached_tokens.is_none()
                || self.input_tokens.is_none()
                || self.output_tokens.is_none()
                || self.cache_read_tokens.is_none()
                || self.cache_creation_tokens.is_none())
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "realtime modality usage requires every audio and folded token bucket".to_string(),
            ));
        }
        if has_audio_split {
            if self.cache_creation_tokens.is_some_and(|value| value != 0) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "realtime modality usage cannot represent cache-creation tokens".to_string(),
                ));
            }
            if self
                .audio_input_tokens
                .zip(self.cache_read_tokens)
                .zip(self.cache_creation_tokens)
                .zip(self.input_tokens)
                .is_some_and(|(((audio, cache_read), cache_creation), input)| {
                    audio
                        .checked_add(cache_read)
                        .and_then(|value| value.checked_add(cache_creation))
                        .is_none_or(|accounted| accounted > input)
                })
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "uncached audio plus cache buckets must not exceed input_tokens".to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmPricingFact {
    pub pricing_version: Option<String>,
    pub cost_source: Option<LlmCostSource>,
    pub input_cost_usd: Option<f64>,
    pub output_cost_usd: Option<f64>,
    pub reasoning_cost_usd: Option<f64>,
    pub cache_cost_usd: Option<f64>,
    pub cost_usd: Option<f64>,
}

impl LlmPricingFact {
    fn validate(&self) -> Result<(), LlmTraceRecordError> {
        let mut has_monetary_value = false;
        for (name, value) in [
            ("input_cost_usd", self.input_cost_usd),
            ("output_cost_usd", self.output_cost_usd),
            ("reasoning_cost_usd", self.reasoning_cost_usd),
            ("cache_cost_usd", self.cache_cost_usd),
            ("cost_usd", self.cost_usd),
        ] {
            has_monetary_value |= value.is_some();
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                return Err(LlmTraceRecordError::InvalidRecord(format!(
                    "{name} must be finite and non-negative"
                )));
            }
        }
        if self.cost_source.is_some() != self.pricing_version.is_some() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "pricing_version and cost_source must be populated together".to_string(),
            ));
        }
        if has_monetary_value && self.cost_source.is_none() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "monetary values require both pricing_version and cost_source".to_string(),
            ));
        }
        match self.cost_source {
            Some(LlmCostSource::Unknown) if has_monetary_value => {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "unknown pricing cannot carry fabricated monetary values".to_string(),
                ));
            },
            Some(LlmCostSource::Provider | LlmCostSource::Computed | LlmCostSource::Estimated)
                if !has_monetary_value =>
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "known pricing source must carry at least one monetary value".to_string(),
                ));
            },
            // `Local` asserts there was no vendor, so a nonzero charge is a
            // contradiction rather than a surprising number. Validated here
            // because the arms above deliberately do not cover it: without
            // this, the new variant would fall through the catch-all and be
            // the one cost source nothing checks — which is how a source of
            // truth quietly stops being one.
            Some(LlmCostSource::Local) if self.cost_usd.is_some_and(|cost| cost != 0.0) => {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "local pricing ran on no vendor and cannot carry a nonzero cost".to_string(),
                ));
            },
            _ => {},
        }
        if self
            .pricing_version
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "pricing_version must not be blank".to_string(),
            ));
        }
        if self
            .pricing_version
            .as_deref()
            .is_some_and(|value| !is_content_free_pricing_version(value))
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "pricing_version must be a content-free machine identifier of at most 128 bytes"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

fn is_content_free_pricing_version(value: &str) -> bool {
    let structurally_safe = !value.is_empty()
        && value.trim() == value
        && value.len() <= MAX_MACHINE_CATEGORY_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/' | b'@')
        });
    structurally_safe
        && value
            .strip_prefix(PRICING_ROW_VERSION_PREFIX)
            .is_none_or(|fingerprint| {
                fingerprint.len() == 64
                    && fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTimingFact {
    pub created_at_ms: Option<i64>,
    pub submitted_at_ms: Option<i64>,
    pub started_at_ms: Option<i64>,
    pub first_token_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub queue_wait_ms: Option<u64>,
    pub local_prep_ms: Option<u64>,
    pub provider_execution_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub generation_after_ttft_ms: Option<u64>,
    pub parse_ms: Option<u64>,
    pub validation_ms: Option<u64>,
    pub latency_ms: Option<u64>,
}

impl LlmTimingFact {
    fn validate(&self) -> Result<(), LlmTraceRecordError> {
        let ordered = [
            self.created_at_ms,
            self.submitted_at_ms,
            self.started_at_ms,
            self.first_token_at_ms,
            self.completed_at_ms,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if ordered.iter().any(|timestamp| *timestamp < 0) {
            return Err(LlmTraceRecordError::InvalidRecord(
                "timing timestamps must be non-negative".to_string(),
            ));
        }
        if ordered.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(LlmTraceRecordError::InvalidRecord(
                "timing timestamps must be monotonic when populated".to_string(),
            ));
        }
        if self
            .ttft_ms
            .zip(self.latency_ms)
            .is_some_and(|(ttft, latency)| ttft > latency)
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "ttft_ms must not exceed latency_ms".to_string(),
            ));
        }
        if self.first_token_at_ms.is_some() != self.ttft_ms.is_some()
            || self.ttft_ms.is_some() && self.started_at_ms.is_none()
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "first_token_at_ms and ttft_ms must be populated together with started_at_ms"
                    .to_string(),
            ));
        }
        if self.generation_after_ttft_ms.is_some() && self.ttft_ms.is_none() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "generation_after_ttft_ms requires an observed ttft_ms".to_string(),
            ));
        }
        if let (Some(ttft), Some(generation), Some(latency)) =
            (self.ttft_ms, self.generation_after_ttft_ms, self.latency_ms)
        {
            if ttft
                .checked_add(generation)
                .is_none_or(|total| total > latency)
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "ttft_ms plus generation_after_ttft_ms must not exceed latency_ms".to_string(),
                ));
            }
        }
        if let (Some(started), Some(first_token), Some(ttft)) =
            (self.started_at_ms, self.first_token_at_ms, self.ttft_ms)
        {
            if u64::try_from(first_token - started).ok() != Some(ttft) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "ttft_ms must match first_token_at_ms minus started_at_ms".to_string(),
                ));
            }
        }
        if let Some(latency) = self.latency_ms {
            for (name, value) in [
                ("queue_wait_ms", self.queue_wait_ms),
                ("local_prep_ms", self.local_prep_ms),
                ("provider_execution_ms", self.provider_execution_ms),
                ("ttft_ms", self.ttft_ms),
                ("generation_after_ttft_ms", self.generation_after_ttft_ms),
                ("parse_ms", self.parse_ms),
                ("validation_ms", self.validation_ms),
            ] {
                if value.is_some_and(|value| value > latency) {
                    return Err(LlmTraceRecordError::InvalidRecord(format!(
                        "{name} must not exceed latency_ms"
                    )));
                }
            }
            let decomposed = [
                self.queue_wait_ms,
                self.local_prep_ms,
                self.provider_execution_ms,
                self.parse_ms,
                self.validation_ms,
            ]
            .into_iter()
            .flatten()
            .try_fold(0_u64, u64::checked_add);
            if decomposed.is_none_or(|total| total > latency) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "queue/local-prep/provider/parse/validation timing must not exceed latency_ms"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmImmediateValidationFact {
    pub response_present: bool,
    pub parse_attempted: bool,
    pub parse_success: Option<bool>,
    pub schema_validation_attempted: bool,
    pub schema_validation_success: Option<bool>,
    pub contract_validation_attempted: bool,
    pub contract_validation_success: Option<bool>,
    pub validation_error_class: Option<String>,
    pub discarded_before_use: bool,
    pub discard_reason: Option<String>,
    pub superseded_by_call_id: Option<String>,
}

impl LlmImmediateValidationFact {
    fn validate(&self) -> Result<(), LlmTraceRecordError> {
        for (attempted, success, label) in [
            (self.parse_attempted, self.parse_success, "parse"),
            (
                self.schema_validation_attempted,
                self.schema_validation_success,
                "schema_validation",
            ),
            (
                self.contract_validation_attempted,
                self.contract_validation_success,
                "contract_validation",
            ),
        ] {
            if attempted != success.is_some() {
                return Err(LlmTraceRecordError::InvalidRecord(format!(
                    "{label}_success must be populated exactly when {label} was attempted"
                )));
            }
        }
        if self.discarded_before_use != self.discard_reason.is_some() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "discard_reason must be populated exactly when discarded_before_use is true"
                    .to_string(),
            ));
        }
        if !self.response_present
            && (self.parse_attempted
                || self.schema_validation_attempted
                || self.contract_validation_attempted)
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "validation cannot be attempted when no response is present".to_string(),
            ));
        }
        let validation_failed = [
            self.parse_success,
            self.schema_validation_success,
            self.contract_validation_success,
        ]
        .into_iter()
        .any(|result| result == Some(false));
        if validation_failed != self.validation_error_class.is_some() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "validation_error_class must be populated exactly when immediate validation fails"
                    .to_string(),
            ));
        }
        for (name, value) in [
            (
                "validation_error_class",
                self.validation_error_class.as_deref(),
            ),
            ("discard_reason", self.discard_reason.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(LlmTraceRecordError::InvalidRecord(format!(
                    "{name} must not be blank"
                )));
            }
        }
        require_machine_category_option(
            "validation_error_class",
            self.validation_error_class.as_deref(),
        )?;
        require_machine_category_option("discard_reason", self.discard_reason.as_deref())?;
        require_stable_identifier_option(
            "superseded_by_call_id",
            self.superseded_by_call_id.as_deref(),
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmCaptureFact {
    pub mode: LlmCaptureMode,
    pub status: LlmCaptureStatus,
    pub training_eligible_at_capture: bool,
    pub training_exclusion_reason: Option<String>,
}

impl Default for LlmCaptureFact {
    fn default() -> Self {
        Self {
            mode: LlmCaptureMode::Metadata,
            status: LlmCaptureStatus::MetadataOnly,
            training_eligible_at_capture: false,
            training_exclusion_reason: Some("training_not_enabled".to_string()),
        }
    }
}

impl LlmCaptureFact {
    fn validate(&self) -> Result<(), LlmTraceRecordError> {
        if self.training_eligible_at_capture == self.training_exclusion_reason.is_some() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "training exclusion reason must exist exactly when capture is ineligible"
                    .to_string(),
            ));
        }
        if self.mode == LlmCaptureMode::Off
            && !matches!(
                self.status,
                LlmCaptureStatus::PolicyDenied | LlmCaptureStatus::SampledOut
            )
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "off capture mode must be policy_denied or sampled_out".to_string(),
            ));
        }
        if self.mode == LlmCaptureMode::Off && self.training_eligible_at_capture {
            return Err(LlmTraceRecordError::InvalidRecord(
                "off capture mode cannot be training eligible".to_string(),
            ));
        }
        require_machine_category_option(
            "training_exclusion_reason",
            self.training_exclusion_reason.as_deref(),
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCallStarted {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub operation: String,
    pub operation_family: Option<String>,
    pub capability: Option<String>,
    pub priority_lane: Option<String>,
    pub source_surface: Option<String>,
    pub origin_channel: Option<String>,
    pub requested_profile: Option<String>,
    pub selected_profile: Option<String>,
    /// Provider-conversation projection used for this logical call. Kept
    /// separate from `iteration_id` so the stable iteration identity remains
    /// safe for joins and idempotency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_projection_mode: Option<String>,
    pub capture: LlmCaptureFact,
}

impl LlmCallStarted {
    pub fn new(
        context: LlmTraceContext,
        operation: impl Into<String>,
        occurred_at_ms: i64,
    ) -> Self {
        Self {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            occurred_at_ms,
            observed_at_ms: occurred_at_ms,
            operation: operation.into(),
            operation_family: None,
            capability: None,
            priority_lane: None,
            source_surface: None,
            origin_channel: None,
            requested_profile: None,
            selected_profile: None,
            prompt_projection_mode: None,
            capture: LlmCaptureFact::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmProviderAttemptEvent {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub dispatch_job_id: Option<String>,
    pub provider_attempt_id: String,
    pub provider_attempt_index: u32,
    pub phase: LlmProviderAttemptPhase,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub operation: String,
    pub effective_profile: Option<String>,
    pub provider: String,
    pub model: String,
    pub model_revision: Option<String>,
    pub timing: LlmTimingFact,
    pub terminal_state: Option<LlmAttemptTerminalState>,
    pub error_class: Option<String>,
    pub error_code: Option<String>,
    pub finish_reason: Option<String>,
    pub refusal: Option<bool>,
    pub truncated: Option<bool>,
    pub usage: LlmTokenUsageFact,
    pub pricing: LlmPricingFact,
    pub capture: LlmCaptureFact,
}

impl LlmProviderAttemptEvent {
    pub fn started(
        context: LlmTraceContext,
        operation: impl Into<String>,
        provider_attempt_index: u32,
        provider: impl Into<String>,
        model: impl Into<String>,
        started_at_ms: i64,
    ) -> Self {
        let provider_attempt_id = context.provider_attempt_id(provider_attempt_index);
        Self {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: None,
            provider_attempt_id,
            provider_attempt_index,
            phase: LlmProviderAttemptPhase::Started,
            occurred_at_ms: started_at_ms,
            observed_at_ms: started_at_ms,
            operation: operation.into(),
            effective_profile: None,
            provider: provider.into(),
            model: model.into(),
            model_revision: None,
            timing: LlmTimingFact {
                started_at_ms: Some(started_at_ms),
                ..LlmTimingFact::default()
            },
            terminal_state: None,
            error_class: None,
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            usage: LlmTokenUsageFact::default(),
            pricing: LlmPricingFact::default(),
            capture: LlmCaptureFact::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCallCompleted {
    pub schema_version: u16,
    pub context: LlmTraceContext,
    pub dispatch_job_id: Option<String>,
    pub occurred_at_ms: i64,
    pub observed_at_ms: i64,
    pub operation: String,
    pub terminal_state: LlmCallTerminalState,
    pub provider_attempt_count: u32,
    pub provider_response_id: Option<String>,
    pub response_kind: Option<String>,
    pub error_class: Option<String>,
    pub error_code: Option<String>,
    pub finish_reason: Option<String>,
    pub refusal: Option<bool>,
    pub truncated: Option<bool>,
    pub timing: LlmTimingFact,
    pub usage: LlmTokenUsageFact,
    pub pricing: LlmPricingFact,
    pub validation: LlmImmediateValidationFact,
    pub capture: LlmCaptureFact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmCaptureGap {
    pub schema_version: u16,
    pub gap_id: String,
    pub scope: LlmScope,
    /// Logical call owning this loss when the missing records are attributable.
    /// Process-wide lag/backpressure gaps remain `None` rather than inventing
    /// an owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_id: Option<String>,
    pub operation: String,
    pub reason: String,
    pub missing_record_count: u64,
    pub first_observed_at_ms: i64,
    pub last_observed_at_ms: i64,
    pub emitted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record_type", content = "record", rename_all = "snake_case")]
pub enum LlmTraceRecord {
    CallStarted(LlmCallStarted),
    ProviderAttempt(LlmProviderAttemptEvent),
    CallCompleted(LlmCallCompleted),
    CaptureGap(LlmCaptureGap),
    ToolLineage(LlmToolLineageRecord),
    CallIo(LlmCallIoRecord),
    ContextBlock(LlmContextBlockRecord),
    ContentTombstone(LlmContentTombstone),
    ContentAccessAudit(LlmContentAccessAudit),
}

impl LlmTraceRecord {
    pub fn key(&self) -> LlmTraceRecordKey {
        match self {
            Self::CallStarted(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::CallFact,
                record.context.llm_call_id.clone(),
                1,
            ),
            Self::CallCompleted(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::CallFact,
                record.context.llm_call_id.clone(),
                2,
            ),
            Self::ProviderAttempt(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::ProviderAttempt,
                record.provider_attempt_id.clone(),
                record.phase.revision(),
            ),
            Self::CaptureGap(record) => {
                LlmTraceRecordKey::new(LlmTraceRecordKind::CaptureGap, record.gap_id.clone(), 1)
            },
            Self::ToolLineage(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::ToolLineage,
                record.tool_execution_id.clone(),
                record
                    .stage
                    .revision(record.stage_index)
                    .unwrap_or_default(),
            ),
            Self::CallIo(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::CallIo,
                record.context.llm_call_id.clone(),
                record
                    .phase
                    .revision(record.provider_attempt_index)
                    .unwrap_or_default(),
            ),
            Self::ContextBlock(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::ContextBlock,
                record.context_block_id.clone(),
                1,
            ),
            Self::ContentTombstone(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::ContentTombstone,
                record.tombstone_id.clone(),
                1,
            ),
            Self::ContentAccessAudit(record) => LlmTraceRecordKey::new(
                LlmTraceRecordKind::ContentAccessAudit,
                record.audit_id.clone(),
                1,
            ),
        }
    }

    pub fn observed_at_ms(&self) -> i64 {
        match self {
            Self::CallStarted(record) => record.observed_at_ms,
            Self::ProviderAttempt(record) => record.observed_at_ms,
            Self::CallCompleted(record) => record.observed_at_ms,
            Self::CaptureGap(record) => record.emitted_at_ms,
            Self::ToolLineage(record) => record.observed_at_ms,
            Self::CallIo(record) => record.observed_at_ms,
            Self::ContextBlock(record) => record.observed_at_ms,
            Self::ContentTombstone(record) => record.observed_at_ms,
            Self::ContentAccessAudit(record) => record.observed_at_ms,
        }
    }

    pub fn scope(&self) -> &LlmScope {
        match self {
            Self::CallStarted(record) => &record.context.scope,
            Self::ProviderAttempt(record) => &record.context.scope,
            Self::CallCompleted(record) => &record.context.scope,
            Self::CaptureGap(record) => &record.scope,
            Self::ToolLineage(record) => &record.context.scope,
            Self::CallIo(record) => &record.context.scope,
            Self::ContextBlock(record) => &record.context.scope,
            Self::ContentTombstone(record) => &record.scope,
            Self::ContentAccessAudit(record) => &record.scope,
        }
    }

    pub fn operation(&self) -> &str {
        match self {
            Self::CallStarted(record) => &record.operation,
            Self::ProviderAttempt(record) => &record.operation,
            Self::CallCompleted(record) => &record.operation,
            Self::CaptureGap(record) => &record.operation,
            Self::ToolLineage(record) => &record.operation,
            Self::CallIo(record) => &record.operation,
            Self::ContextBlock(record) => &record.operation,
            Self::ContentTombstone(_) => "content_deletion",
            Self::ContentAccessAudit(_) => "restricted_content_read",
        }
    }

    pub const fn buffer_class(&self) -> LlmTraceBufferClass {
        match self {
            Self::CallIo(_) => LlmTraceBufferClass::RestrictedPayload,
            Self::ToolLineage(_)
            | Self::ContextBlock(_)
            | Self::ContentTombstone(_)
            | Self::ContentAccessAudit(_) => LlmTraceBufferClass::Lineage,
            Self::CallStarted(_)
            | Self::ProviderAttempt(_)
            | Self::CallCompleted(_)
            | Self::CaptureGap(_) => LlmTraceBufferClass::Critical,
        }
    }

    pub fn capture_status(&self) -> LlmCaptureStatus {
        match self {
            Self::CallStarted(record) => record.capture.status,
            Self::ProviderAttempt(record) => record.capture.status,
            Self::CallCompleted(record) => record.capture.status,
            Self::CaptureGap(_) => LlmCaptureStatus::BackpressureDegraded,
            Self::ToolLineage(_) => LlmCaptureStatus::Complete,
            Self::CallIo(record) => record.capture.status,
            Self::ContextBlock(_) => LlmCaptureStatus::Complete,
            Self::ContentTombstone(_) | Self::ContentAccessAudit(_) => {
                LlmCaptureStatus::MetadataOnly
            },
        }
    }

    pub fn validate(&self) -> Result<(), LlmTraceRecordError> {
        match self {
            Self::CallStarted(record) => validate_call_started(record),
            Self::ProviderAttempt(record) => validate_provider_attempt(record),
            Self::CallCompleted(record) => validate_call_completed(record),
            Self::CaptureGap(record) => validate_capture_gap(record),
            Self::ToolLineage(record) => validate_tool_lineage(record),
            Self::CallIo(record) => validate_call_io(record),
            Self::ContextBlock(record) => validate_context_block(record),
            Self::ContentTombstone(record) => validate_content_tombstone(record),
            Self::ContentAccessAudit(record) => validate_content_access_audit(record),
        }?;

        let serialized_len = serde_json::to_vec(self)?.len();
        let max_bytes = if matches!(self, Self::CallIo(_)) {
            MAX_SERIALIZED_LLM_RESTRICTED_RECORD_BYTES
        } else {
            MAX_SERIALIZED_LLM_TRACE_RECORD_BYTES
        };
        if serialized_len > max_bytes {
            return Err(LlmTraceRecordError::InvalidRecord(format!(
                "serialized record exceeds {max_bytes}-byte limit"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmTraceJournalEnvelope {
    pub journal_schema_version: u16,
    pub sequence: u64,
    pub key: LlmTraceRecordKey,
    pub observed_at_ms: i64,
    pub payload_checksum: String,
    pub record: LlmTraceRecord,
    /// Durable JSON is checksummed over its exact raw `record` value. A typed
    /// f64 may serialize to a shorter equivalent decimal after deserialization,
    /// so disk loaders mark the already raw-verified envelope here. This state
    /// is process-local and is never written back into the journal.
    #[serde(skip)]
    raw_payload_checksum_verified: bool,
}

#[derive(Deserialize)]
struct RawLlmTraceJournalRecord<'a> {
    #[serde(borrow)]
    record: &'a serde_json::value::RawValue,
}

impl LlmTraceJournalEnvelope {
    pub fn new(sequence: u64, record: LlmTraceRecord) -> Result<Self, LlmTraceRecordError> {
        if sequence == 0 {
            return Err(LlmTraceRecordError::InvalidRecord(
                "journal sequence must be one-based".to_string(),
            ));
        }
        record.validate()?;
        let payload_checksum = checksum_record(&record)?;
        Ok(Self {
            journal_schema_version: LLM_TRACE_JOURNAL_SCHEMA_VERSION,
            sequence,
            key: record.key(),
            observed_at_ms: record.observed_at_ms(),
            payload_checksum,
            record,
            raw_payload_checksum_verified: false,
        })
    }

    /// Deserialize and verify one durable JSONL envelope against the exact raw
    /// `record` bytes that were originally checksummed. Typed reserialization
    /// is not byte-stable for every valid f64 decimal representation.
    pub fn from_json_line_verified(line: &[u8]) -> Result<Self, LlmTraceRecordError> {
        let raw: RawLlmTraceJournalRecord<'_> = serde_json::from_slice(line)?;
        let mut envelope: Self = serde_json::from_slice(line)?;
        let actual = checksum_bytes(raw.record.get().as_bytes());
        if actual != envelope.payload_checksum {
            return Err(LlmTraceRecordError::ChecksumMismatch {
                expected: envelope.payload_checksum.clone(),
                actual,
            });
        }
        envelope.raw_payload_checksum_verified = true;
        envelope.verify()?;
        Ok(envelope)
    }

    pub fn verify(&self) -> Result<(), LlmTraceRecordError> {
        if self.journal_schema_version != LLM_TRACE_JOURNAL_SCHEMA_VERSION {
            return Err(LlmTraceRecordError::InvalidRecord(format!(
                "unsupported journal schema version {}",
                self.journal_schema_version
            )));
        }
        if self.sequence == 0 || self.key != self.record.key() {
            return Err(LlmTraceRecordError::InvalidRecord(
                "journal identity does not match its typed record".to_string(),
            ));
        }
        self.record.validate()?;
        let actual = checksum_record(&self.record)?;
        if actual != self.payload_checksum && !self.raw_payload_checksum_verified {
            return Err(LlmTraceRecordError::ChecksumMismatch {
                expected: self.payload_checksum.clone(),
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmCaptureReceipt {
    pub key: LlmTraceRecordKey,
    pub capture_status: LlmCaptureStatus,
    pub accepted: bool,
}

pub trait LlmTraceRecordSink: Send + Sync {
    fn try_record(&self, record: LlmTraceRecord) -> Result<(), LlmTraceRecordError>;
}

pub trait LlmTraceRecorder: Send + Sync {
    fn begin_call(&self, request: LlmCallStarted)
        -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_attempt(
        &self,
        attempt: LlmProviderAttemptEvent,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn complete_call(
        &self,
        response: LlmCallCompleted,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_gap(&self, gap: LlmCaptureGap) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_tool_lineage(
        &self,
        record: LlmToolLineageRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_call_io(
        &self,
        record: LlmCallIoRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_context_block(
        &self,
        record: LlmContextBlockRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_content_tombstone(
        &self,
        record: LlmContentTombstone,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
    fn record_content_access_audit(
        &self,
        record: LlmContentAccessAudit,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError>;
}

pub struct TypedLlmTraceRecorder {
    sink: Arc<dyn LlmTraceRecordSink>,
}

impl TypedLlmTraceRecorder {
    pub fn new(sink: Arc<dyn LlmTraceRecordSink>) -> Self {
        Self { sink }
    }

    fn submit(&self, record: LlmTraceRecord) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        record.validate()?;
        let receipt = LlmCaptureReceipt {
            key: record.key(),
            capture_status: record.capture_status(),
            accepted: true,
        };
        self.sink.try_record(record)?;
        Ok(receipt)
    }
}

impl LlmTraceRecorder for TypedLlmTraceRecorder {
    fn begin_call(
        &self,
        request: LlmCallStarted,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::CallStarted(request))
    }

    fn record_attempt(
        &self,
        attempt: LlmProviderAttemptEvent,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::ProviderAttempt(attempt))
    }

    fn complete_call(
        &self,
        response: LlmCallCompleted,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::CallCompleted(response))
    }

    fn record_gap(&self, gap: LlmCaptureGap) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::CaptureGap(gap))
    }

    fn record_tool_lineage(
        &self,
        record: LlmToolLineageRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::ToolLineage(record))
    }

    fn record_call_io(
        &self,
        record: LlmCallIoRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::CallIo(record))
    }

    fn record_context_block(
        &self,
        record: LlmContextBlockRecord,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::ContextBlock(record))
    }

    fn record_content_tombstone(
        &self,
        record: LlmContentTombstone,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::ContentTombstone(record))
    }

    fn record_content_access_audit(
        &self,
        record: LlmContentAccessAudit,
    ) -> Result<LlmCaptureReceipt, LlmTraceRecordError> {
        self.submit(LlmTraceRecord::ContentAccessAudit(record))
    }
}

#[derive(Debug, Error)]
pub enum LlmTraceRecordError {
    #[error("invalid LLM trace record: {0}")]
    InvalidRecord(String),
    #[error("failed to serialize LLM trace record: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("LLM trace record checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("LLM trace sink rejected record: {0}")]
    Sink(String),
}

fn validate_context(context: &LlmTraceContext) -> Result<(), LlmTraceRecordError> {
    if !context.is_valid() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "trace context or scope is invalid".to_string(),
        ));
    }
    Ok(())
}

fn validate_common(
    schema_version: u16,
    occurred_at_ms: i64,
    observed_at_ms: i64,
) -> Result<(), LlmTraceRecordError> {
    if schema_version != LLM_TRACE_FACT_SCHEMA_VERSION {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "fact schema version must be {LLM_TRACE_FACT_SCHEMA_VERSION}"
        )));
    }
    if occurred_at_ms < 0 || observed_at_ms < occurred_at_ms {
        return Err(LlmTraceRecordError::InvalidRecord(
            "observation time must be at or after a non-negative event time".to_string(),
        ));
    }
    Ok(())
}

fn validate_call_io(record: &LlmCallIoRecord) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_restricted_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    require_machine_category("operation", &record.operation)?;
    require_machine_category("retention_class", &record.retention_class)?;
    record.capture.validate()?;
    if !matches!(record.capture.mode, LlmCaptureMode::Sanitized)
        || !matches!(
            record.capture.status,
            LlmCaptureStatus::Complete | LlmCaptureStatus::Redacted | LlmCaptureStatus::Oversize
        )
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "call I/O records require sanitized capture with complete, redacted, or oversize status"
                .to_string(),
        ));
    }
    record.phase.revision(record.provider_attempt_index)?;
    if record.phase == LlmCallIoPhase::LogicalRequest && record.provider_attempt_index.is_some() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "logical request cannot carry a provider attempt index".to_string(),
        ));
    }
    validate_redaction_report(&record.redaction)?;
    for (name, fingerprint) in [
        (
            "logical_request_fingerprint",
            record.logical_request_fingerprint.as_deref(),
        ),
        (
            "effective_request_fingerprint",
            record.effective_request_fingerprint.as_deref(),
        ),
        (
            "response_fingerprint",
            record.response_fingerprint.as_deref(),
        ),
    ] {
        if let Some(fingerprint) = fingerprint {
            require_fingerprint(name, fingerprint)?;
        }
    }
    let required_fingerprint = match record.phase {
        LlmCallIoPhase::LogicalRequest => record.logical_request_fingerprint.as_ref(),
        LlmCallIoPhase::EffectiveRequest => record.effective_request_fingerprint.as_ref(),
        LlmCallIoPhase::NormalizedResponse => record.response_fingerprint.as_ref(),
    };
    if required_fingerprint.is_none() || record.payload.is_null() || record.sanitized_bytes == 0 {
        return Err(LlmTraceRecordError::InvalidRecord(
            "call I/O phase requires its fingerprint and a non-empty sanitized payload".to_string(),
        ));
    }
    if record.sanitized_bytes > record.original_bytes.saturating_add(64 * 1024) {
        return Err(LlmTraceRecordError::InvalidRecord(
            "sanitized payload expansion exceeds the bounded normalization allowance".to_string(),
        ));
    }
    Ok(())
}

fn validate_tool_lineage(record: &LlmToolLineageRecord) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    record.stage.revision(record.stage_index)?;
    require_stable_identifier("model_tool_call_id", &record.model_tool_call_id)?;
    require_stable_identifier("tool_execution_id", &record.tool_execution_id)?;
    let expected_tool_execution_id = format!(
        "{}:tool:{}",
        record.context.llm_call_id, record.model_tool_call_id
    );
    if record.tool_execution_id != expected_tool_execution_id {
        return Err(LlmTraceRecordError::InvalidRecord(
            "tool_execution_id must be derived from llm_call_id and model_tool_call_id".to_string(),
        ));
    }
    require_stable_identifier("branch_id", &record.branch_id)?;
    for (name, value) in [
        ("operation", record.operation.as_str()),
        ("source_surface", record.source_surface.as_str()),
        ("tool_name", record.tool_name.as_str()),
    ] {
        require_machine_category(name, value)?;
    }
    require_machine_category_option("tool_family", record.tool_family.as_deref())?;
    require_machine_category_option("failure_code", record.failure_code.as_deref())?;
    require_machine_category_option("linkage_gap", record.linkage_gap.as_deref())?;
    require_stable_identifier_option("result_ref", record.result_ref.as_deref())?;
    require_stable_identifier_option("canonical_event_ref", record.canonical_event_ref.as_deref())?;
    if record.related_execution_ids.len() > MAX_TOOL_RELATED_EXECUTION_IDS {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "related execution ids must contain at most {MAX_TOOL_RELATED_EXECUTION_IDS} entries"
        )));
    }
    let mut seen_related_execution_ids = HashSet::new();
    for execution_id in &record.related_execution_ids {
        require_stable_identifier("related_execution_id", execution_id)?;
        if execution_id.len() > MAX_TOOL_RELATED_EXECUTION_ID_BYTES {
            return Err(LlmTraceRecordError::InvalidRecord(format!(
                "related execution ids must be at most {MAX_TOOL_RELATED_EXECUTION_ID_BYTES} bytes"
            )));
        }
        if !seen_related_execution_ids.insert(execution_id) {
            return Err(LlmTraceRecordError::InvalidRecord(
                "related execution ids must be deduplicated".to_string(),
            ));
        }
    }
    if !record.related_execution_ids.is_empty()
        && !matches!(
            record.stage,
            LlmToolLineageStage::ExecutionFinished | LlmToolLineageStage::BranchMaterialized
        )
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "related execution ids are legal only on execution-finished or branch lineage"
                .to_string(),
        ));
    }
    require_stable_identifier_option("consumed_by_call_id", record.consumed_by_call_id.as_deref())?;
    require_fingerprint(
        "arguments_fingerprint",
        record.arguments_fingerprint.as_deref().ok_or_else(|| {
            LlmTraceRecordError::InvalidRecord(
                "tool lineage requires an arguments fingerprint at every stage".to_string(),
            )
        })?,
    )?;
    if record.same_tool_arguments_count == 0 {
        return Err(LlmTraceRecordError::InvalidRecord(
            "same_tool_arguments_count must be at least one".to_string(),
        ));
    }
    let required_stage_fact = match record.stage {
        LlmToolLineageStage::NameValidated => record.name_known,
        LlmToolLineageStage::ArgumentsParsed => record.arguments_parsed,
        LlmToolLineageStage::SchemaValidated => record.schema_matched,
        LlmToolLineageStage::AuthorizationResolved => record.policy_allowed,
        LlmToolLineageStage::ExecutionStarted => record.transport_ran,
        LlmToolLineageStage::ExecutionFinished => record.tool_reported_success,
        LlmToolLineageStage::ResultValidated => record.result_validation_success,
        _ => Some(true),
    };
    if required_stage_fact.is_none() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "tool-lineage validation stage requires its independent classification fact"
                .to_string(),
        ));
    }
    if matches!(record.stage, LlmToolLineageStage::ApprovalResolved)
        && (record.approval_required.is_none() || record.approval_obtained.is_none())
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "approval-resolved lineage requires approval_required and approval_obtained"
                .to_string(),
        ));
    }
    if record.approval_obtained == Some(true) && record.approval_required != Some(true) {
        return Err(LlmTraceRecordError::InvalidRecord(
            "approval can be obtained only when it was required".to_string(),
        ));
    }
    match record.stage {
        LlmToolLineageStage::ResultConsumed => {
            if record.consumed_by_call_id.is_none() || record.result_ref.is_none() {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "result-consumed lineage requires result_ref and consumed_by_call_id"
                        .to_string(),
                ));
            }
            if record.consumed_by_call_id.as_deref() == Some(record.context.llm_call_id.as_str()) {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "a tool result must be consumed by a later logical call".to_string(),
                ));
            }
        },
        _ if record.consumed_by_call_id.is_some() => {
            return Err(LlmTraceRecordError::InvalidRecord(
                "consumed_by_call_id is legal only on result-consumed lineage".to_string(),
            ));
        },
        _ => {},
    }
    match record.stage {
        LlmToolLineageStage::LinkageGap if record.linkage_gap.is_none() => {
            return Err(LlmTraceRecordError::InvalidRecord(
                "linkage-gap stage requires a machine-readable linkage_gap".to_string(),
            ));
        },
        LlmToolLineageStage::LinkageGap => {},
        _ if record.linkage_gap.is_some() => {
            return Err(LlmTraceRecordError::InvalidRecord(
                "linkage_gap is legal only on linkage-gap lineage".to_string(),
            ));
        },
        _ => {},
    }
    let failure_outcome = matches!(
        record.outcome,
        LlmToolLineageOutcome::Failed
            | LlmToolLineageOutcome::Denied
            | LlmToolLineageOutcome::Cancelled
            | LlmToolLineageOutcome::TimedOut
    );
    if failure_outcome != record.failure_owner.is_some() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "failed, denied, cancelled, or timed-out lineage requires exactly one failure owner"
                .to_string(),
        ));
    }
    if matches!(
        record.side_effect_state,
        LlmToolSideEffectState::Reversed | LlmToolSideEffectState::RollbackFailed
    ) && !matches!(record.stage, LlmToolLineageStage::RollbackFinished)
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "terminal rollback state is legal only on rollback-finished lineage".to_string(),
        ));
    }
    // `Remained` asserts that a side effect happened and stands. Only a record
    // that also says a transport ran can support that, and the claim is worth
    // guarding rather than trusting: it is the one state a reader will act on
    // without further checking, and a row claiming it for a dispatch that never
    // left the process would be worse than the `unknown` this variant replaced.
    if matches!(record.side_effect_state, LlmToolSideEffectState::Remained)
        && record.transport_ran != Some(true)
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "a remained side effect requires a record that a transport ran".to_string(),
        ));
    }
    if matches!(record.branch_state, LlmToolBranchState::Successful)
        && record.on_successful_path != Some(true)
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "successful branch state requires on_successful_path=true".to_string(),
        ));
    }
    Ok(())
}

fn validate_context_block(record: &LlmContextBlockRecord) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_restricted_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    require_machine_category("operation", &record.operation)?;
    require_stable_identifier("context_block_id", &record.context_block_id)?;
    for (name, value) in [
        ("role", record.role.as_str()),
        ("block_kind", record.block_kind.as_str()),
        ("source_kind", record.source_kind.as_str()),
        ("transformation", record.transformation.as_str()),
    ] {
        require_machine_category(name, value)?;
    }
    require_stable_identifier_option("source_id", record.source_id.as_deref())?;
    require_stable_identifier_option("payload_ref", record.payload_ref.as_deref())?;
    require_machine_category_option("truncation_reason", record.truncation_reason.as_deref())?;
    require_fingerprint("original_fingerprint", &record.original_fingerprint)?;
    require_fingerprint("effective_fingerprint", &record.effective_fingerprint)?;
    validate_machine_categories("redaction_categories", &record.redaction_categories)?;
    Ok(())
}

fn validate_content_tombstone(record: &LlmContentTombstone) -> Result<(), LlmTraceRecordError> {
    validate_restricted_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    if !record.scope.is_valid() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "content tombstone scope must be valid".to_string(),
        ));
    }
    require_stable_identifier("tombstone_id", &record.tombstone_id)?;
    require_machine_category("target_kind", &record.target_kind)?;
    require_stable_identifier("target_id", &record.target_id)?;
    require_machine_category("reason", &record.reason)
}

fn validate_content_access_audit(
    record: &LlmContentAccessAudit,
) -> Result<(), LlmTraceRecordError> {
    validate_restricted_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    if !record.scope.is_valid() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "content access audit scope must be valid".to_string(),
        ));
    }
    require_stable_identifier("audit_id", &record.audit_id)?;
    require_stable_identifier("actor_id", &record.actor_id)?;
    require_stable_identifier_option("execution_id", record.execution_id.as_deref())?;
    require_machine_category("target_kind", &record.target_kind)?;
    require_stable_identifier("target_id", &record.target_id)?;
    require_machine_category("content_kind", &record.content_kind)?;
    require_machine_category("redaction_version", &record.redaction_version)?;
    require_machine_category("outcome", &record.outcome)?;
    if record.reason.trim().is_empty()
        || record.reason.len() > 256
        || record.reason.chars().any(char::is_control)
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "content access audit reason must contain 1..=256 non-control bytes".to_string(),
        ));
    }
    Ok(())
}

fn validate_restricted_common(
    schema_version: u16,
    occurred_at_ms: i64,
    observed_at_ms: i64,
) -> Result<(), LlmTraceRecordError> {
    if schema_version != LLM_RESTRICTED_CONTENT_SCHEMA_VERSION {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "restricted content schema version must be {LLM_RESTRICTED_CONTENT_SCHEMA_VERSION}"
        )));
    }
    if occurred_at_ms < 0 || observed_at_ms < occurred_at_ms {
        return Err(LlmTraceRecordError::InvalidRecord(
            "restricted observation time must be at or after a non-negative event time".to_string(),
        ));
    }
    Ok(())
}

fn validate_redaction_report(report: &LlmRedactionReport) -> Result<(), LlmTraceRecordError> {
    require_machine_category("redaction policy version", &report.policy_version)?;
    validate_machine_categories("redaction categories", &report.categories)?;
    if usize::try_from(report.redaction_count).unwrap_or(usize::MAX) < report.categories.len() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "redaction count cannot be smaller than its category count".to_string(),
        ));
    }
    Ok(())
}

fn validate_machine_categories(name: &str, values: &[String]) -> Result<(), LlmTraceRecordError> {
    let mut previous: Option<&str> = None;
    for value in values {
        require_machine_category(name, value)?;
        if previous.is_some_and(|previous| previous >= value.as_str()) {
            return Err(LlmTraceRecordError::InvalidRecord(format!(
                "{name} must be sorted and deduplicated"
            )));
        }
        previous = Some(value);
    }
    Ok(())
}

fn require_fingerprint(name: &str, value: &str) -> Result<(), LlmTraceRecordError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "{name} must be 64 lowercase hex characters"
        )));
    }
    Ok(())
}

fn validate_call_started(record: &LlmCallStarted) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    require_machine_category("operation", &record.operation)?;
    require_nonblank_options(&[
        ("operation_family", record.operation_family.as_deref()),
        ("capability", record.capability.as_deref()),
        ("priority_lane", record.priority_lane.as_deref()),
        ("source_surface", record.source_surface.as_deref()),
        ("origin_channel", record.origin_channel.as_deref()),
    ])?;
    require_stable_identifier_option("requested_profile", record.requested_profile.as_deref())?;
    require_stable_identifier_option("selected_profile", record.selected_profile.as_deref())?;
    if record
        .prompt_projection_mode
        .as_deref()
        .is_some_and(|value| !matches!(value, "bootstrap" | "continuation" | "rebootstrap"))
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "prompt projection mode must be bootstrap, continuation, or rebootstrap".to_string(),
        ));
    }
    for (name, value) in [
        ("operation_family", record.operation_family.as_deref()),
        ("capability", record.capability.as_deref()),
        ("priority_lane", record.priority_lane.as_deref()),
        ("source_surface", record.source_surface.as_deref()),
        ("origin_channel", record.origin_channel.as_deref()),
    ] {
        require_machine_category_option(name, value)?;
    }
    record.capture.validate()
}

fn validate_provider_attempt(record: &LlmProviderAttemptEvent) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    if record.provider_attempt_index == 0 {
        return Err(LlmTraceRecordError::InvalidRecord(
            "provider attempt index must be one-based".to_string(),
        ));
    }
    let expected = record
        .context
        .provider_attempt_id(record.provider_attempt_index);
    if record.provider_attempt_id != expected {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "provider attempt id must be {expected}"
        )));
    }
    require_machine_category("operation", &record.operation)?;
    require_stable_identifier("provider", &record.provider)?;
    require_stable_identifier("model", &record.model)?;
    if let Some(dispatch_job_id) = record.dispatch_job_id.as_deref() {
        require_stable_identifier("dispatch_job_id", dispatch_job_id)?;
    }
    require_nonblank_options(&[
        ("error_class", record.error_class.as_deref()),
        ("error_code", record.error_code.as_deref()),
        ("finish_reason", record.finish_reason.as_deref()),
    ])?;
    require_stable_identifier_option("effective_profile", record.effective_profile.as_deref())?;
    require_stable_identifier_option("model_revision", record.model_revision.as_deref())?;
    require_machine_category_option("error_class", record.error_class.as_deref())?;
    require_machine_category_option("error_code", record.error_code.as_deref())?;
    require_machine_category_option("finish_reason", record.finish_reason.as_deref())?;
    record.timing.validate()?;
    record.usage.validate()?;
    record.pricing.validate()?;
    record.capture.validate()?;
    match record.phase {
        LlmProviderAttemptPhase::Started => {
            if record.timing.started_at_ms != Some(record.occurred_at_ms)
                || record.timing.first_token_at_ms.is_some()
                || record.timing.completed_at_ms.is_some()
                || record.timing.ttft_ms.is_some()
                || record.timing.generation_after_ttft_ms.is_some()
                || record.timing.provider_execution_ms.is_some()
                || record.timing.parse_ms.is_some()
                || record.timing.validation_ms.is_some()
                || record.timing.latency_ms.is_some()
                || record.terminal_state.is_some()
                || has_preterminal_attempt_payload(record)
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "attempt start requires an exact started_at_ms and no terminal payload"
                        .to_string(),
                ));
            }
        },
        LlmProviderAttemptPhase::FirstToken => {
            if record.timing.started_at_ms.is_none()
                || record.timing.first_token_at_ms != Some(record.occurred_at_ms)
                || record.timing.ttft_ms.is_none()
                || record.timing.completed_at_ms.is_some()
                || record.timing.generation_after_ttft_ms.is_some()
                || record.timing.provider_execution_ms.is_some()
                || record.timing.parse_ms.is_some()
                || record.timing.validation_ms.is_some()
                || record.timing.latency_ms.is_some()
                || record.terminal_state.is_some()
                || has_preterminal_attempt_payload(record)
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "first-token attempt revision requires exact start/first-token times and no terminal payload"
                        .to_string(),
                ));
            }
        },
        LlmProviderAttemptPhase::Completed => {
            if record.timing.completed_at_ms != Some(record.occurred_at_ms)
                || record.timing.first_token_at_ms.is_some() != record.timing.ttft_ms.is_some()
                || record.terminal_state.is_none()
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "completed attempt requires an exact completion time, paired first-token fields, and terminal state"
                        .to_string(),
                ));
            }
            if record.timing.started_at_ms.is_some() != record.timing.latency_ms.is_some() {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "completed attempt start and latency must be populated together when exact attempt timing is known"
                        .to_string(),
                ));
            }
            if record.timing.started_at_ms.is_none()
                && (record.timing.first_token_at_ms.is_some()
                    || record.timing.provider_execution_ms.is_some()
                    || record.timing.generation_after_ttft_ms.is_some())
            {
                return Err(LlmTraceRecordError::InvalidRecord(
                    "completed attempt cannot carry derived provider timing without an exact start"
                        .to_string(),
                ));
            }
            match record.terminal_state {
                Some(LlmAttemptTerminalState::Succeeded) if record.error_class.is_some() => {
                    return Err(LlmTraceRecordError::InvalidRecord(
                        "successful provider attempt cannot carry an error_class".to_string(),
                    ));
                },
                Some(
                    LlmAttemptTerminalState::Failed
                    | LlmAttemptTerminalState::Cancelled
                    | LlmAttemptTerminalState::TimedOut,
                ) if record.error_class.is_none() => {
                    return Err(LlmTraceRecordError::InvalidRecord(
                        "failed, cancelled, or timed-out provider attempt requires an error_class"
                            .to_string(),
                    ));
                },
                _ => {},
            }
        },
    }
    Ok(())
}

fn validate_call_completed(record: &LlmCallCompleted) -> Result<(), LlmTraceRecordError> {
    validate_context(&record.context)?;
    validate_common(
        record.schema_version,
        record.occurred_at_ms,
        record.observed_at_ms,
    )?;
    require_machine_category("operation", &record.operation)?;
    if let Some(dispatch_job_id) = record.dispatch_job_id.as_deref() {
        require_stable_identifier("dispatch_job_id", dispatch_job_id)?;
    }
    require_nonblank_options(&[
        ("response_kind", record.response_kind.as_deref()),
        ("error_class", record.error_class.as_deref()),
        ("error_code", record.error_code.as_deref()),
        ("finish_reason", record.finish_reason.as_deref()),
    ])?;
    require_stable_identifier_option(
        "provider_response_id",
        record.provider_response_id.as_deref(),
    )?;
    require_machine_category_option("response_kind", record.response_kind.as_deref())?;
    require_machine_category_option("error_class", record.error_class.as_deref())?;
    require_machine_category_option("error_code", record.error_code.as_deref())?;
    require_machine_category_option("finish_reason", record.finish_reason.as_deref())?;
    record.timing.validate()?;
    record.usage.validate()?;
    record.pricing.validate()?;
    record.validation.validate()?;
    record.capture.validate()?;
    if record.timing.created_at_ms.is_none()
        || record.timing.completed_at_ms != Some(record.occurred_at_ms)
        || record.timing.latency_ms.is_none()
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "completed call requires created_at_ms, total latency, and an event time equal to timing.completed_at_ms"
                .to_string(),
        ));
    }
    let is_non_billing_logical_summary =
        record.response_kind.as_deref() == Some(LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND);
    let is_harness_aggregate =
        record.response_kind.as_deref() == Some(LLM_HARNESS_AGGREGATE_RESPONSE_KIND);
    if is_harness_aggregate
        && (record.provider_attempt_count != 0
            || record.provider_response_id.is_some()
            || record.dispatch_job_id.is_some()
            || !matches!(
                record.pricing.cost_source,
                Some(LlmCostSource::Estimated | LlmCostSource::Unknown)
            ))
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "harness aggregate requires estimated/unknown pricing and no physical attempts".into(),
        ));
    }
    if record.terminal_state == LlmCallTerminalState::Succeeded
        && record.provider_attempt_count == 0
        && !is_non_billing_logical_summary
        && !is_harness_aggregate
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "successful call must have at least one provider attempt".to_string(),
        ));
    }
    if is_non_billing_logical_summary
        && (record.provider_attempt_count != 0
            || record.usage != LlmTokenUsageFact::default()
            || record.pricing.cost_usd.is_some())
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "logical chunk summary must be a zero-attempt, zero-usage, non-billing call"
                .to_string(),
        ));
    }
    if record.terminal_state == LlmCallTerminalState::Succeeded
        && !record.validation.response_present
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "successful call must report a response present at the caller boundary".to_string(),
        ));
    }
    match record.terminal_state {
        LlmCallTerminalState::Succeeded if record.error_class.is_some() => {
            return Err(LlmTraceRecordError::InvalidRecord(
                "successful call cannot carry an error_class".to_string(),
            ));
        },
        LlmCallTerminalState::Failed
        | LlmCallTerminalState::Cancelled
        | LlmCallTerminalState::Tombstoned
            if record.error_class.is_none() =>
        {
            return Err(LlmTraceRecordError::InvalidRecord(
                "failed, cancelled, or tombstoned call requires an error_class".to_string(),
            ));
        },
        _ => {},
    }
    Ok(())
}

fn has_preterminal_attempt_payload(record: &LlmProviderAttemptEvent) -> bool {
    record.error_class.is_some()
        || record.error_code.is_some()
        || record.finish_reason.is_some()
        || record.refusal.is_some()
        || record.truncated.is_some()
        || record.usage != LlmTokenUsageFact::default()
        || record.pricing != LlmPricingFact::default()
}

fn validate_capture_gap(record: &LlmCaptureGap) -> Result<(), LlmTraceRecordError> {
    if record.schema_version != LLM_TRACE_FACT_SCHEMA_VERSION {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "gap schema version must be {LLM_TRACE_FACT_SCHEMA_VERSION}"
        )));
    }
    require_stable_identifier("gap_id", &record.gap_id)?;
    if !record.scope.is_valid() {
        return Err(LlmTraceRecordError::InvalidRecord(
            "gap scope is invalid".to_string(),
        ));
    }
    require_stable_identifier_option("llm_call_id", record.llm_call_id.as_deref())?;
    require_machine_category("operation", &record.operation)?;
    require_nonempty("reason", &record.reason)?;
    require_machine_category("reason", &record.reason)?;
    if record.missing_record_count == 0 {
        return Err(LlmTraceRecordError::InvalidRecord(
            "capture gap count must be greater than zero".to_string(),
        ));
    }
    if record.first_observed_at_ms < 0
        || record.first_observed_at_ms > record.last_observed_at_ms
        || record.last_observed_at_ms > record.emitted_at_ms
    {
        return Err(LlmTraceRecordError::InvalidRecord(
            "capture gap timestamps must be non-negative and ordered".to_string(),
        ));
    }
    Ok(())
}

fn require_nonempty(name: &str, value: &str) -> Result<(), LlmTraceRecordError> {
    if value.trim().is_empty() {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "{name} must not be empty"
        )));
    }
    Ok(())
}

fn require_stable_identifier(name: &str, value: &str) -> Result<(), LlmTraceRecordError> {
    if value.is_empty() || value.trim() != value {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "{name} must be a canonical nonblank identifier"
        )));
    }
    Ok(())
}

fn require_stable_identifier_option(
    name: &str,
    value: Option<&str>,
) -> Result<(), LlmTraceRecordError> {
    value.map_or(Ok(()), |value| require_stable_identifier(name, value))
}

fn require_nonblank_options(values: &[(&str, Option<&str>)]) -> Result<(), LlmTraceRecordError> {
    for (name, value) in values {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(LlmTraceRecordError::InvalidRecord(format!(
                "{name} must not be blank"
            )));
        }
    }
    Ok(())
}

/// Ordinary Phase 2 facts are content-free. Fields that represent categories
/// must therefore remain short machine tokens; accepting arbitrary prose here
/// would let an upstream parser/provider error smuggle prompt or response text
/// into the supposedly metadata-only journal.
fn require_machine_category(name: &str, value: &str) -> Result<(), LlmTraceRecordError> {
    if !is_content_free_machine_category(value) {
        return Err(LlmTraceRecordError::InvalidRecord(format!(
            "{name} must be a content-free machine category of at most {MAX_MACHINE_CATEGORY_BYTES} bytes"
        )));
    }
    Ok(())
}

pub fn is_content_free_machine_category(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= MAX_MACHINE_CATEGORY_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn require_machine_category_option(
    name: &str,
    value: Option<&str>,
) -> Result<(), LlmTraceRecordError> {
    value.map_or(Ok(()), |value| require_machine_category(name, value))
}

fn checksum_record(record: &LlmTraceRecord) -> Result<String, LlmTraceRecordError> {
    let bytes = serde_json::to_vec(record)?;
    Ok(checksum_bytes(&bytes))
}

fn checksum_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Mutex;

    use magicllm::{LlmScope, LlmWorkloadClass};

    use super::*;

    #[derive(Default)]
    struct CollectingSink {
        records: Mutex<Vec<LlmTraceRecord>>,
    }

    impl LlmTraceRecordSink for CollectingSink {
        fn try_record(&self, record: LlmTraceRecord) -> Result<(), LlmTraceRecordError> {
            self.records.lock().expect("records").push(record);
            Ok(())
        }
    }

    fn context() -> LlmTraceContext {
        LlmTraceContext::new(
            LlmScope::new("principal", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        )
    }

    fn completed_call(context: LlmTraceContext) -> LlmCallCompleted {
        LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: Some("job-1".to_string()),
            occurred_at_ms: 30,
            observed_at_ms: 31,
            operation: "chat".to_string(),
            terminal_state: LlmCallTerminalState::Succeeded,
            provider_attempt_count: 1,
            provider_response_id: Some("response-1".to_string()),
            response_kind: Some("text".to_string()),
            error_class: None,
            error_code: None,
            finish_reason: Some("stop".to_string()),
            refusal: Some(false),
            truncated: Some(false),
            timing: LlmTimingFact {
                created_at_ms: Some(10),
                submitted_at_ms: Some(11),
                started_at_ms: Some(12),
                first_token_at_ms: Some(20),
                completed_at_ms: Some(30),
                queue_wait_ms: Some(1),
                provider_execution_ms: Some(18),
                ttft_ms: Some(8),
                generation_after_ttft_ms: Some(10),
                latency_ms: Some(20),
                ..LlmTimingFact::default()
            },
            usage: LlmTokenUsageFact {
                input_tokens: Some(100),
                output_tokens: Some(20),
                total_tokens: Some(120),
                ..LlmTokenUsageFact::default()
            },
            pricing: LlmPricingFact {
                pricing_version: Some("2026-07-22".to_string()),
                cost_source: Some(LlmCostSource::Computed),
                cost_usd: Some(0.01),
                ..LlmPricingFact::default()
            },
            validation: LlmImmediateValidationFact {
                response_present: true,
                parse_attempted: true,
                parse_success: Some(true),
                ..LlmImmediateValidationFact::default()
            },
            capture: LlmCaptureFact::default(),
        }
    }

    fn tool_lineage(stage: LlmToolLineageStage, stage_index: u32) -> LlmToolLineageRecord {
        let context = context();
        LlmToolLineageRecord {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            tool_execution_id: format!("{}:tool:call_1", context.llm_call_id),
            context,
            model_tool_call_id: "call_1".to_string(),
            branch_id: "branch_1".to_string(),
            operation: "agentic_decision".to_string(),
            source_surface: "interactive_task".to_string(),
            tool_name: "browser__click".to_string(),
            tool_family: Some("browser".to_string()),
            stage,
            stage_index,
            occurred_at_ms: 10,
            observed_at_ms: 11,
            arguments_fingerprint: Some("a".repeat(64)),
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids: Vec::new(),
            consumed_by_call_id: None,
            name_known: None,
            arguments_parsed: None,
            schema_matched: None,
            policy_allowed: None,
            approval_required: None,
            approval_obtained: None,
            transport_ran: None,
            tool_reported_success: None,
            result_validation_success: None,
            outcome: LlmToolLineageOutcome::Pending,
            failure_owner: None,
            failure_code: None,
            side_effect_state: LlmToolSideEffectState::None,
            branch_state: LlmToolBranchState::Active,
            on_successful_path: None,
            same_tool_arguments_count: 1,
            observation_action_cycle_count: 0,
            recovered_after_failure: false,
            linkage_gap: None,
        }
    }

    #[test]
    fn tool_lineage_revision_ranges_keep_attempts_consumers_and_rollbacks_distinct() {
        let cases = [
            (LlmToolLineageStage::Proposed, 0, 1),
            (LlmToolLineageStage::ExecutionStarted, 1, 1_001),
            (LlmToolLineageStage::ExecutionFinished, 7, 2_007),
            (LlmToolLineageStage::ResultConsumed, 2, 4_002),
            (LlmToolLineageStage::RollbackStarted, 3, 6_003),
            (LlmToolLineageStage::RollbackFinished, 3, 7_003),
            (LlmToolLineageStage::LinkageGap, 1, 8_001),
        ];
        for (stage, index, revision) in cases {
            assert_eq!(stage.revision(index).expect("valid revision"), revision);
        }
        assert!(LlmToolLineageStage::Proposed.revision(1).is_err());
        assert!(LlmToolLineageStage::ResultConsumed.revision(0).is_err());
        assert!(LlmToolLineageStage::ResultConsumed.revision(1_000).is_err());
    }

    #[test]
    fn tool_lineage_validates_independent_parse_policy_approval_and_transport_outcomes() {
        let mut parse = tool_lineage(LlmToolLineageStage::ArgumentsParsed, 0);
        parse.arguments_parsed = Some(false);
        parse.outcome = LlmToolLineageOutcome::Failed;
        parse.failure_owner = Some(LlmToolFailureOwner::Arguments);
        parse.failure_code = Some("invalid_json".to_string());
        LlmTraceRecord::ToolLineage(parse)
            .validate()
            .expect("parse failure");

        let mut policy = tool_lineage(LlmToolLineageStage::AuthorizationResolved, 0);
        policy.policy_allowed = Some(false);
        policy.outcome = LlmToolLineageOutcome::Denied;
        policy.failure_owner = Some(LlmToolFailureOwner::Policy);
        policy.failure_code = Some("action_not_allowed".to_string());
        LlmTraceRecord::ToolLineage(policy)
            .validate()
            .expect("policy denial");

        let mut approval = tool_lineage(LlmToolLineageStage::ApprovalResolved, 0);
        approval.approval_required = Some(true);
        approval.approval_obtained = Some(false);
        approval.outcome = LlmToolLineageOutcome::Denied;
        approval.failure_owner = Some(LlmToolFailureOwner::Approval);
        approval.failure_code = Some("approval_denied".to_string());
        LlmTraceRecord::ToolLineage(approval)
            .validate()
            .expect("approval denial");

        let mut outage = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
        outage.transport_ran = Some(true);
        outage.tool_reported_success = Some(false);
        outage.outcome = LlmToolLineageOutcome::Failed;
        outage.failure_owner = Some(LlmToolFailureOwner::ExternalProvider);
        outage.failure_code = Some("upstream_unavailable".to_string());
        outage.result_ref = Some("result_1".to_string());
        outage.canonical_event_ref = Some("event_1".to_string());
        LlmTraceRecord::ToolLineage(outage)
            .validate()
            .expect("external outage");

        let mut tool_succeeded = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
        tool_succeeded.transport_ran = Some(true);
        tool_succeeded.tool_reported_success = Some(true);
        tool_succeeded.outcome = LlmToolLineageOutcome::Succeeded;
        tool_succeeded.result_ref = Some("result_2".to_string());
        LlmTraceRecord::ToolLineage(tool_succeeded)
            .validate()
            .expect("tool execution can succeed before result validation");

        let mut invalid_result = tool_lineage(LlmToolLineageStage::ResultValidated, 0);
        invalid_result.result_validation_success = Some(false);
        invalid_result.outcome = LlmToolLineageOutcome::Failed;
        invalid_result.failure_owner = Some(LlmToolFailureOwner::ResultValidation);
        invalid_result.failure_code = Some("result_contract_validation_failed".to_string());
        invalid_result.result_ref = Some("result_2".to_string());
        LlmTraceRecord::ToolLineage(invalid_result)
            .validate()
            .expect("result validation remains independent of tool execution");
    }

    #[test]
    fn tool_result_consumption_is_an_explicit_repeatable_edge() {
        let mut first = tool_lineage(LlmToolLineageStage::ResultConsumed, 1);
        first.result_ref = Some("result_1".to_string());
        first.consumed_by_call_id = Some("next_call_1".to_string());
        first.outcome = LlmToolLineageOutcome::Succeeded;
        let mut second = first.clone();
        second.stage_index = 2;
        second.consumed_by_call_id = Some("next_call_2".to_string());
        let first = LlmTraceRecord::ToolLineage(first);
        let second = LlmTraceRecord::ToolLineage(second);
        first.validate().expect("first consumer");
        second.validate().expect("second consumer");
        assert_ne!(first.key().revision, second.key().revision);
        assert_eq!(first.key().stable_id, second.key().stable_id);

        let mut self_consumed = tool_lineage(LlmToolLineageStage::ResultConsumed, 1);
        self_consumed.result_ref = Some("result_1".to_string());
        self_consumed.consumed_by_call_id = Some(self_consumed.context.llm_call_id.clone());
        self_consumed.outcome = LlmToolLineageOutcome::Succeeded;
        assert!(LlmTraceRecord::ToolLineage(self_consumed)
            .validate()
            .is_err());
    }

    #[test]
    fn tool_lineage_rejects_invented_execution_identity_and_duplicate_children() {
        let mut identity = tool_lineage(LlmToolLineageStage::Proposed, 0);
        identity.tool_execution_id = "unrelated".to_string();
        assert!(LlmTraceRecord::ToolLineage(identity).validate().is_err());

        let mut children = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
        children.transport_ran = Some(true);
        children.tool_reported_success = Some(true);
        children.outcome = LlmToolLineageOutcome::Succeeded;
        children.result_ref = Some("result_1".to_string());
        children.related_execution_ids = vec!["child_1".to_string(), "child_1".to_string()];
        assert!(LlmTraceRecord::ToolLineage(children).validate().is_err());
    }

    #[test]
    fn a_remained_side_effect_must_be_backed_by_a_transport_having_run() {
        // `Remained` is the one state a reader acts on without checking
        // anything else — it asserts the effect happened and stands. A row
        // claiming it for a dispatch that never left the process would be worse
        // than the `unknown` this variant replaced, so the claim is guarded
        // rather than trusted.
        let mut ran = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
        ran.transport_ran = Some(true);
        ran.tool_reported_success = Some(true);
        ran.outcome = LlmToolLineageOutcome::Succeeded;
        ran.result_ref = Some("result_1".to_string());
        ran.side_effect_state = LlmToolSideEffectState::Remained;
        LlmTraceRecord::ToolLineage(ran)
            .validate()
            .expect("a dispatch that ran may claim its effect stands");

        for transport_ran in [None, Some(false)] {
            let mut never_ran = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
            never_ran.transport_ran = transport_ran;
            never_ran.tool_reported_success = Some(true);
            never_ran.outcome = LlmToolLineageOutcome::Succeeded;
            never_ran.result_ref = Some("result_1".to_string());
            never_ran.side_effect_state = LlmToolSideEffectState::Remained;
            assert!(
                LlmTraceRecord::ToolLineage(never_ran).validate().is_err(),
                "transport_ran={transport_ran:?} must not support a remained effect"
            );
        }
    }

    #[test]
    fn abandoned_branch_and_rollback_are_not_conflated_with_tool_failure() {
        let mut abandoned = tool_lineage(LlmToolLineageStage::BranchMaterialized, 0);
        abandoned.outcome = LlmToolLineageOutcome::Abandoned;
        abandoned.branch_state = LlmToolBranchState::Abandoned;
        abandoned.on_successful_path = Some(false);
        LlmTraceRecord::ToolLineage(abandoned)
            .validate()
            .expect("abandoned branch");

        let mut reversed = tool_lineage(LlmToolLineageStage::RollbackFinished, 1);
        reversed.outcome = LlmToolLineageOutcome::Succeeded;
        reversed.side_effect_state = LlmToolSideEffectState::Reversed;
        LlmTraceRecord::ToolLineage(reversed)
            .validate()
            .expect("successful rollback");

        let mut failed = tool_lineage(LlmToolLineageStage::RollbackFinished, 1);
        failed.outcome = LlmToolLineageOutcome::Failed;
        failed.failure_owner = Some(LlmToolFailureOwner::Runtime);
        failed.failure_code = Some("rollback_failed".to_string());
        failed.side_effect_state = LlmToolSideEffectState::RollbackFailed;
        LlmTraceRecord::ToolLineage(failed)
            .validate()
            .expect("failed rollback");
    }

    #[test]
    fn cross_execution_delegation_ids_are_fact_only_and_stage_bound() {
        let mut finished = tool_lineage(LlmToolLineageStage::ExecutionFinished, 1);
        finished.transport_ran = Some(true);
        finished.tool_reported_success = Some(true);
        finished.outcome = LlmToolLineageOutcome::Succeeded;
        finished.result_ref = Some("result_1".to_string());
        finished.related_execution_ids = vec![
            "child_execution_1".to_string(),
            "child_execution_2".to_string(),
        ];
        LlmTraceRecord::ToolLineage(finished)
            .validate()
            .expect("delegation edge");

        let mut proposed = tool_lineage(LlmToolLineageStage::Proposed, 0);
        proposed.related_execution_ids = vec!["child_execution_1".to_string()];
        assert!(LlmTraceRecord::ToolLineage(proposed).validate().is_err());
    }

    #[test]
    fn call_start_and_completion_are_two_revisions_of_one_stable_call_fact() {
        let context = context();
        let started = LlmTraceRecord::CallStarted(LlmCallStarted::new(context.clone(), "chat", 10));
        let completed = LlmTraceRecord::CallCompleted(completed_call(context.clone()));

        assert_eq!(started.key().stable_id, context.llm_call_id);
        assert_eq!(completed.key().stable_id, context.llm_call_id);
        assert_eq!(started.key().record_kind, LlmTraceRecordKind::CallFact);
        assert_eq!(started.key().revision, 1);
        assert_eq!(completed.key().revision, 2);
        assert_ne!(
            started.key().idempotency_key(),
            completed.key().idempotency_key()
        );
    }

    #[test]
    fn terminal_usage_harness_aggregate_cannot_claim_a_physical_bill() {
        let mut record = completed_call(context());
        record.dispatch_job_id = None;
        record.provider_response_id = None;
        record.provider_attempt_count = 0;
        record.response_kind = Some(LLM_HARNESS_AGGREGATE_RESPONSE_KIND.into());
        record.pricing = LlmPricingFact {
            pricing_version: Some("harness-reported".into()),
            cost_source: Some(LlmCostSource::Estimated),
            cost_usd: Some(0.125),
            ..Default::default()
        };
        LlmTraceRecord::CallCompleted(record.clone())
            .validate()
            .unwrap();
        record.provider_attempt_count = 1;
        assert!(LlmTraceRecord::CallCompleted(record.clone())
            .validate()
            .is_err());
        record.provider_attempt_count = 0;
        record.pricing.cost_source = Some(LlmCostSource::Provider);
        assert!(LlmTraceRecord::CallCompleted(record).validate().is_err());
    }

    #[test]
    fn call_start_rejects_unknown_prompt_projection_mode() {
        let mut started = LlmCallStarted::new(context(), "chat", 10);
        started.prompt_projection_mode = Some("seventh_call".to_string());
        assert!(LlmTraceRecord::CallStarted(started).validate().is_err());
    }

    #[test]
    fn ordinary_success_requires_attempts_and_logical_summaries_are_non_billing() {
        let mut ordinary = completed_call(context());
        ordinary.provider_attempt_count = 0;
        assert!(LlmTraceRecord::CallCompleted(ordinary.clone())
            .validate()
            .is_err());

        ordinary.response_kind = Some(LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND.to_string());
        ordinary.usage = LlmTokenUsageFact::default();
        ordinary.pricing = LlmPricingFact {
            pricing_version: Some("logical-chunk-summary-non-billing-v1".to_string()),
            cost_source: Some(LlmCostSource::Unknown),
            ..LlmPricingFact::default()
        };
        LlmTraceRecord::CallCompleted(ordinary.clone())
            .validate()
            .expect("non-billing logical summary");

        ordinary.provider_attempt_count = 1;
        let error = LlmTraceRecord::CallCompleted(ordinary)
            .validate()
            .expect_err("summary cannot conceal a physical attempt");
        assert!(error.to_string().contains("logical chunk summary"));

        let mut failed_summary = completed_call(context());
        failed_summary.terminal_state = LlmCallTerminalState::Failed;
        failed_summary.provider_attempt_count = 0;
        failed_summary.response_kind = Some(LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND.to_string());
        failed_summary.error_class = Some("provider".to_string());
        failed_summary.validation.response_present = false;
        failed_summary.validation.parse_attempted = false;
        failed_summary.validation.parse_success = None;
        failed_summary.usage = LlmTokenUsageFact::default();
        failed_summary.pricing = LlmPricingFact::default();
        LlmTraceRecord::CallCompleted(failed_summary)
            .validate()
            .expect("failed non-billing logical summary");
    }

    #[test]
    fn content_free_categories_reject_prose_and_oversized_values() {
        let mut call = completed_call(context());
        call.response_kind = Some("the model replied with private user text".to_string());
        let error = LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("response kind prose must not cross the fact boundary");
        assert!(error.to_string().contains("content-free machine category"));

        let mut call = completed_call(context());
        call.operation = "summarize the user's private email".to_string();
        let error = LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("operation prose must not cross the fact boundary");
        assert!(error.to_string().contains("content-free machine category"));

        let mut started = LlmCallStarted::new(context(), "chat", 1);
        started.capability = Some("private user request".to_string());
        let error = LlmTraceRecord::CallStarted(started)
            .validate()
            .expect_err("capability prose must not cross the fact boundary");
        assert!(error.to_string().contains("content-free machine category"));

        let mut call = completed_call(context());
        call.pricing.pricing_version = Some("rate derived from private contract text".to_string());
        let error = LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("pricing provenance prose must not cross the fact boundary");
        assert!(error
            .to_string()
            .contains("content-free machine identifier"));

        let mut call = completed_call(context());
        call.pricing.pricing_version = Some("runtime-pricing-table@call-time".to_string());
        LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect("bounded historical pricing identifiers remain compatible");

        let mut call = completed_call(context());
        call.pricing.pricing_version = Some("pricing-row-v1:not-a-fingerprint".to_string());
        let error = LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("reserved pricing-row provenance must carry an exact fingerprint");
        assert!(error
            .to_string()
            .contains("content-free machine identifier"));

        let mut validation = LlmImmediateValidationFact {
            response_present: true,
            contract_validation_attempted: true,
            contract_validation_success: Some(false),
            validation_error_class: Some("x".repeat(MAX_MACHINE_CATEGORY_BYTES + 1)),
            discarded_before_use: true,
            discard_reason: Some("contract_rejected".to_string()),
            ..LlmImmediateValidationFact::default()
        };
        let error = validation
            .validate()
            .expect_err("oversized validation class must fail closed");
        assert!(error.to_string().contains("content-free machine category"));

        validation.validation_error_class = Some("json_schema".to_string());
        validation.validate().expect("short machine categories");

        let gap = LlmCaptureGap {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            gap_id: "gap-test".to_string(),
            scope: LlmScope::new("principal", "workspace"),
            llm_call_id: None,
            operation: "chat".to_string(),
            reason: "parser returned private content".to_string(),
            missing_record_count: 1,
            first_observed_at_ms: 1,
            last_observed_at_ms: 1,
            emitted_at_ms: 1,
        };
        let error = LlmTraceRecord::CaptureGap(gap)
            .validate()
            .expect_err("gap reason prose must not cross the fact boundary");
        assert!(error.to_string().contains("content-free machine category"));
    }

    #[test]
    fn route_and_provider_identifiers_reject_boundary_whitespace() {
        let mut started = LlmCallStarted::new(context(), "chat", 1);
        started.selected_profile = Some(" chat-fast".to_string());
        assert!(LlmTraceRecord::CallStarted(started)
            .validate()
            .expect_err("selected profile must be canonical")
            .to_string()
            .contains("canonical nonblank identifier"));

        let mut attempt =
            LlmProviderAttemptEvent::started(context(), "chat", 1, "openai ", "gpt-test", 10);
        assert!(LlmTraceRecord::ProviderAttempt(attempt.clone())
            .validate()
            .expect_err("provider must be canonical")
            .to_string()
            .contains("canonical nonblank identifier"));

        attempt.provider = "openai".to_string();
        attempt.effective_profile = Some("profile ".to_string());
        assert!(LlmTraceRecord::ProviderAttempt(attempt)
            .validate()
            .expect_err("effective profile must be canonical")
            .to_string()
            .contains("canonical nonblank identifier"));
    }

    #[test]
    fn capture_disabled_facts_cannot_claim_training_eligibility() {
        let invalid = LlmCaptureFact {
            mode: LlmCaptureMode::Off,
            status: LlmCaptureStatus::PolicyDenied,
            training_eligible_at_capture: true,
            training_exclusion_reason: None,
        };
        let error = invalid
            .validate()
            .expect_err("disabled capture cannot be eligible for training");
        assert!(error.to_string().contains("cannot be training eligible"));

        LlmCaptureFact {
            training_eligible_at_capture: false,
            training_exclusion_reason: Some("capture_disabled".to_string()),
            ..invalid
        }
        .validate()
        .expect("disabled capture with an exclusion reason is coherent");
    }

    #[test]
    fn timing_requires_a_coherent_first_token_decomposition() {
        let mut missing_ttft = completed_call(context());
        missing_ttft.timing.ttft_ms = None;
        assert!(LlmTraceRecord::CallCompleted(missing_ttft)
            .validate()
            .expect_err("first token without TTFT must fail")
            .to_string()
            .contains("first_token_at_ms and ttft_ms"));

        let mut missing_first_token = completed_call(context());
        missing_first_token.timing.first_token_at_ms = None;
        assert!(LlmTraceRecord::CallCompleted(missing_first_token)
            .validate()
            .expect_err("TTFT without first-token time must fail")
            .to_string()
            .contains("first_token_at_ms and ttft_ms"));

        let mut orphan_generation = completed_call(context());
        orphan_generation.timing.first_token_at_ms = None;
        orphan_generation.timing.ttft_ms = None;
        assert!(LlmTraceRecord::CallCompleted(orphan_generation)
            .validate()
            .expect_err("post-TTFT generation requires TTFT")
            .to_string()
            .contains("generation_after_ttft_ms requires"));
    }

    #[test]
    fn provider_attempt_phases_share_identity_and_advance_revision() {
        let context = context();
        let started =
            LlmProviderAttemptEvent::started(context.clone(), "chat", 2, "openai", "gpt-test", 10);
        let mut first_token = started.clone();
        first_token.phase = LlmProviderAttemptPhase::FirstToken;
        first_token.occurred_at_ms = 15;
        first_token.observed_at_ms = 15;
        first_token.timing.first_token_at_ms = Some(15);
        first_token.timing.ttft_ms = Some(5);
        let mut completed = first_token.clone();
        completed.phase = LlmProviderAttemptPhase::Completed;
        completed.occurred_at_ms = 20;
        completed.observed_at_ms = 21;
        completed.timing.completed_at_ms = Some(20);
        completed.timing.generation_after_ttft_ms = Some(5);
        completed.timing.latency_ms = Some(10);
        completed.terminal_state = Some(LlmAttemptTerminalState::Succeeded);

        for event in [&started, &first_token, &completed] {
            LlmTraceRecord::ProviderAttempt(event.clone())
                .validate()
                .expect("valid attempt revision");
            assert_eq!(event.provider_attempt_id, context.provider_attempt_id(2));
        }
        assert_eq!(LlmTraceRecord::ProviderAttempt(started).key().revision, 1);
        assert_eq!(
            LlmTraceRecord::ProviderAttempt(first_token).key().revision,
            2
        );
        assert_eq!(LlmTraceRecord::ProviderAttempt(completed).key().revision, 3);
    }

    #[test]
    fn mismatched_provider_attempt_identity_is_rejected() {
        let mut attempt =
            LlmProviderAttemptEvent::started(context(), "chat", 1, "openai", "gpt-test", 10);
        attempt.provider_attempt_id = "forged:a1".to_string();

        let error = LlmTraceRecord::ProviderAttempt(attempt)
            .validate()
            .expect_err("forged identity must fail");
        assert!(error.to_string().contains("provider attempt id"));
    }

    #[test]
    fn first_token_and_terminal_attempt_revisions_require_complete_timing() {
        let mut attempt =
            LlmProviderAttemptEvent::started(context(), "chat", 1, "openai", "gpt-test", 10);
        attempt.phase = LlmProviderAttemptPhase::FirstToken;
        assert!(LlmTraceRecord::ProviderAttempt(attempt.clone())
            .validate()
            .is_err());

        attempt.phase = LlmProviderAttemptPhase::Completed;
        attempt.timing.completed_at_ms = Some(20);
        assert!(LlmTraceRecord::ProviderAttempt(attempt).validate().is_err());
    }

    #[test]
    fn terminal_attempt_allows_unknown_timing_without_inventing_a_start() {
        let mut attempt =
            LlmProviderAttemptEvent::started(context(), "chat", 2, "anthropic", "fallback", 10);
        attempt.phase = LlmProviderAttemptPhase::Completed;
        attempt.occurred_at_ms = 30;
        attempt.observed_at_ms = 31;
        attempt.timing = LlmTimingFact {
            completed_at_ms: Some(30),
            ..LlmTimingFact::default()
        };
        attempt.terminal_state = Some(LlmAttemptTerminalState::Succeeded);

        LlmTraceRecord::ProviderAttempt(attempt.clone())
            .validate()
            .expect("terminal identity may be known while fallback timing is not");

        attempt.timing.provider_execution_ms = Some(20);
        let error = LlmTraceRecord::ProviderAttempt(attempt)
            .validate()
            .expect_err("provider duration without an exact start must be rejected");
        assert!(error.to_string().contains("without an exact start"));
    }

    #[test]
    fn terminal_records_require_total_latency_and_call_creation_time() {
        let mut call = completed_call(context());
        call.timing.created_at_ms = None;
        assert!(LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("call creation time is mandatory")
            .to_string()
            .contains("created_at_ms"));

        let mut call = completed_call(context());
        call.timing.latency_ms = None;
        assert!(LlmTraceRecord::CallCompleted(call)
            .validate()
            .expect_err("call latency is mandatory")
            .to_string()
            .contains("total latency"));

        let mut attempt =
            LlmProviderAttemptEvent::started(context(), "chat", 1, "openai", "gpt-test", 10);
        attempt.phase = LlmProviderAttemptPhase::Completed;
        attempt.occurred_at_ms = 20;
        attempt.observed_at_ms = 20;
        attempt.timing.completed_at_ms = Some(20);
        attempt.terminal_state = Some(LlmAttemptTerminalState::Succeeded);
        assert!(LlmTraceRecord::ProviderAttempt(attempt)
            .validate()
            .expect_err("attempt latency is mandatory")
            .to_string()
            .contains("start and latency"));
    }

    #[test]
    fn validation_attempt_and_result_presence_cannot_disagree() {
        let mut completed = completed_call(context());
        completed.validation.parse_attempted = false;
        completed.validation.parse_success = Some(true);

        assert!(LlmTraceRecord::CallCompleted(completed).validate().is_err());
    }

    #[test]
    fn token_totals_and_reasoning_bounds_fail_closed() {
        let mut completed = completed_call(context());
        completed.usage.total_tokens = Some(999);
        assert!(LlmTraceRecord::CallCompleted(completed)
            .validate()
            .expect_err("inconsistent total")
            .to_string()
            .contains("total_tokens"));

        let mut completed = completed_call(context());
        completed.usage.reasoning_tokens = Some(21);
        assert!(LlmTraceRecord::CallCompleted(completed)
            .validate()
            .expect_err("reasoning exceeds output")
            .to_string()
            .contains("reasoning_tokens"));
    }

    #[test]
    fn realtime_token_buckets_require_a_complete_consistent_modality_split() {
        let mut completed = completed_call(context());
        completed.usage = LlmTokenUsageFact {
            input_tokens: Some(500),
            output_tokens: Some(200),
            reasoning_tokens: Some(0),
            cache_read_tokens: Some(100),
            cache_creation_tokens: Some(0),
            audio_input_tokens: Some(300),
            audio_output_tokens: Some(150),
            audio_cached_tokens: Some(80),
            total_tokens: Some(700),
        };
        LlmTraceRecord::CallCompleted(completed.clone())
            .validate()
            .expect("consistent realtime usage");

        completed.usage.audio_output_tokens = None;
        assert!(LlmTraceRecord::CallCompleted(completed.clone())
            .validate()
            .expect_err("partial audio split")
            .to_string()
            .contains("every audio and folded token bucket"));

        completed.usage.audio_output_tokens = Some(150);
        completed.usage.audio_input_tokens = Some(401);
        assert!(LlmTraceRecord::CallCompleted(completed)
            .validate()
            .expect_err("audio and cache exceed folded input")
            .to_string()
            .contains("uncached audio plus cache"));
    }

    #[test]
    fn monetary_values_require_pricing_provenance() {
        let mut completed = completed_call(context());
        completed.pricing.pricing_version = None;
        completed.pricing.cost_source = None;
        let error = LlmTraceRecord::CallCompleted(completed)
            .validate()
            .expect_err("unversioned cost must fail");
        assert!(error.to_string().contains("monetary values require"));
    }

    #[test]
    fn terminal_state_and_error_class_cannot_contradict() {
        let mut succeeded = completed_call(context());
        succeeded.error_class = Some("provider_error".to_string());
        assert!(LlmTraceRecord::CallCompleted(succeeded).validate().is_err());

        let mut failed = completed_call(context());
        failed.terminal_state = LlmCallTerminalState::Failed;
        failed.error_class = None;
        assert!(LlmTraceRecord::CallCompleted(failed).validate().is_err());
    }

    #[test]
    fn journal_envelope_roundtrips_and_detects_payload_tampering() {
        let record = LlmTraceRecord::CallCompleted(completed_call(context()));
        let envelope = LlmTraceJournalEnvelope::new(1, record).expect("envelope");
        envelope.verify().expect("checksum");
        let encoded = serde_json::to_vec(&envelope).expect("serialize");
        let decoded: LlmTraceJournalEnvelope =
            serde_json::from_slice(&encoded).expect("deserialize");
        decoded.verify().expect("roundtrip checksum");

        let mut tampered = decoded;
        if let LlmTraceRecord::CallCompleted(completed) = &mut tampered.record {
            completed.operation = "tampered".to_string();
        }
        assert!(matches!(
            tampered.verify(),
            Err(LlmTraceRecordError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn durable_journal_verifies_exact_raw_record_bytes_across_float_roundtrip() {
        let mut completed = completed_call(context());
        completed.pricing.cost_usd = Some(f64::from_bits(0.01097_f64.to_bits() - 1));
        let envelope = LlmTraceJournalEnvelope::new(1, LlmTraceRecord::CallCompleted(completed))
            .expect("envelope");
        let encoded = serde_json::to_vec(&envelope).expect("serialize");
        LlmTraceJournalEnvelope::from_json_line_verified(&encoded)
            .expect("raw durable checksum survives typed float roundtrip");

        let mut tampered = String::from_utf8(encoded).expect("utf8 json");
        tampered = tampered.replacen("\"operation\":\"chat\"", "\"operation\":\"other\"", 1);
        assert!(matches!(
            LlmTraceJournalEnvelope::from_json_line_verified(tampered.as_bytes()),
            Err(LlmTraceRecordError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn recorder_validates_before_delivering_to_sink() {
        let sink = Arc::new(CollectingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let context = context();
        let receipt = recorder
            .begin_call(LlmCallStarted::new(context.clone(), "chat", 10))
            .expect("accepted call");
        assert!(receipt.accepted);
        assert_eq!(receipt.key.stable_id, context.llm_call_id);

        let mut invalid = completed_call(context);
        invalid.operation.clear();
        assert!(recorder.complete_call(invalid).is_err());
        assert_eq!(sink.records.lock().expect("records").len(), 1);
    }

    #[test]
    fn serialized_fact_size_is_bounded_before_sink_or_replay_acceptance() {
        let mut completed = completed_call(context());
        completed.provider_response_id = Some("r".repeat(MAX_SERIALIZED_LLM_TRACE_RECORD_BYTES));
        let error = LlmTraceRecord::CallCompleted(completed)
            .validate()
            .expect_err("oversized fact must fail closed");
        assert!(error
            .to_string()
            .contains("serialized record exceeds 16384-byte limit"));
    }

    #[test]
    fn capture_gap_requires_exact_positive_count_and_ordered_window() {
        let gap = LlmCaptureGap {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            gap_id: "gap-1".to_string(),
            scope: LlmScope::new("principal", "workspace"),
            llm_call_id: Some("call-1".to_string()),
            operation: "chat".to_string(),
            reason: "critical_buffer_saturated".to_string(),
            missing_record_count: 2,
            first_observed_at_ms: 10,
            last_observed_at_ms: 20,
            emitted_at_ms: 30,
        };
        LlmTraceRecord::CaptureGap(gap.clone())
            .validate()
            .expect("valid gap");

        let mut blank_owner = gap.clone();
        blank_owner.llm_call_id = Some(" ".to_string());
        assert!(LlmTraceRecord::CaptureGap(blank_owner).validate().is_err());

        let mut padded_id = gap.clone();
        padded_id.gap_id = " gap-1".to_string();
        assert!(LlmTraceRecord::CaptureGap(padded_id).validate().is_err());

        let mut invalid = gap;
        invalid.missing_record_count = 0;
        assert!(LlmTraceRecord::CaptureGap(invalid).validate().is_err());
    }

    #[test]
    fn machine_readable_fact_contract_matches_rust_vocabulary() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../data/magician_v2/llm_observability/phase2-fact-contract-v1.json");
        let contract: serde_json::Value = serde_json::from_slice(
            &std::fs::read(path).expect("phase 2 fact contract should exist"),
        )
        .expect("phase 2 fact contract should be valid JSON");

        assert_eq!(
            contract["fact_schema_version"].as_u64(),
            Some(LLM_TRACE_FACT_SCHEMA_VERSION as u64)
        );
        assert_eq!(
            contract["journal_schema_version"].as_u64(),
            Some(LLM_TRACE_JOURNAL_SCHEMA_VERSION as u64)
        );
        assert_eq!(
            contract["maximum_serialized_fact_bytes"].as_u64(),
            Some(MAX_SERIALIZED_LLM_TRACE_RECORD_BYTES as u64)
        );
        assert_eq!(
            strings(&contract["record_kinds"]),
            vec!["call_fact", "provider_attempt", "capture_gap"]
        );
        assert_eq!(
            strings(&contract["capture_statuses"]),
            vec![
                "complete",
                "metadata_only",
                "redacted",
                "sampled_out",
                "policy_denied",
                "oversize",
                "backpressure_degraded",
                "write_failed",
            ]
        );
        assert_eq!(
            contract["record_revisions"]["provider_attempt"]["first_token"].as_u64(),
            Some(LlmProviderAttemptPhase::FirstToken.revision() as u64)
        );
        assert_eq!(
            contract["ordinary_fact_payload_content_allowed"].as_bool(),
            Some(false)
        );
        assert_eq!(
            contract["capture_gap_ownership"].as_str(),
            Some("call_specific_gaps_carry_llm_call_id_process_wide_gaps_leave_it_null")
        );
    }

    fn strings(value: &serde_json::Value) -> Vec<&str> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|value| value.as_str().expect("string"))
            .collect()
    }
}
