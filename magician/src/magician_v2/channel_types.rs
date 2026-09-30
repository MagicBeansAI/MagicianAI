//! Channel vocabulary shared by the lib (observe config, content sources)
//! and the comms crate. Extracted so the unified observe config and the
//! content comms adapters can live lib-side.

use serde::{Deserialize, Serialize};

pub fn default_schema_version() -> u32 {
    1
}

/// Assistance lane a channel account (and therefore its threads and
/// annotations) belongs to (Phase 1b design, "Lanes"): `user_assist` is
/// the owner's own correspondence; `envoy` is the agent's own
/// correspondence (Presto's gmail/WhatsApp), surfaced separately in
/// Phase 3. Rows default to `user_assist` — Phase 1 wrote only that lane.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLane {
    #[default]
    UserAssist,
    Envoy,
}

/// Local-distillation queue state per message (Phase 1b design,
/// "Local-only distillation"). `pending` rows are drained by the distill
/// queue worker; `suppressed` rows short-circuit BEFORE distillation and
/// stay redacted-metadata-only forever; `skipped` means the fail-closed
/// local-provider guard refused to run (op unbound or non-local);
/// `failed` rows retry with a cap tracked in `distill_attempts`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DistillState {
    #[default]
    Pending,
    Done,
    Skipped,
    Suppressed,
    Failed,
    /// Queued work retired because its source is outside the history window.
    /// Source metadata, prior summaries and attempt counters remain intact.
    Expired,
}

/// Message direction relative to the account owner — WhatsApp's
/// needs-reply primitive (last-inbound + age). Nullable on the row:
/// providers derive it only where cheap (gmail: SENT label / sender ==
/// self).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

/// Tiny local-only follow-up signal distilled from a single channel message
/// or a bounded same-thread batch. This is not a separate product entity; it
/// is the evidence-side hint that helps the classifier turn communication
/// into ordinary follow-ups.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelFollowUpHint {
    /// Closed-ish vocabulary normalized by the distiller:
    /// `needs_reply`, `owner_owes`, `other_owes`, `waiting_on`,
    /// `check_back`, `schedule`, or `none`.
    pub kind: String,
    /// Who appears to hold the next action: `owner`, `counterparty`,
    /// `agent`, or `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_text: Option<String>,
    /// `low`, `normal`, or `high` when the message itself supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<String>,
    /// Short, content-safe rationale. No raw quotes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Short action-critical details the owner needs to act: exact due
    /// date/time, amount, merchant/biller, meeting time, or a redacted
    /// identifier such as last4. Full secrets and account numbers are not
    /// allowed here.
    #[serde(
        default,
        deserialize_with = "deserialize_string_vec",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub key_details: Vec<String>,
}

/// The kind of information carried by a communication, independent of its
/// provider and of the attention lane it eventually enters.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelInformationType {
    ChangeNotice,
    Deadline,
    Transaction,
    Request,
    Scheduling,
    Event,
    GeneralInformation,
    Promotion,
    #[default]
    Other,
}

/// Versioned, provider-neutral, content-safe brief persisted instead of a raw
/// message body. The compatibility `summary` output remains authoritative;
/// the parser copies it here so the two representations cannot diverge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelInformationBrief {
    pub schema_version: u32,
    #[serde(default)]
    pub information_type: ChannelInformationType,
    #[serde(default)]
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub key_facts: Vec<String>,
    #[serde(default)]
    pub changes: Vec<ChannelChangeFact>,
    #[serde(default)]
    pub temporal_facts: Vec<ChannelTemporalFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stated_action: Option<String>,
    #[serde(default)]
    pub detail_status: ChannelDetailStatus,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub missing_details: Vec<String>,
}

/// Whether the source supplied enough concrete information to understand the
/// notice. `SourceOmitsDetails` is evidence, not a model failure: it prevents a
/// vague change notice from being presented as complete.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelDetailStatus {
    Complete,
    #[default]
    Partial,
    SourceOmitsDetails,
}

/// Provenance of an ingested row: the metadata sync worker or a fixture
/// seed (the `annotations/seed` endpoint / test fixtures).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailRecordOrigin {
    MetadataSync,
    Seed,
}

/// Append-only per-message metadata row keyed by message_id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailMessageMeta {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub provider: String,
    pub account_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    pub thread_id: String,
    pub message_id: String,
    /// Opaque per-provider incremental-sync cursor stamped on the message
    /// (for gmail it holds the Gmail `historyId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cursor: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    /// Subject; [`REDACTED_SUBJECT_PLACEHOLDER`] when
    /// `sensitive_suppressed` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
    /// To/Cc recipient DOMAINS only (privacy contract).
    #[serde(default)]
    pub to_domains: Vec<String>,
    #[serde(default)]
    pub cc_domains: Vec<String>,
    /// Gmail internalDate, epoch millis.
    pub internal_date: i64,
    pub observed_at: i64,
    /// Inbound/outbound relative to the account owner; None when the
    /// provider can't derive it cheaply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<MessageDirection>,
    /// Locally-derived understanding (N3 distiller output). NEVER raw
    /// content — bodies exist only in process memory during distillation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Distiller hint: true when this message reasonably expects a response.
    #[serde(default)]
    pub needs_reply_hint: bool,
    /// Distiller hint for ordinary follow-up routing. Stored only as a compact
    /// JSON object; never raw content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_hint: Option<ChannelFollowUpHint>,
    /// Information-complete, content-safe local distillation. This never
    /// contains a raw body or provider-controlled URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distill_brief: Option<ChannelInformationBrief>,
    /// Prompt/output contract version that produced `distill_brief`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distill_contract_version: Option<u32>,
    /// Wall-clock completion time for this distillation (epoch millis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distilled_at: Option<i64>,
    /// Scope-local monotonic completion sequence. Unlike `internal_date`, a
    /// later re-distillation of an old message receives a newer revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distill_revision: Option<i64>,
    /// Distillation queue state; appenders set `suppressed` directly for
    /// sensitive rows so they never enter the pending queue.
    #[serde(default)]
    pub distill_state: DistillState,
    /// Failed-distillation retry counter (incremented on `failed`).
    #[serde(default)]
    pub distill_attempts: i64,
    #[serde(default)]
    pub sensitive_suppressed: bool,
    pub origin: MailRecordOrigin,
}

/// One concrete before/after (or newly introduced) change supported by the
/// source. Every string is sanitized and bounded before persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelChangeFact {
    pub aspect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_text: Option<String>,
}

/// One source-supported date/time fact. The model supplies `text`; `at_ms` is a
/// deterministic epoch-millisecond marker when the text carries a supported
/// absolute date, and remains null for relative/ambiguous text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelTemporalFact {
    pub kind: ChannelTemporalKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Closed temporal taxonomy emitted as text by the local model. `at_ms` is
/// populated only by deterministic server-side parsing of supported absolute
/// date text.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelTemporalKind {
    Due,
    Expiry,
    Effective,
    Scheduled,
    Occurred,
    PeriodStart,
    PeriodEnd,
    #[default]
    Other,
}

fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(value)) => vec![value],
        _ => Vec::new(),
    })
}

impl ChannelLane {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::UserAssist => "user_assist",
            Self::Envoy => "envoy",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "user_assist" => Ok(Self::UserAssist),
            "envoy" => Ok(Self::Envoy),
            other => anyhow::bail!("unknown channel lane: {other}"),
        }
    }
}

impl MessageDirection {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "inbound" => Ok(Self::Inbound),
            "outbound" => Ok(Self::Outbound),
            other => anyhow::bail!("unknown message direction: {other}"),
        }
    }
}

impl DistillState {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Suppressed => "suppressed",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "done" => Ok(Self::Done),
            "skipped" => Ok(Self::Skipped),
            "suppressed" => Ok(Self::Suppressed),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            other => anyhow::bail!("unknown distill state: {other}"),
        }
    }
}

impl MailRecordOrigin {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::MetadataSync => "metadata_sync",
            Self::Seed => "seed",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "metadata_sync" => Ok(Self::MetadataSync),
            "seed" => Ok(Self::Seed),
            other => anyhow::bail!("unknown mail record origin: {other}"),
        }
    }
}

impl ChannelRequiredActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::FollowUp => "follow_up",
            Self::Schedule => "schedule",
        }
    }
}

/// Deterministic action intent derived from local, source-supported distill
/// fields. This is deliberately smaller than the product action vocabulary:
/// optional operations such as sharing or saving to memory never affect lane
/// routing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRequiredActionKind {
    Reply,
    FollowUp,
    Schedule,
}
