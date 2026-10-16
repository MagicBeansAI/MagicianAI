//! Local-only distillation queue (Channel Assist Phase 1b, N3).
//!
//! Design: `docs/plans/2026-07-05-channel-assist-phase1b-design.md`
//! ("Local-only distillation"). This module IS the privacy posture of the
//! channel-assist plane: message bodies are fetched into process memory,
//! distilled into compatibility fields plus a bounded provider-neutral safe
//! brief by a LOCAL model, persisted as derived data only, and discarded.
//!
//! # Fail-closed locality guard — where the promise is enforced
//!
//! [`resolve_local_provider`] resolves the profile EXPLICITLY bound to
//! the `channel_ingest_distill` operation in `operation_mapping` and
//! requires its provider kind to be the local family
//! ([`LLMProviderKind::Ollama`] — the only local-inference kind magicllm
//! has). On success it returns the [`VerifiedLocalBinding`] (profile
//! name + kind) that every subsequent dispatch is PINNED to. On failure,
//! "distillation unavailable" with two distinct severities:
//!
//! - operation unbound, or no router at all (the shipped config template
//!   leaves the binding commented out) → distillation is IDLE: nothing
//!   is drained, rows stay `pending`, the backlog is preserved for when
//!   a local binding appears, and one edge-triggered info is logged. The
//!   router's `default_profile` fallback is deliberately not consulted —
//!   unbound means OFF, never "whatever the default is".
//! - bound to a provider the current `privacy.processing.mode` does not
//!   permit (local mode + non-ollama arm) → fail-closed IDLE with one loud
//!   warn per streak: the queue is NOT drained, rows stay `pending`, and
//!   content is never fetched or sent. Under a locality policy this state
//!   means config was hand-edited against the selected mode; a durable
//!   queue is never destroyed to signal a config mistake.
//!
//! **No remote dispatch path exists BY CONSTRUCTION** — three layers,
//! each sufficient on its own:
//!
//! 1. Guard (this module): the one and only production [`DistillLlm`]
//!    implementation ([`RouterDistillLlm`]) re-runs
//!    [`resolve_local_provider`] inside `complete()` immediately before
//!    every dispatch; a non-Ok verdict aborts before any request exists.
//! 2. Profile pin (magicllm `locked_profile`): the dispatch carries the
//!    guard-verified profile name as `router_profile_override`, so the
//!    transport router cannot re-resolve the operation to a different or
//!    default profile, and any `fallback_profile` traversal away from
//!    the pinned profile is refused ("requested profile disallows
//!    fallback").
//! 3. Provider lock (magicllm `router_required_provider_kind`): the
//!    transport router refuses — against its own immutable config
//!    snapshot at the moment of dispatch, on the initial profile AND on
//!    every fallback hop — any profile that is not Ollama-kind. This
//!    closes the hot-reload race where the pinned profile NAME is
//!    redefined onto a remote provider between the guard's config read
//!    and the dispatch's.
//!
//! No other code path in this module (or anywhere else) sends distill
//! content to an LLM.
//!
//! # Two-stage ingest, stage 2
//!
//! Sync workers land metadata rows fast with `distill_state = pending`
//! (stage 1). This worker drains the pending queue at the local model's
//! own pace: fetch content by provider (the [`ContentFetcher`] registry is
//! populated from provider adapters), distill, write the result, drop the
//! text. Suppressed rows are short-circuited at the
//! appender and never enter the queue; the drain re-checks defensively
//! anyway. Failures degrade to metadata-only rows: `failed` with a
//! capped retry counter, terminal `skipped` at [`MAX_DISTILL_ATTEMPTS`].
//!
//! # Chunking (v1 judgment call)
//!
//! `content::prepare_for_distill` produces bounded paragraph chunks; v1
//! distills the FIRST chunk only and tells the model when material was
//! truncated (the `{truncation_note}` prompt variable). A multi-chunk
//! map/combine pass would double or triple local-model latency per long
//! message for marginal gains on a bounded brief — the first ~10k chars of
//! NEW content (quoted history already stripped) carry the intent and concrete
//! facts of almost every real message. Revisit if eval says otherwise.

use magician::magician_v2::llm_dispatch_seam::{
    resolve_local_provider_for_operation, DistillLlm, DistillUnavailable, RouterDistillLlm,
    VerifiedLocalBinding,
};
// Test-only, and re-added for the same reason as in `attention_lane_bridge`:
// `72fae8b75` removed what only the tests use, having checked the production
// build and not the test target.
#[cfg(any(test, feature = "test-fixtures"))]
use magician::magician_v2::llm_dispatch_seam::{require_local_provider, PinnedLocalDispatch};
#[cfg(any(test, feature = "test-fixtures"))]
use magicllm::LLMProviderKind;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use magician::magician_v2::analytics::runtime_activity_layer::{
    KIND_BACKGROUND, WORKLOAD_COMMS_ASSIST,
};

use crate::channel_assist::resurfacing::source_refs::comm_source_ref_for_message;
use magician::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::artifact_v2::{CapabilityScopePaths, CapabilityWorkspaceManager};
use magician::magician_v2::attention::resurfacing::scoring::supported_temporal_markers_ms;
use magician::magician_v2::attention::resurfacing::source_refs::parse_comm_source_message_key;
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::attention::resurfacing::types::{CandidateState, SourceKind};
use magician::magician_v2::observe_catchup::ObserveCatchUpController;
use magician::magician_v2::observe_catchup::{
    CatchUpAdmission, CatchUpDecision, CatchUpReplayMode,
};
use magician::magician_v2::prompts::{
    managed_prompt, names as prompt_names, rendered_prompt, versions as prompt_versions, Prompt,
};
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::content::{
    build_header_document, distill_chunk_chars, distill_max_chunks, prepare_for_distill,
    DistillContent,
};
use super::store::{DistillBackfillRuntimeState, MailAssistStore, RecentDistillEntry};
use super::types::{
    sanitize_channel_brief_text, ChannelChangeFact, ChannelDetailStatus, ChannelFollowUpHint,
    ChannelInformationBrief, ChannelInformationType, ChannelTemporalFact, ChannelTemporalKind,
    DistillState, MailMessageMeta, CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
};

/// Wall-clock epoch milliseconds (best-effort; 0 before the epoch). Used only
/// to timestamp live-feed entries, so precision/monotonicity don't matter.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Operation name the distiller resolves and dispatches through. Bound in
/// CONFIG (per the house config-swap rule) — see the commented
/// `channel_ingest_distill` entry in `llm-router.yaml`'s `operation_mapping`.
pub const CHANNEL_INGEST_DISTILL_OPERATION: &str = "channel_ingest_distill";

/// Total distillation attempts per message before the row is terminally
/// `skipped` (metadata-only forever).
pub const MAX_DISTILL_ATTEMPTS: i64 = 3;

const LOG_TARGET: &str = "channel_assist::distill";

const DEFAULT_INTERVAL_SECS: u64 = 60;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 120;
const DEFAULT_BATCH: usize = 8;
const DEFAULT_CONCURRENCY: usize = 1;

/// Defensive ceilings on the safe brief. A runaway local model must not bloat
/// the store, API payload, or downstream curator prompt.
const MAX_SUMMARY_CHARS: usize = 900;
const MAX_BRIEF_TEXT_CHARS: usize = 160;
const MAX_BRIEF_KEY_FACTS: usize = 8;
const MAX_BRIEF_CHANGES: usize = 6;
const MAX_BRIEF_TEMPORAL_FACTS: usize = 6;
const MAX_BRIEF_MISSING_DETAILS: usize = 6;

/// Closed intent taxonomy persisted in the `intent` column. Anything the
/// model returns outside this set normalizes to `other` — the column
/// stays queryable by the Phase-2 classifier.
pub const INTENT_TAXONOMY: [&str; 8] = [
    "needs_reply",
    "fyi",
    "action_request",
    "scheduling",
    "transactional",
    "social",
    "newsletter",
    "other",
];
const FOLLOW_UP_HINT_KINDS: [&str; 7] = [
    "none",
    "needs_reply",
    "owner_owes",
    "other_owes",
    "waiting_on",
    "check_back",
    "schedule",
];
const FOLLOW_UP_ACTORS: [&str; 4] = ["owner", "counterparty", "agent", "unknown"];
const FOLLOW_UP_URGENCIES: [&str; 3] = ["low", "normal", "high"];
const MAX_HINT_DETAILS: usize = 6;
const MAX_HINT_DETAIL_CHARS: usize = 120;

// ---------------------------------------------------------------------------
// Fail-closed locality guard
// ---------------------------------------------------------------------------

/// Resolve the fail-closed locality guard against the live router config,
/// returning the verified binding every dispatch must pin to. This is the
/// availability gate in front of the LLM call; the pin + provider lock it
/// feeds are enforced again inside magicllm at dispatch time — see the
/// module header ("three layers") and [`RouterDistillLlm`].
pub fn resolve_local_provider(
    router: Option<&OperationLlmRouter>,
) -> Result<VerifiedLocalBinding, DistillUnavailable> {
    resolve_local_provider_for_operation(router, CHANNEL_INGEST_DISTILL_OPERATION)
}

// ---------------------------------------------------------------------------
// LLM seam
// ---------------------------------------------------------------------------

/// Placeholder used when no router exists: the guard already failed with
/// [`DistillUnavailable::RouterUnavailable`], so this is never invoked —
/// it errors loudly if that invariant ever breaks.
struct UnavailableLlm;

#[async_trait]
impl DistillLlm for UnavailableLlm {
    async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
        Err(anyhow!(
            "distill LLM invoked with no operation router — guard invariant violated"
        ))
    }
}

// ---------------------------------------------------------------------------
// Strict-JSON output parsing
// ---------------------------------------------------------------------------

/// The distiller's structured output. `Deserialize` only — nothing here
/// is serialized back out; the compact derived fields are persisted through
/// [`MailAssistStore::set_distill_result`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DistillOutput {
    pub summary: String,
    pub intent: String,
    #[serde(default)]
    pub needs_reply_hint: bool,
    #[serde(default)]
    pub follow_up_hint: Option<ChannelFollowUpHint>,
    /// V2 provider-neutral safe brief. Optional at the wire boundary so the
    /// worker can still run the v1 contract during staged rollout; contract-v2
    /// parsing requires it deterministically.
    #[serde(default)]
    pub brief: Option<ChannelInformationBrief>,
}

/// Provider-native schema paired with the managed distillation prompt. The
/// schema is deliberately generated from the same contract-version switch as
/// the prompt so staged v1 callers do not accidentally receive the v2 shape.
/// Semantic/evidence checks remain server-side in
/// [`validate_information_brief`]; this schema prevents malformed JSON and
/// wrong field types before Ollama emits them.
fn distill_response_schema(contract_version: u32) -> Value {
    let nullable_string = || {
        json!({
            "anyOf": [
                {"type": "string"},
                {"type": "null"}
            ]
        })
    };
    let follow_up_hint = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": {"type": "string", "enum": FOLLOW_UP_HINT_KINDS},
            "actor": {
                "anyOf": [
                    {"type": "string", "enum": FOLLOW_UP_ACTORS},
                    {"type": "null"}
                ]
            },
            "counterparty": nullable_string(),
            "due_text": nullable_string(),
            "urgency": {
                "anyOf": [
                    {"type": "string", "enum": FOLLOW_UP_URGENCIES},
                    {"type": "null"}
                ]
            },
            "rationale": nullable_string(),
            "key_details": {
                "type": "array",
                "items": {"type": "string"},
                "maxItems": MAX_HINT_DETAILS
            }
        },
        "required": [
            "kind",
            "actor",
            "counterparty",
            "due_text",
            "urgency",
            "rationale",
            "key_details"
        ]
    });

    let mut properties = serde_json::Map::from_iter([
        ("summary".to_string(), json!({"type": "string"})),
        (
            "intent".to_string(),
            json!({"type": "string", "enum": INTENT_TAXONOMY}),
        ),
        ("needs_reply_hint".to_string(), json!({"type": "boolean"})),
        ("follow_up_hint".to_string(), follow_up_hint),
    ]);
    let mut required = vec!["summary", "intent", "needs_reply_hint", "follow_up_hint"];

    if contract_version >= CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
        properties.insert(
            "brief".to_string(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "schema_version": {
                        "type": "integer",
                        "enum": [CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION]
                    },
                    "information_type": {
                        "type": "string",
                        "enum": [
                            "change_notice",
                            "deadline",
                            "transaction",
                            "request",
                            "scheduling",
                            "event",
                            "general_information",
                            "promotion",
                            "other"
                        ]
                    },
                    "key_facts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": MAX_BRIEF_KEY_FACTS
                    },
                    "changes": {
                        "type": "array",
                        "maxItems": MAX_BRIEF_CHANGES,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "aspect": {"type": "string"},
                                "before": nullable_string(),
                                "after": nullable_string(),
                                "effective_text": nullable_string()
                            },
                            "required": ["aspect", "before", "after", "effective_text"]
                        }
                    },
                    "temporal_facts": {
                        "type": "array",
                        "maxItems": MAX_BRIEF_TEMPORAL_FACTS,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {
                                    "type": "string",
                                    "enum": [
                                        "due",
                                        "expiry",
                                        "effective",
                                        "scheduled",
                                        "occurred",
                                        "period_start",
                                        "period_end",
                                        "other"
                                    ]
                                },
                                "text": {"type": "string"}
                            },
                            "required": ["kind", "text"]
                        }
                    },
                    "stated_action": nullable_string(),
                    "detail_status": {
                        "type": "string",
                        "enum": ["complete", "partial", "source_omits_details"]
                    },
                    "missing_details": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": MAX_BRIEF_MISSING_DETAILS
                    }
                },
                "required": [
                    "schema_version",
                    "information_type",
                    "key_facts",
                    "changes",
                    "temporal_facts",
                    "stated_action",
                    "detail_status",
                    "missing_details"
                ]
            }),
        );
        required.push("brief");
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

/// Parse the model's reply as the strict JSON contract. Tolerates prose/
/// code-fence wrapping by slicing the outermost `{…}` (small local models
/// fence despite instructions), then requires exact field shapes via
/// serde. A non-taxonomy intent normalizes to `other` rather than failing
/// the whole distillation; an empty summary fails (there is nothing to
/// persist).
#[cfg(any(test, feature = "test-fixtures"))]
pub fn parse_distill_output(raw: &str) -> Result<DistillOutput> {
    parse_distill_output_for_contract(raw, 1)
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn parse_distill_output_for_contract(
    raw: &str,
    contract_version: u32,
) -> Result<DistillOutput> {
    parse_distill_output_with_policy(raw, contract_version, MAX_SUMMARY_CHARS)
}

fn parse_distill_output_with_policy(
    raw: &str,
    contract_version: u32,
    summary_max_chars: usize,
) -> Result<DistillOutput> {
    let start = raw.find('{');
    let end = raw.rfind('}');
    let (Some(start), Some(end)) = (start, end) else {
        anyhow::bail!("no JSON object in distill reply");
    };
    if end < start {
        anyhow::bail!("no JSON object in distill reply");
    }
    let mut output: DistillOutput = serde_json::from_str(&raw[start..=end])
        .map_err(|error| anyhow!("distill JSON contract mismatch: {error}"))?;
    output.summary = sanitize_channel_brief_text(&output.summary, summary_max_chars.max(1))
        .context("distill reply carried an empty summary")?;
    output.intent = normalize_intent(&output.intent);
    output.follow_up_hint = normalize_follow_up_hint(output.follow_up_hint);
    output.brief = output
        .brief
        .map(|brief| normalize_information_brief(brief, &output.summary))
        .transpose()?;
    reconcile_information_brief_compatibility(&mut output, summary_max_chars.max(1))?;
    validate_information_brief(&output, contract_version)?;
    Ok(output)
}

/// Reconcile redundant compatibility fields from the richer v2 brief without
/// inventing source facts. The model sometimes places the supported value or
/// action in the structured brief but omits it from the legacy summary/hints;
/// re-asking the model cannot improve evidence that is already present.
fn reconcile_information_brief_compatibility(
    output: &mut DistillOutput,
    summary_max_chars: usize,
) -> Result<()> {
    let Some(brief) = output.brief.as_ref() else {
        return Ok(());
    };
    let mut summary = output.summary.clone();
    if brief.detail_status == ChannelDetailStatus::SourceOmitsDetails
        && !brief.missing_details.is_empty()
        && !summary_discloses_gap(&summary)
    {
        summary = prepend_compatibility_fact(
            &summary,
            &format!("Source omits: {}.", brief.missing_details.join("; ")),
        );
    }
    match brief.information_type {
        ChannelInformationType::ChangeNotice => {
            if let Some(change) = brief.changes.iter().find(|change| {
                [change.after.as_deref(), change.before.as_deref()]
                    .into_iter()
                    .flatten()
                    .all(|value| !summary_mentions(&summary, value))
            }) {
                let value = change.after.as_deref().or(change.before.as_deref());
                if let Some(value) = value {
                    summary = prepend_compatibility_fact(
                        &summary,
                        &format!("Change to {}: {}.", change.aspect, value),
                    );
                }
            }
        },
        ChannelInformationType::Deadline => {
            if let Some(fact) = brief.temporal_facts.iter().find(|fact| {
                matches!(
                    fact.kind,
                    ChannelTemporalKind::Due | ChannelTemporalKind::Expiry
                ) && !summary_mentions(&summary, &fact.text)
            }) {
                summary =
                    prepend_compatibility_fact(&summary, &format!("Deadline: {}.", fact.text));
            }
        },
        ChannelInformationType::Request if brief.stated_action.is_some() => {
            output.needs_reply_hint = true;
            output.intent = "action_request".to_string();
        },
        _ => {},
    }
    output.summary = sanitize_channel_brief_text(summary, summary_max_chars)
        .context("reconciled distill summary was empty")?;
    if let Some(brief) = output.brief.as_mut() {
        brief.summary.clone_from(&output.summary);
    }
    Ok(())
}

/// Put a structured compatibility fact first so the summary length clamp
/// cannot discard the exact value we are repairing. Appending before a hard
/// clamp made long but otherwise valid summaries lose the repair again.
fn prepend_compatibility_fact(summary: &str, fact: &str) -> String {
    let fact = fact.trim();
    let summary = summary.trim();
    if summary.is_empty() {
        fact.to_string()
    } else if fact.is_empty() {
        summary.to_string()
    } else {
        format!("{fact} {summary}")
    }
}

fn normalize_information_brief(
    mut brief: ChannelInformationBrief,
    summary: &str,
) -> Result<ChannelInformationBrief> {
    brief.summary = summary.to_string();
    brief.key_facts = clean_brief_list(brief.key_facts, MAX_BRIEF_KEY_FACTS);
    brief.missing_details = clean_brief_list(brief.missing_details, MAX_BRIEF_MISSING_DETAILS);
    brief.stated_action = brief
        .stated_action
        .and_then(|value| sanitize_channel_brief_text(value, MAX_BRIEF_TEXT_CHARS));

    let mut changes = Vec::new();
    for change in brief.changes {
        let Some(aspect) = sanitize_channel_brief_text(change.aspect, MAX_BRIEF_TEXT_CHARS) else {
            continue;
        };
        let normalized = ChannelChangeFact {
            aspect,
            before: clean_optional_brief_text(change.before),
            after: clean_optional_brief_text(change.after),
            effective_text: clean_optional_brief_text(change.effective_text),
        };
        // An aspect by itself only repeats that "something changed". Keep a
        // row only when it says before/after/new value; effective dates remain
        // available through temporal_facts.
        if normalized.before.is_none() && normalized.after.is_none() {
            continue;
        }
        if !changes.iter().any(|existing: &ChannelChangeFact| {
            existing.aspect.eq_ignore_ascii_case(&normalized.aspect)
                && existing.before == normalized.before
                && existing.after == normalized.after
        }) {
            changes.push(normalized);
        }
        if changes.len() >= MAX_BRIEF_CHANGES {
            break;
        }
    }
    brief.changes = changes;

    let mut temporal_facts = Vec::new();
    for temporal in brief.temporal_facts {
        let Some(text) = sanitize_channel_brief_text(temporal.text, MAX_BRIEF_TEXT_CHARS) else {
            continue;
        };
        let normalized = ChannelTemporalFact {
            kind: temporal.kind,
            // Model-supplied epochs are never trusted. Absolute dates in the
            // source-supported text become deterministic UTC date markers;
            // relative or ambiguous text remains unmaterialized.
            at_ms: supported_temporal_markers_ms(&text).into_iter().next(),
            text,
            timezone: temporal
                .timezone
                .and_then(|value| sanitize_channel_brief_text(value, 64)),
        };
        if !temporal_facts.iter().any(|existing: &ChannelTemporalFact| {
            existing.kind == normalized.kind && existing.text.eq_ignore_ascii_case(&normalized.text)
        }) {
            temporal_facts.push(normalized);
        }
        if temporal_facts.len() >= MAX_BRIEF_TEMPORAL_FACTS {
            break;
        }
    }
    brief.temporal_facts = temporal_facts;
    Ok(brief)
}

fn clean_optional_brief_text(raw: Option<String>) -> Option<String> {
    raw.and_then(|value| sanitize_channel_brief_text(value, MAX_BRIEF_TEXT_CHARS))
}

fn clean_brief_list(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut cleaned = Vec::new();
    for value in values {
        let Some(value) = sanitize_channel_brief_text(value, MAX_BRIEF_TEXT_CHARS) else {
            continue;
        };
        if cleaned
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            continue;
        }
        cleaned.push(value);
        if cleaned.len() >= limit {
            break;
        }
    }
    cleaned
}

fn validate_information_brief(output: &DistillOutput, contract_version: u32) -> Result<()> {
    let Some(brief) = output.brief.as_ref() else {
        if contract_version >= CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
            anyhow::bail!("information brief is required by contract v2");
        }
        return Ok(());
    };
    if brief.schema_version != CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
        anyhow::bail!("information brief has an unsupported schema version");
    }
    if brief.detail_status == ChannelDetailStatus::Complete && !brief.missing_details.is_empty() {
        anyhow::bail!("complete information brief cannot carry missing details");
    }
    if brief.detail_status == ChannelDetailStatus::SourceOmitsDetails
        && brief.missing_details.is_empty()
    {
        anyhow::bail!("source-omits-details brief must name the missing information");
    }

    match brief.information_type {
        ChannelInformationType::ChangeNotice => {
            let explicit_gap = brief.detail_status == ChannelDetailStatus::SourceOmitsDetails
                && !brief.missing_details.is_empty();
            if brief.changes.is_empty() && !explicit_gap {
                anyhow::bail!(
                    "change notice requires a concrete before/after change or an explicit source gap"
                );
            }
            if !brief.changes.is_empty()
                && !brief.changes.iter().any(|change| {
                    [change.before.as_deref(), change.after.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(|value| summary_mentions(&brief.summary, value))
                })
            {
                anyhow::bail!(
                    "change-notice summary must state at least one concrete change value"
                );
            }
            if explicit_gap && !summary_discloses_gap(&brief.summary) {
                anyhow::bail!("source-gap summary must say that the source omits the details");
            }
        },
        ChannelInformationType::Deadline => {
            let has_deadline = brief.temporal_facts.iter().any(|fact| {
                matches!(
                    fact.kind,
                    ChannelTemporalKind::Due | ChannelTemporalKind::Expiry
                )
            });
            let explicit_gap = brief.detail_status != ChannelDetailStatus::Complete
                && !brief.missing_details.is_empty();
            if !has_deadline && !explicit_gap {
                anyhow::bail!("deadline brief requires a due/expiry fact or an explicit gap");
            }
            if has_deadline
                && !brief.temporal_facts.iter().any(|fact| {
                    matches!(
                        fact.kind,
                        ChannelTemporalKind::Due | ChannelTemporalKind::Expiry
                    ) && summary_mentions(&brief.summary, &fact.text)
                })
            {
                anyhow::bail!("deadline summary must state the supported due/expiry text");
            }
            if explicit_gap && !summary_discloses_gap(&brief.summary) {
                anyhow::bail!("deadline-gap summary must say that the deadline is missing");
            }
        },
        ChannelInformationType::Request => {
            let legacy_action = output.needs_reply_hint
                || output.intent == "action_request"
                || output.follow_up_hint.is_some();
            if !legacy_action || brief.stated_action.is_none() {
                anyhow::bail!(
                    "request brief must agree with actionable compatibility fields and state the action"
                );
            }
        },
        _ => {},
    }

    let has_required_action = output.needs_reply_hint
        || output.follow_up_hint.is_some()
        || matches!(output.intent.as_str(), "needs_reply" | "action_request");
    if has_required_action
        && matches!(
            brief.information_type,
            ChannelInformationType::GeneralInformation
                | ChannelInformationType::Promotion
                | ChannelInformationType::Other
        )
        && brief.stated_action.is_none()
    {
        anyhow::bail!("required-action hint disagrees with the information brief");
    }
    Ok(())
}

fn summary_mentions(summary: &str, evidence: &str) -> bool {
    let summary_tokens = semantic_comparison_tokens(summary);
    let evidence_tokens = semantic_comparison_tokens(evidence);
    !evidence_tokens.is_empty()
        && evidence_tokens
            .iter()
            .all(|token| summary_tokens.iter().any(|candidate| candidate == token))
}

fn semantic_comparison_tokens(value: &str) -> Vec<String> {
    value
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn summary_discloses_gap(summary: &str) -> bool {
    let summary = summary.to_ascii_lowercase();
    [
        "does not state",
        "not stated",
        "not provided",
        "not included",
        "source omits",
        "message omits",
        "only in the",
        "unavailable",
        "is missing",
    ]
    .iter()
    .any(|phrase| summary.contains(phrase))
}

/// Closed-taxonomy normalization for the persisted `intent` column.
pub fn normalize_intent(raw: &str) -> String {
    let candidate = raw.trim().to_ascii_lowercase();
    if INTENT_TAXONOMY.contains(&candidate.as_str()) {
        candidate
    } else {
        "other".to_string()
    }
}

fn normalize_token(raw: Option<String>, allowed: &[&str], default: &str) -> String {
    let candidate = raw
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    if allowed.contains(&candidate.as_str()) {
        candidate
    } else {
        default.to_string()
    }
}

fn clean_hint_text(raw: Option<String>, max_chars: usize) -> Option<String> {
    raw.and_then(|value| sanitize_channel_brief_text(value, max_chars))
}

fn clean_hint_details(raw: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for detail in raw {
        let Some(cleaned) = sanitize_channel_brief_text(&detail, MAX_HINT_DETAIL_CHARS) else {
            continue;
        };
        if out
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&cleaned))
        {
            continue;
        }
        out.push(cleaned);
        if out.len() >= MAX_HINT_DETAILS {
            break;
        }
    }
    out
}

fn normalize_follow_up_hint(hint: Option<ChannelFollowUpHint>) -> Option<ChannelFollowUpHint> {
    let hint = hint?;
    let kind = normalize_token(Some(hint.kind), &FOLLOW_UP_HINT_KINDS, "none");
    if kind == "none" {
        return None;
    }
    Some(ChannelFollowUpHint {
        kind,
        actor: Some(normalize_token(hint.actor, &FOLLOW_UP_ACTORS, "unknown")),
        counterparty: clean_hint_text(hint.counterparty, 80),
        due_text: clean_hint_text(hint.due_text, 80),
        urgency: hint
            .urgency
            .map(|u| normalize_token(Some(u), &FOLLOW_UP_URGENCIES, "normal")),
        rationale: clean_hint_text(hint.rationale, 180),
        key_details: clean_hint_details(hint.key_details),
    })
}

/// One distillation with a single fix-it retry: parse failure on the
/// first reply re-asks once with the managed repair prompt appended; a second
/// parse failure (or any transport error) bubbles as the attempt's
/// failure. Exactly the "retry once with a fix-it suffix, then failed"
/// contract.
#[cfg(any(test, feature = "test-fixtures"))]
pub async fn distill_with_retry(
    llm: &dyn DistillLlm,
    system: &str,
    user: &str,
) -> Result<DistillOutput> {
    let repair = managed_prompt(
        prompt_names::CHANNEL_INGEST_DISTILL_REPAIR_USER,
        prompt_versions::CHANNEL_INGEST_DISTILL_REPAIR_USER,
    )
    .await?;
    distill_with_retry_with_policy(llm, system, user, 1, MAX_SUMMARY_CHARS, &repair).await
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn distill_with_retry_for_contract(
    llm: &dyn DistillLlm,
    system: &str,
    user: &str,
    contract_version: u32,
) -> Result<DistillOutput> {
    let repair = managed_prompt(
        prompt_names::CHANNEL_INGEST_DISTILL_REPAIR_USER,
        prompt_versions::CHANNEL_INGEST_DISTILL_REPAIR_USER,
    )
    .await?;
    distill_with_retry_with_policy(
        llm,
        system,
        user,
        contract_version,
        MAX_SUMMARY_CHARS,
        &repair,
    )
    .await
}

async fn distill_with_retry_with_policy(
    llm: &dyn DistillLlm,
    system: &str,
    user: &str,
    contract_version: u32,
    summary_max_chars: usize,
    repair_prompt: &Prompt,
) -> Result<DistillOutput> {
    let response_schema = distill_response_schema(contract_version);
    let first = llm
        .complete_with_response_schema(system, user, &response_schema)
        .await?;
    match parse_distill_output_with_policy(&first, contract_version, summary_max_chars) {
        Ok(output) => Ok(output),
        Err(first_error) => {
            debug!(
                target: LOG_TARGET,
                error = %first_error,
                "distill reply failed contract validation; retrying once with fix-it suffix"
            );
            // Validation errors contain field/contract names only, never model
            // output or source content, so the targeted hint is safe to send
            // back to the same already-local model.
            let mut repair_vars = HashMap::new();
            repair_vars.insert(
                "validation_error".to_string(),
                first_error
                    .to_string()
                    .chars()
                    .take(240)
                    .collect::<String>(),
            );
            let repair = repair_prompt
                .render(&repair_vars)
                .context("rendering managed channel distillation repair prompt")?;
            let retry_user = format!("{user}\n\n{repair}");
            let second = llm
                .complete_with_response_schema(system, &retry_user, &response_schema)
                .await?;
            parse_distill_output_with_policy(&second, contract_version, summary_max_chars).map_err(
                |second_error| {
                    anyhow!(
                        "distill output invalid after retry (first: {first_error}; retry: \
                     {second_error})"
                    )
                },
            )
        },
    }
}

// ---------------------------------------------------------------------------
// ContentFetcher registry (the N4/N5 plug-in contract)
// ---------------------------------------------------------------------------

/// Everything a provider content fetch may need for one scope. Built once
/// per tick (mirrors `IngestContext`; the ingest-only knobs are absent).
#[derive(Debug, Clone)]
pub struct DistillContext {
    pub principal: String,
    pub workspace: String,
    /// Runtime workspace layout, used by local-store-backed content fetchers.
    pub workspace_layout: ArtifactV2Workspace,
    /// Capability auth root — CLI-profile-backed providers (gws) resolve
    /// `<auth_root>/<profile>` under it.
    pub auth_root: PathBuf,
    /// Scope paths for subprocess PATH augmentation (CLI providers).
    pub scope_paths: CapabilityScopePaths,
    /// Per-chunk char budget ([`distill_chunk_chars`], read once per tick).
    pub chunk_chars: usize,
    /// Chunk-count cap ([`distill_max_chunks`], read once per tick).
    pub max_chunks: usize,
}

/// Per-provider content fetch for the distill queue. Implementations
/// return the message's prepared NEW-content chunks (in-memory only —
/// [`DistillContent`] is deliberately not `Serialize`); the queue
/// discards them after distilling. Providers register fetchers through the
/// adapter registry; local sources re-read their own stores by id, since the
/// queue always re-fetches (see the `ingest` module header's content-handoff
/// contract).
#[async_trait]
pub trait ContentFetcher: Send + Sync {
    /// Provider key, matched against [`MailMessageMeta::provider`].
    fn provider(&self) -> &'static str;

    /// Fetch and prepare one pending message's content. An empty
    /// `chunks` result means "no distillable text" (the row is skipped,
    /// metadata-only); an error counts as a capped distill attempt.
    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent>;
}

/// The production registry — one fetcher per provider that can distill local
/// content. Backed by the channel-adapter registry so sync and distill discover
/// shipped providers from the same source.
pub fn default_content_fetchers() -> Vec<Box<dyn ContentFetcher>> {
    super::adapter_registry::default_channel_content_fetchers()
}

/// Channel word handed to the prompt's `{channel}` variable — the ONLY
/// channel-shaped token code contributes; all channel language beyond
/// this single word lives in the prompt store.
pub fn channel_label(provider: &str) -> &'static str {
    super::channel_providers::channel_label(provider)
}

// ---------------------------------------------------------------------------
// Per-row planning (pure, tested) + prompt assembly
// ---------------------------------------------------------------------------

/// What the drain should do with one queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// Suppressed rows are short-circuited at the appender and never
    /// enter the pending queue; if one surfaces anyway (an appender bug,
    /// a manually-edited row), re-assert `suppressed` WITHOUT fetching
    /// content — belt and braces on the redaction promise.
    DefensiveSuppress,
    /// No content fetcher registered for the row's provider. Unreachable
    /// by construction today (rows only exist once a provider has an
    /// ingestor, and N4/N5 land ingestor + fetcher together), but if it
    /// happens the row stays PENDING — the durable obligation outlives
    /// the gap instead of being burned as skipped/failed.
    AwaitContentPath,
    Distill,
}

pub fn plan_row_action(message: &MailMessageMeta, has_fetcher: bool) -> RowAction {
    if message.sensitive_suppressed || message.distill_state == DistillState::Suppressed {
        return RowAction::DefensiveSuppress;
    }
    if !has_fetcher {
        return RowAction::AwaitContentPath;
    }
    RowAction::Distill
}

/// Whether one more failure exhausts the attempt budget.
pub fn attempts_exhausted_after(previous_attempts: i64) -> bool {
    previous_attempts + 1 >= MAX_DISTILL_ATTEMPTS
}

struct DistillPromptSet {
    system: String,
    user: String,
    repair: Prompt,
}

/// Render the managed system and user prompts. The managed repair prompt is
/// rendered only after a contract-validation failure.
async fn render_distill_prompts_for_contract(
    message: &MailMessageMeta,
    content: &DistillContent,
    contract_version: u32,
) -> Result<DistillPromptSet> {
    let channel = channel_label(&message.provider);
    let header = build_header_document(message, content.attachment_count);
    let chunk = content.chunks.first().map(String::as_str).unwrap_or("");
    // First-chunk-only (module header, "Chunking"): any dropped material
    // — later chunks or the chunker's own cap — is disclosed through the
    // managed prompt's explicit truncation field so the summary can hedge.
    let truncated = content.truncated || content.chunks.len() > 1;
    let (system_version, user_version) =
        if contract_version >= CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
            (
                prompt_versions::CHANNEL_INGEST_DISTILL_SYSTEM,
                prompt_versions::CHANNEL_INGEST_DISTILL_USER,
            )
        } else {
            (
                prompt_versions::CHANNEL_INGEST_DISTILL_SYSTEM_LEGACY,
                prompt_versions::CHANNEL_INGEST_DISTILL_USER_LEGACY,
            )
        };
    let system = rendered_prompt(
        prompt_names::CHANNEL_INGEST_DISTILL_SYSTEM,
        system_version,
        HashMap::new(),
    )
    .await
    .context("rendering managed channel distillation system prompt")?;

    let mut vars = HashMap::new();
    vars.insert("channel".to_string(), channel.to_string());
    vars.insert("header_document".to_string(), header.clone());
    vars.insert("content".to_string(), chunk.to_string());
    vars.insert("content_truncated".to_string(), truncated.to_string());
    let user = rendered_prompt(
        prompt_names::CHANNEL_INGEST_DISTILL_USER,
        user_version,
        vars,
    )
    .await
    .context("rendering managed channel distillation user prompt")?;
    let repair = managed_prompt(
        prompt_names::CHANNEL_INGEST_DISTILL_REPAIR_USER,
        prompt_versions::CHANNEL_INGEST_DISTILL_REPAIR_USER,
    )
    .await
    .context("loading managed channel distillation repair prompt")?;
    Ok(DistillPromptSet {
        system,
        user,
        repair,
    })
}

fn coalesced_thread_content(
    messages: &[(MailMessageMeta, DistillContent)],
    ctx: &DistillContext,
) -> DistillContent {
    if messages.len() == 1 {
        return messages[0].1.clone();
    }

    let mut body = String::new();
    let mut truncated = false;
    let mut had_html = false;
    let mut attachment_count = 0usize;
    for (idx, (message, content)) in messages.iter().enumerate() {
        if idx > 0 {
            body.push_str("\n\n");
        }
        body.push_str(&format!("## Thread message {}\n", idx + 1));
        body.push_str(&build_header_document(message, content.attachment_count));
        body.push_str("\n\n");
        if let Some(chunk) = content.chunks.first() {
            body.push_str(chunk);
        }
        truncated |= content.truncated || content.chunks.len() > 1;
        had_html |= content.had_html;
        attachment_count = attachment_count.saturating_add(content.attachment_count);
    }

    let prepared = prepare_for_distill(&body, ctx.chunk_chars, ctx.max_chunks);
    DistillContent {
        chunks: prepared.chunks,
        truncated: prepared.truncated || truncated,
        had_html,
        attachment_count,
    }
}

// ---------------------------------------------------------------------------
// One queue pass
// ---------------------------------------------------------------------------

/// Counts for one drain pass (log/observability payload; N6 surfaces
/// queue depth via `sync/status`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DistillPassOutcome {
    /// Rows pulled from the queue this pass. Stays 0 on idle passes
    /// (guard says unbound/no-router): the backlog is deliberately not
    /// touched.
    pub drained: usize,
    pub distilled: usize,
    /// Non-local-guard skips + no-content skips + terminal attempt-cap
    /// skips. Unbound/no-router passes skip NOTHING (rows stay pending).
    pub skipped: usize,
    /// Defensive suppressions (should stay 0 — see [`RowAction`]).
    pub suppressed: usize,
    pub failed: usize,
    pub awaiting_content_path: usize,
    pub groups: usize,
    pub coalesced_messages: usize,
    pub concurrency: usize,
    /// Set when the fail-closed guard refused the pass.
    pub unavailable: Option<DistillUnavailable>,
}

#[derive(Debug, Clone, Copy)]
pub struct DistillRunOptions {
    pub concurrency: usize,
    pub coalesce_threads: bool,
    pub brief_contract_version: u32,
    pub summary_max_chars: usize,
    /// Automatic startup work may read only rows at/after this boundary.
    /// Manual passes and ordinary pre-controller callers leave it unset.
    pub min_internal_date: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistillWorkKind {
    Queue,
    Backfill,
}

impl Default for DistillRunOptions {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            coalesce_threads: false,
            brief_contract_version: 1,
            summary_max_chars: MAX_SUMMARY_CHARS,
            min_internal_date: None,
        }
    }
}

#[derive(Debug, Clone)]
struct DistillRowGroup {
    messages: Vec<MailMessageMeta>,
}

fn group_distill_rows(rows: Vec<MailMessageMeta>, coalesce_threads: bool) -> Vec<DistillRowGroup> {
    if !coalesce_threads {
        return rows
            .into_iter()
            .map(|message| DistillRowGroup {
                messages: vec![message],
            })
            .collect();
    }

    let mut groups: Vec<DistillRowGroup> = Vec::new();
    let mut index_by_thread: HashMap<(String, String, String), usize> = HashMap::new();
    for message in rows {
        let key = (
            message.provider.clone(),
            message.account_alias.clone(),
            message.thread_id.clone(),
        );
        if let Some(index) = index_by_thread.get(&key).copied() {
            groups[index].messages.push(message);
        } else {
            index_by_thread.insert(key, groups.len());
            groups.push(DistillRowGroup {
                messages: vec![message],
            });
        }
    }
    for group in &mut groups {
        group.messages.sort_by(|a, b| {
            b.internal_date
                .cmp(&a.internal_date)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
    }
    groups
}

fn merge_outcome(total: &mut DistillPassOutcome, next: DistillPassOutcome) {
    total.drained += next.drained;
    total.distilled += next.distilled;
    total.skipped += next.skipped;
    total.suppressed += next.suppressed;
    total.failed += next.failed;
    total.awaiting_content_path += next.awaiting_content_path;
    total.groups += next.groups;
    total.coalesced_messages += next.coalesced_messages;
}

/// Pull one batch off the queue: pending rows first, then cap-aware
/// retries of failed rows with the remaining budget.
async fn list_batch(
    store: &MailAssistStore,
    ctx: &DistillContext,
    batch: usize,
    min_internal_date: Option<i64>,
) -> Result<Vec<MailMessageMeta>> {
    let mut rows = store
        .list_pending_distill_since(
            &ctx.principal,
            &ctx.workspace,
            batch,
            min_internal_date.unwrap_or(i64::MIN),
        )
        .await?;
    if rows.len() < batch {
        let retryable = store
            .list_retryable_distill_since(
                &ctx.principal,
                &ctx.workspace,
                MAX_DISTILL_ATTEMPTS,
                batch - rows.len(),
                min_internal_date.unwrap_or(i64::MIN),
            )
            .await?;
        rows.extend(retryable);
    }
    Ok(rows)
}

/// Drain one batch of the distillation queue: pending rows first, then
/// cap-aware retries of failed rows with the remaining budget. All seams
/// (guard verdict, fetchers, LLM) are injected so tests run without any
/// live CLI/LLM.
///
/// Guard-failure semantics (see [`DistillUnavailable`]): every failure is
/// IDLE — nothing drained, backlog preserved, one streak-limited log line.
/// Content is never fetched and the LLM is never called on any path.
#[cfg(any(test, feature = "test-fixtures"))]
pub async fn run_distill_pass(
    store: &MailAssistStore,
    ctx: &DistillContext,
    guard: Result<VerifiedLocalBinding, DistillUnavailable>,
    fetchers: &[Box<dyn ContentFetcher>],
    llm: &dyn DistillLlm,
    batch: usize,
) -> Result<DistillPassOutcome> {
    run_distill_pass_with_options(
        store,
        ctx,
        guard,
        fetchers,
        llm,
        batch,
        DistillRunOptions::default(),
    )
    .await
}

pub async fn run_distill_pass_with_options(
    store: &MailAssistStore,
    ctx: &DistillContext,
    guard: Result<VerifiedLocalBinding, DistillUnavailable>,
    fetchers: &[Box<dyn ContentFetcher>],
    llm: &dyn DistillLlm,
    batch: usize,
    options: DistillRunOptions,
) -> Result<DistillPassOutcome> {
    let concurrency = options.concurrency.max(1);
    let mut outcome = DistillPassOutcome::default();
    outcome.concurrency = concurrency;

    if let Err(reason) = guard {
        // Idle — EVERY guard failure preserves the backlog: the queue is not
        // even read, rows stay pending, and the durable obligation survives
        // until a binding permitted by the locality policy appears.
        outcome.unavailable = Some(reason);
        return Ok(outcome);
    }

    let rows = list_batch(store, ctx, batch, options.min_internal_date).await?;
    process_distill_rows(
        store,
        ctx,
        fetchers,
        llm,
        rows,
        options,
        DistillWorkKind::Queue,
    )
    .await
}

async fn process_distill_rows(
    store: &MailAssistStore,
    ctx: &DistillContext,
    fetchers: &[Box<dyn ContentFetcher>],
    llm: &dyn DistillLlm,
    rows: Vec<MailMessageMeta>,
    options: DistillRunOptions,
    work_kind: DistillWorkKind,
) -> Result<DistillPassOutcome> {
    let concurrency = options.concurrency.max(1);
    let mut outcome = DistillPassOutcome {
        concurrency,
        ..Default::default()
    };
    let groups = group_distill_rows(rows, options.coalesce_threads);
    if groups.is_empty() {
        return Ok(outcome);
    }

    let brief_contract_version = options.brief_contract_version;
    let summary_max_chars = options.summary_max_chars.max(1);
    let results = stream::iter(groups)
        .map(|group| {
            process_distill_group(
                store,
                ctx,
                fetchers,
                llm,
                group,
                brief_contract_version,
                summary_max_chars,
                work_kind,
            )
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    for result in results {
        match result {
            Ok(group_outcome) => merge_outcome(&mut outcome, group_outcome),
            Err(error) => {
                outcome.failed += 1;
                warn!(target: LOG_TARGET, error = %error, "distill group failed");
            },
        }
    }
    Ok(outcome)
}

async fn process_distill_group(
    store: &MailAssistStore,
    ctx: &DistillContext,
    fetchers: &[Box<dyn ContentFetcher>],
    llm: &dyn DistillLlm,
    group: DistillRowGroup,
    brief_contract_version: u32,
    summary_max_chars: usize,
    work_kind: DistillWorkKind,
) -> Result<DistillPassOutcome> {
    let mut outcome = DistillPassOutcome {
        drained: group.messages.len(),
        groups: usize::from(!group.messages.is_empty()),
        ..Default::default()
    };
    let mut fetched = Vec::new();

    for message in &group.messages {
        let fetcher = fetchers
            .iter()
            .find(|fetcher| fetcher.provider() == message.provider);
        match plan_row_action(message, fetcher.is_some()) {
            RowAction::DefensiveSuppress => {
                warn!(
                    target: LOG_TARGET,
                    provider = message.provider.as_str(),
                    account = message.account_alias.as_str(),
                    message_id = message.message_id.as_str(),
                    "suppressed row reached the distill queue; re-asserting suppression \
                     (appender short-circuit should have prevented this)"
                );
                if set_state_logged(store, ctx, message, DistillState::Suppressed).await {
                    outcome.suppressed += 1;
                }
            },
            RowAction::AwaitContentPath => {
                debug!(
                    target: LOG_TARGET,
                    provider = message.provider.as_str(),
                    message_id = message.message_id.as_str(),
                    "no content fetcher for provider; row stays pending"
                );
                outcome.awaiting_content_path += 1;
                if work_kind == DistillWorkKind::Backfill {
                    record_work_failure(
                        store,
                        ctx,
                        message,
                        &mut outcome,
                        &anyhow!("no content fetcher registered for provider"),
                        work_kind,
                    )
                    .await;
                }
            },
            RowAction::Distill => {
                let fetcher = fetcher.expect("Distill action implies a fetcher");
                match fetcher.fetch(ctx, message).await {
                    Err(error) => {
                        record_work_failure(store, ctx, message, &mut outcome, &error, work_kind)
                            .await;
                    },
                    Ok(content) if content.chunks.is_empty() => {
                        // No distillable text (empty body, pure-quote
                        // reply, undecodable parts): metadata-only row,
                        // not a failure — retrying cannot conjure text.
                        if work_kind == DistillWorkKind::Backfill {
                            record_work_failure(
                                store,
                                ctx,
                                message,
                                &mut outcome,
                                &anyhow!("source no longer contains distillable content"),
                                work_kind,
                            )
                            .await;
                        } else if set_state_logged(store, ctx, message, DistillState::Skipped).await
                        {
                            outcome.skipped += 1;
                        }
                    },
                    Ok(content) => {
                        fetched.push((message.clone(), content));
                    },
                }
            },
        }
    }

    if fetched.is_empty() {
        return Ok(outcome);
    }

    outcome.coalesced_messages = fetched.len().saturating_sub(1);
    let target = fetched[0].0.clone();
    let content = if fetched.len() == 1 {
        fetched.remove(0).1
    } else {
        coalesced_thread_content(&fetched, ctx)
    };
    let prompts =
        render_distill_prompts_for_contract(&target, &content, brief_contract_version).await?;
    let started = std::time::Instant::now();
    match distill_with_retry_with_policy(
        llm,
        &prompts.system,
        &prompts.user,
        brief_contract_version,
        summary_max_chars,
        &prompts.repair,
    )
    .await
    {
        Ok(output) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            let completed_at = now_ms();
            let evidence_message_ids: Vec<String> = fetched
                .iter()
                .map(|(message, _)| message.message_id.clone())
                .collect();
            let write = store
                .set_distill_result_with_brief_and_evidence_ids(
                    &ctx.principal,
                    &ctx.workspace,
                    &target.provider,
                    &target.account_alias,
                    &target.message_id,
                    &output.summary,
                    &output.intent,
                    output.needs_reply_hint,
                    output.follow_up_hint.as_ref(),
                    output.brief.as_ref(),
                    brief_contract_version,
                    completed_at,
                    &evidence_message_ids,
                )
                .await;
            match write {
                Ok(distill_revision) => {
                    outcome.distilled += 1;
                    // Live feed: input identity (metadata only — the raw
                    // body just dropped) paired with the derived output.
                    store.record_recent_distill(RecentDistillEntry {
                        principal: ctx.principal.clone(),
                        workspace: ctx.workspace.clone(),
                        provider: target.provider.clone(),
                        account_alias: target.account_alias.clone(),
                        thread_id: target.thread_id.clone(),
                        message_id: target.message_id.clone(),
                        subject: target.subject.clone(),
                        from_name: target.from_name.clone(),
                        from_address: target.from_address.clone(),
                        received_at: target.internal_date,
                        summary: output.summary.clone(),
                        intent: output.intent.clone(),
                        brief_contract_version: output
                            .brief
                            .as_ref()
                            .map(|_| brief_contract_version),
                        detail_status: output.brief.as_ref().map(|brief| brief.detail_status),
                        distill_revision: Some(distill_revision),
                        at_ms: completed_at,
                        latency_ms: Some(latency_ms),
                    });
                    if work_kind == DistillWorkKind::Queue {
                        for (message, _) in fetched.iter().skip(1) {
                            if set_state_logged(store, ctx, message, DistillState::Skipped).await {
                                outcome.skipped += 1;
                            }
                        }
                    }
                },
                Err(error) => {
                    warn!(
                        target: LOG_TARGET,
                        message_id = target.message_id.as_str(),
                        error = %error,
                        "failed to persist distill result"
                    );
                    record_work_failure(store, ctx, &target, &mut outcome, &error, work_kind).await;
                    for (message, _) in fetched.iter().skip(1) {
                        record_work_failure(store, ctx, message, &mut outcome, &error, work_kind)
                            .await;
                    }
                },
            }
        },
        Err(error) => {
            record_work_failure(store, ctx, &target, &mut outcome, &error, work_kind).await;
            for (message, _) in fetched.iter().skip(1) {
                record_work_failure(store, ctx, message, &mut outcome, &error, work_kind).await;
            }
        },
    }
    // `content`, `system`, `user` drop here — the raw text never outlives
    // the group's iteration.
    Ok(outcome)
}

async fn record_work_failure(
    store: &MailAssistStore,
    ctx: &DistillContext,
    message: &MailMessageMeta,
    outcome: &mut DistillPassOutcome,
    error: &anyhow::Error,
    work_kind: DistillWorkKind,
) {
    if work_kind == DistillWorkKind::Queue {
        record_distill_failure(store, ctx, message, outcome, error).await;
        return;
    }

    warn!(
        target: LOG_TARGET,
        provider = message.provider.as_str(),
        account = message.account_alias.as_str(),
        message_id = message.message_id.as_str(),
        error = %error,
        "historical distill repair failed; existing safe result preserved"
    );
    outcome.failed += 1;
    if let Err(record_error) = store
        .record_distill_backfill_failure(
            &ctx.principal,
            &ctx.workspace,
            &message.provider,
            &message.account_alias,
            &message.message_id,
            message.distill_revision,
            &error.to_string(),
            now_ms(),
        )
        .await
    {
        debug!(
            target: LOG_TARGET,
            message_id = message.message_id.as_str(),
            error = %record_error,
            "historical distill failure receipt was not persisted"
        );
    }
}

/// Record one failed attempt: transition to `failed` (the store
/// increments `distill_attempts`), then terminally `skipped` once the
/// attempt budget is exhausted — the row degrades to metadata-only, per
/// the design.
async fn record_distill_failure(
    store: &MailAssistStore,
    ctx: &DistillContext,
    message: &MailMessageMeta,
    outcome: &mut DistillPassOutcome,
    error: &anyhow::Error,
) {
    warn!(
        target: LOG_TARGET,
        provider = message.provider.as_str(),
        account = message.account_alias.as_str(),
        message_id = message.message_id.as_str(),
        attempt = message.distill_attempts + 1,
        error = %error,
        "distillation attempt failed"
    );
    if !set_state_logged(store, ctx, message, DistillState::Failed).await {
        return;
    }
    outcome.failed += 1;
    if attempts_exhausted_after(message.distill_attempts) {
        if set_state_logged(store, ctx, message, DistillState::Skipped).await {
            outcome.skipped += 1;
        }
    }
}

/// `set_distill_state` with warn-and-continue semantics (one bad row must
/// not stop the drain). Returns whether the write landed.
async fn set_state_logged(
    store: &MailAssistStore,
    ctx: &DistillContext,
    message: &MailMessageMeta,
    state: DistillState,
) -> bool {
    match store
        .set_distill_state(
            &ctx.principal,
            &ctx.workspace,
            &message.provider,
            &message.account_alias,
            &message.message_id,
            state,
        )
        .await
    {
        Ok(()) => true,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                message_id = message.message_id.as_str(),
                state = state.as_db_str(),
                error = %error,
                "failed to update distill state"
            );
            false
        },
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChannelDistillConfig {
    /// `CHANNEL_DISTILL_ENABLED` kill-switch (default on). Independent of
    /// the guard: the switch stops the worker entirely; the guard decides
    /// whether a running worker may distill.
    pub enabled: bool,
    /// `CHANNEL_DISTILL_INTERVAL_SECS` (default 60): the queue drains at the
    /// local model's pace, decoupled from sync latency.
    pub interval: Duration,
    /// `CHANNEL_DISTILL_STARTUP_DELAY_SECS` (default 120 — after the sync
    /// worker's first pass has had a chance to land rows).
    pub startup_delay: Duration,
    /// `CHANNEL_DISTILL_BATCH` (default 8): rows per tick.
    pub batch: usize,
    /// `CHANNEL_DISTILL_CONCURRENCY` (default 1): maximum in-flight distill
    /// LLM calls per worker tick. Must be matched by the Ollama server's
    /// `OLLAMA_NUM_PARALLEL` or the extra calls queue instead of running.
    pub concurrency: usize,
    /// `CHANNEL_DISTILL_COALESCE_THREADS` (default on): merge same-thread rows
    /// from one batch into a single prompt, using the newest row as the
    /// durable result carrier and marking older included rows metadata-only.
    pub coalesce_threads: bool,
    /// Version of the structured output requested from the existing local
    /// distillation call. Values other than 1/2 normalize to the latest known
    /// contract instead of selecting an unrecognized prompt.
    pub brief_contract_version: u32,
    /// Persisted compatibility-summary ceiling from structured config.
    pub summary_max_chars: usize,
    /// Historical contract repair runs only after the normal queue drains.
    pub backfill_enabled: bool,
    /// Provider-message age bound for historical repair selection.
    pub backfill_lookback_days: u32,
    /// Maximum historical rows attempted per otherwise-idle worker tick.
    pub backfill_batch_size: usize,
    /// Prioritize exact messages backing currently surfaced comm candidates.
    pub backfill_surfaced_first: bool,
}

impl Default for ChannelDistillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
            batch: DEFAULT_BATCH,
            concurrency: DEFAULT_CONCURRENCY,
            coalesce_threads: true,
            brief_contract_version: 1,
            summary_max_chars: MAX_SUMMARY_CHARS,
            backfill_enabled: false,
            backfill_lookback_days: 30,
            backfill_batch_size: 2,
            backfill_surfaced_first: true,
        }
    }
}

impl ChannelDistillConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("CHANNEL_DISTILL_ENABLED") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_DISTILL_INTERVAL_SECS") {
            if secs > 0 {
                config.interval = Duration::from_secs(secs);
            }
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_DISTILL_STARTUP_DELAY_SECS") {
            config.startup_delay = Duration::from_secs(secs);
        }
        if let Some(batch) = env_parse::<usize>("CHANNEL_DISTILL_BATCH") {
            if batch > 0 {
                config.batch = batch;
            }
        }
        if let Some(concurrency) = env_parse::<usize>("CHANNEL_DISTILL_CONCURRENCY") {
            if concurrency > 0 {
                config.concurrency = concurrency;
            }
        }
        if let Ok(raw) = std::env::var("CHANNEL_DISTILL_COALESCE_THREADS") {
            config.coalesce_threads = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
        }
        config
    }

    /// Overlay the structured magician-config policy onto the existing worker
    /// cadence/env controls. Environment variables remain only for the legacy
    /// worker scheduling knobs; the brief data contract is centrally owned.
    pub fn from_settings(settings: &magician::config::ChannelAssistDistillationConfig) -> Self {
        let mut config = Self::from_env();
        config.brief_contract_version = match settings.brief_contract_version {
            1 => 1,
            _ => CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        };
        config.summary_max_chars = settings.summary_max_chars.clamp(1, MAX_SUMMARY_CHARS);
        config.backfill_enabled = settings.backfill.enabled;
        config.backfill_lookback_days = settings.backfill.lookback_days.clamp(1, 3_650);
        config.backfill_batch_size = settings.backfill.batch_size.clamp(1, 64);
        config.backfill_surfaced_first = settings.backfill.surfaced_first;
        config
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

const DISTILL_BACKFILL_SCAN_CAP: usize = 2_000;
const DISTILL_BACKFILL_SURFACED_REF_CAP: usize = 2_000;
const BACKGROUND_SCOPE_CAP: usize = 128;

async fn background_scopes(workspace_layout: &ArtifactV2Workspace) -> Vec<(String, String)> {
    // Tenants only: distillation turns a tenant's incoming messages into
    // context for that tenant. A reserved sink receives no messages, and
    // reaching its store would open the scope's DuckDB and hold it.
    let mut scopes = match workspace_layout.list_tenant_scope_segments().await {
        Ok(scopes) => scopes,
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "failed to enumerate distill scopes; using default scope");
            Vec::new()
        },
    };
    scopes.push((
        DEFAULT_SCOPE_PRINCIPAL.to_string(),
        DEFAULT_SCOPE_WORKSPACE.to_string(),
    ));
    scopes.sort();
    scopes.dedup();
    scopes.truncate(BACKGROUND_SCOPE_CAP);
    let default_scope = (
        DEFAULT_SCOPE_PRINCIPAL.to_string(),
        DEFAULT_SCOPE_WORKSPACE.to_string(),
    );
    if !scopes.contains(&default_scope) {
        scopes.pop();
        scopes.push(default_scope);
        scopes.sort();
    }
    scopes
}

static DISTILL_BACKFILL_NOTIFY: OnceLock<Notify> = OnceLock::new();

pub async fn request_distill_backfill(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
) -> Result<()> {
    store.request_distill_backfill(principal, workspace).await?;
    DISTILL_BACKFILL_NOTIFY
        .get_or_init(Notify::new)
        .notify_one();
    Ok(())
}

pub async fn set_distill_backfill_paused(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    paused: bool,
) -> Result<()> {
    store
        .set_distill_backfill_paused(principal, workspace, paused)
        .await?;
    if !paused {
        DISTILL_BACKFILL_NOTIFY
            .get_or_init(Notify::new)
            .notify_one();
    }
    Ok(())
}

pub fn distill_backfill_runtime_snapshot_json(
    runtime: &DistillBackfillRuntimeState,
) -> serde_json::Value {
    serde_json::json!({
        "paused": runtime.paused,
        "manual_requested": runtime.manual_completed < runtime.manual_requests,
        "manual_requests": runtime.manual_requests,
        "manual_completed": runtime.manual_completed,
        "runs": runtime.runs,
        "selected": runtime.selected,
        "distilled": runtime.distilled,
        "failed": runtime.failed,
        "yielded_pending": runtime.yielded_pending,
        "yielded_dispatch_pressure": runtime.yielded_dispatch_pressure,
        "last_run_at_ms": runtime.last_run_at_ms,
    })
}

pub fn distill_backfill_cutoff_ms(lookback_days: u32, now_ms: i64) -> i64 {
    now_ms.saturating_sub(i64::from(lookback_days).saturating_mul(86_400_000))
}

fn backfill_manual_generation(runtime: &DistillBackfillRuntimeState) -> Option<u64> {
    (runtime.manual_completed < runtime.manual_requests).then_some(runtime.manual_requests)
}

async fn record_backfill_yield(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    dispatch_pressure: bool,
) {
    if let Err(error) = store
        .record_distill_backfill_yield(principal, workspace, dispatch_pressure)
        .await
    {
        warn!(target: LOG_TARGET, principal, workspace, error = %error, "failed to persist distill backfill yield");
    }
}

async fn record_backfill_run(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    outcome: Option<&DistillPassOutcome>,
    manual_generation: Option<u64>,
) {
    let (selected, distilled, failed) = outcome
        .map(|outcome| {
            (
                outcome.drained as u64,
                outcome.distilled as u64,
                outcome.failed as u64,
            )
        })
        .unwrap_or_default();
    if let Err(error) = store
        .record_distill_backfill_run(
            principal,
            workspace,
            manual_generation,
            selected,
            distilled,
            failed,
            now_ms(),
        )
        .await
    {
        warn!(target: LOG_TARGET, principal, workspace, error = %error, "failed to persist distill backfill run");
    }
}

async fn list_distill_backfill_rows(
    store: &MailAssistStore,
    resurfacing_store: Option<&ResurfacingStore>,
    principal: &str,
    workspace: &str,
    config: &ChannelDistillConfig,
) -> Result<Vec<MailMessageMeta>> {
    let now = now_ms();
    let cutoff = distill_backfill_cutoff_ms(config.backfill_lookback_days, now);
    let scan_limit = config
        .backfill_batch_size
        .saturating_mul(32)
        .max(config.backfill_batch_size)
        .min(DISTILL_BACKFILL_SCAN_CAP);
    let mut selected = Vec::new();
    let mut surfaced = HashSet::new();
    if config.backfill_surfaced_first {
        if let Some(resurfacing_store) = resurfacing_store {
            let surfaced_refs = resurfacing_store
                .list_source_refs_by_state(
                    principal,
                    workspace,
                    SourceKind::Comm,
                    CandidateState::Surfaced,
                    DISTILL_BACKFILL_SURFACED_REF_CAP,
                )
                .await?;
            surfaced = surfaced_refs.iter().cloned().collect::<HashSet<_>>();
            let surfaced_keys = surfaced_refs
                .iter()
                .filter_map(|source_ref| parse_comm_source_message_key(source_ref))
                .collect::<Vec<_>>();
            selected = store
                .list_distill_backfill_candidates_by_keys(
                    principal,
                    workspace,
                    config.brief_contract_version,
                    cutoff,
                    now,
                    &surfaced_keys,
                    config.backfill_batch_size,
                )
                .await?;
        }
    }
    if selected.len() < config.backfill_batch_size {
        let selected_keys = selected
            .iter()
            .map(|message| {
                (
                    message.provider.clone(),
                    message.account_alias.clone(),
                    message.message_id.clone(),
                )
            })
            .collect::<HashSet<_>>();
        let mut newest = store
            .list_distill_backfill_candidates(
                principal,
                workspace,
                config.brief_contract_version,
                cutoff,
                now,
                scan_limit,
            )
            .await?;
        prioritize_surfaced_backfill_rows(&mut newest, &surfaced);
        newest.retain(|message| {
            !selected_keys.contains(&(
                message.provider.clone(),
                message.account_alias.clone(),
                message.message_id.clone(),
            ))
        });
        selected.extend(
            newest
                .into_iter()
                .take(config.backfill_batch_size - selected.len()),
        );
    }
    Ok(selected)
}

fn prioritize_surfaced_backfill_rows(
    rows: &mut [MailMessageMeta],
    surfaced_source_refs: &HashSet<String>,
) {
    // Slice sorting is stable, preserving the store's newest-first ordering
    // within the surfaced and non-surfaced buckets.
    rows.sort_by_key(|message| {
        !surfaced_source_refs.contains(&comm_source_ref_for_message(message))
    });
}

/// Background distillation worker handle (MemoryEvalRunner pattern, like
/// [`super::sync::ChannelSyncWorker`]).
#[derive(Debug)]
pub struct ChannelDistillWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelDistillWorker {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        store: MailAssistStore,
        router: Option<Arc<OperationLlmRouter>>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        config: ChannelDistillConfig,
        resurfacing_store: Option<ResurfacingStore>,
        catch_up: Option<Arc<ObserveCatchUpController>>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_periodic(
                workspace_layout,
                store,
                router,
                broadcaster,
                config,
                resurfacing_store,
                catch_up,
                cancel_for_task,
            )
            .await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_periodic(
    workspace_layout: ArtifactV2Workspace,
    store: MailAssistStore,
    router: Option<Arc<OperationLlmRouter>>,
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    config: ChannelDistillConfig,
    resurfacing_store: Option<ResurfacingStore>,
    catch_up: Option<Arc<ObserveCatchUpController>>,
    cancel: CancellationToken,
) {
    if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    // Streak-edge state for the guard warning: one LOUD warn when
    // distillation becomes unavailable (or the reason changes), debug on
    // subsequent ticks, one info on recovery — never a warn per row.
    let mut unavailable_streak: Option<DistillUnavailable> = None;

    run_tick(
        &workspace_layout,
        &store,
        router.as_ref(),
        broadcaster.as_ref(),
        &config,
        resurfacing_store.as_ref(),
        catch_up.as_deref(),
        &mut unavailable_streak,
    )
    .await;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {
                run_tick(
                    &workspace_layout,
                    &store,
                    router.as_ref(),
                    broadcaster.as_ref(),
                    &config,
                    resurfacing_store.as_ref(),
                    catch_up.as_deref(),
                    &mut unavailable_streak,
                )
                .await;
            }
            _ = DISTILL_BACKFILL_NOTIFY.get_or_init(Notify::new).notified() => {
                run_tick(
                    &workspace_layout,
                    &store,
                    router.as_ref(),
                    broadcaster.as_ref(),
                    &config,
                    resurfacing_store.as_ref(),
                    catch_up.as_deref(),
                    &mut unavailable_streak,
                )
                .await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

async fn run_tick(
    workspace_layout: &ArtifactV2Workspace,
    store: &MailAssistStore,
    router: Option<&Arc<OperationLlmRouter>>,
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    config: &ChannelDistillConfig,
    resurfacing_store: Option<&ResurfacingStore>,
    catch_up: Option<&ObserveCatchUpController>,
    unavailable_streak: &mut Option<DistillUnavailable>,
) {
    let guard = resolve_local_provider(router.map(|router| router.as_ref()));
    match (&guard, unavailable_streak.as_ref()) {
        (Err(reason), previous) if previous != Some(reason) => {
            // Every guard failure preserves the backlog; the log level
            // carries the severity. Unbound/no-router is an expected idle
            // state — informational. A provider the locality policy refuses
            // means config disagrees with `privacy.processing.mode` — loud.
            if matches!(reason, DistillUnavailable::NonLocalProvider(_)) {
                warn!(
                    target: LOG_TARGET,
                    reason = %reason,
                    "channel distillation UNAVAILABLE — the binding disagrees with the selected \
                     privacy.processing.mode; backlog preserved (rows stay pending) until the \
                     mapping or the mode agrees"
                );
            } else {
                info!(
                    target: LOG_TARGET,
                    reason = %reason,
                    "distillation idle: '{CHANNEL_INGEST_DISTILL_OPERATION}' unbound — backlog \
                     preserved (rows stay pending); bind it to a local ollama profile in config \
                     to enable"
                );
            }
            *unavailable_streak = Some(reason.clone());
        },
        (Err(reason), _) => {
            debug!(target: LOG_TARGET, reason = %reason, "distillation still unavailable");
        },
        (Ok(_), Some(_)) => {
            info!(
                target: LOG_TARGET,
                "channel distillation available again (local provider bound)"
            );
            *unavailable_streak = None;
        },
        (Ok(_), None) => {},
    }

    for (principal, workspace) in background_scopes(workspace_layout).await {
        let mut scoped_config = config.clone();
        if let Some(controller) = catch_up {
            // Historical contract repair is maintenance, but its age bound
            // must not exceed the same scoped history choice.
            let policy = controller.status(&principal, &workspace).await.policy;
            scoped_config.backfill_lookback_days = scoped_config
                .backfill_lookback_days
                .min(policy.lookback_days);
            if !policy.enabled {
                scoped_config.backfill_enabled = false;
            }
        }
        run_scope_tick(
            workspace_layout,
            store,
            router,
            broadcaster,
            &scoped_config,
            resurfacing_store,
            guard.clone(),
            &principal,
            &workspace,
            catch_up,
        )
        .await;
    }
}

/// One span per scope tick — the distill counterpart to
/// `classify::run_classify_pass_with_concurrency_and_attention`, and declared
/// for the same reason: this worker calls the operation router directly rather
/// than through an agent run, so without a declaring ancestor here every
/// distill model call resolves to no workload class at all.
///
/// `comms_assist` matches what `workload_for_operation` already stamps on the
/// `channel_*` operations this tick dispatches, so the live activity row and
/// the stored `llm_dispatch_batch` row group under one name.
#[allow(clippy::too_many_arguments)]
#[instrument(
    name = "mail_distill_scope_tick",
    skip_all,
    fields(
        activity_kind = KIND_BACKGROUND,
        workload_class = WORKLOAD_COMMS_ASSIST,
        principal = %principal,
        workspace = %workspace,
    )
)]
async fn run_scope_tick(
    workspace_layout: &ArtifactV2Workspace,
    store: &MailAssistStore,
    router: Option<&Arc<OperationLlmRouter>>,
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    config: &ChannelDistillConfig,
    resurfacing_store: Option<&ResurfacingStore>,
    guard: Result<VerifiedLocalBinding, DistillUnavailable>,
    principal: &str,
    workspace: &str,
    catch_up: Option<&ObserveCatchUpController>,
) {
    let auth_root = workspace_layout.capability_auth_root(principal, workspace);
    // Same PATH-augmentation seam as the sync worker: repo_root = process
    // CWD (the assumption `gws_binary()` already makes).
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scope_paths = CapabilityWorkspaceManager::new(workspace_layout.clone(), repo_root)
        .scope_paths(principal, workspace);
    let ctx = DistillContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        workspace_layout: workspace_layout.clone(),
        auth_root,
        scope_paths,
        chunk_chars: distill_chunk_chars(),
        max_chunks: distill_max_chunks(),
    };
    let fetchers = default_content_fetchers();
    let router_llm = router.map(|router| {
        RouterDistillLlm::new(
            Arc::clone(router),
            broadcaster.cloned(),
            principal,
            workspace,
        )
    });
    let llm: &dyn DistillLlm = match router_llm.as_ref() {
        Some(llm) => llm,
        None => &UnavailableLlm,
    };

    let local_binding_available = guard.is_ok();
    // Retention is independent of model availability and startup replay caps.
    // Retire only work older than the user's rolling history window, never
    // in-range rows merely deferred by a startup budget. Each batch releases
    // the writer; a finite pass allows other scopes and current work to run.
    let history_policy = magician::magician_v2::observe_catchup::load_observe_catch_up_policy(
        workspace_layout,
        principal,
        workspace,
    )
    .await;
    let history_floor = distill_backfill_cutoff_ms(history_policy.lookback_days, now_ms());
    let mut expired = 0;
    for _ in 0..32 {
        match store
            .expire_distill_history_batch(principal, workspace, history_floor, MAX_DISTILL_ATTEMPTS)
            .await
        {
            Ok(count) => {
                expired += count;
                if count < crate::channel_assist::store::DISTILL_HISTORY_BATCH {
                    break;
                }
                tokio::task::yield_now().await;
            },
            Err(error) => {
                warn!(target: LOG_TARGET, principal, workspace, %error, "distill history retirement failed");
                break;
            },
        }
    }
    if expired > 0 {
        info!(target: LOG_TARGET, principal, workspace, expired, history_floor, "retired out-of-range distillation work; source records retained");
    }
    let source_id = "message_processing";
    let mut admission: Option<CatchUpAdmission> = None;
    let mut skipped_catch_up = false;
    let mut queue_batch = config.batch;
    let mut min_internal_date = Some(history_floor);
    if let Some(controller) = catch_up {
        match controller
            .begin(
                principal,
                workspace,
                source_id,
                "Local message understanding",
                "messages",
                CatchUpReplayMode::CheckpointedReplay,
                config.batch,
                "Only metadata admitted by message sync is considered; bodies remain local and sensitive rows stay suppressed.",
            )
            .await
        {
            CatchUpDecision::Admit(grant) => {
                queue_batch = grant.max_items;
                min_internal_date = Some(grant.historical_floor_ms.max(history_floor));
                admission = Some(grant);
            },
            CatchUpDecision::SkipHistorical { retain_from_ms, reason } => {
                debug!(target: LOG_TARGET, principal, workspace, reason, "historical local message processing skipped; draining only post-boot rows");
                min_internal_date = Some(retain_from_ms.max(history_floor));
                skipped_catch_up = true;
            },
            CatchUpDecision::Normal => {
                min_internal_date = Some(controller.boot_started_at_ms().max(history_floor));
            },
        }
    }
    let queue_result = run_distill_pass_with_options(
        store,
        &ctx,
        guard,
        &fetchers,
        llm,
        queue_batch,
        DistillRunOptions {
            concurrency: config.concurrency,
            coalesce_threads: config.coalesce_threads,
            brief_contract_version: config.brief_contract_version,
            summary_max_chars: config.summary_max_chars,
            min_internal_date,
        },
    )
    .await;
    if let Some(controller) = catch_up {
        if let Some(grant) = admission {
            match &queue_result {
                Ok(outcome) => {
                    let backlog_preserved = outcome
                        .unavailable
                        .as_ref()
                        .is_some_and(DistillUnavailable::preserves_backlog);
                    let unavailable = outcome.unavailable.as_ref().map(ToString::to_string);
                    controller.complete_with_exhaustion(
                        principal,
                        workspace,
                        grant.clone(),
                        outcome.drained,
                        unavailable.as_deref(),
                        !backlog_preserved && outcome.drained < grant.max_items,
                    )
                },
                Err(error) => controller.complete_with_exhaustion(
                    principal,
                    workspace,
                    grant,
                    0,
                    Some(&error.to_string()),
                    false,
                ),
            }
        } else if skipped_catch_up {
            let error = queue_result.as_ref().err().map(ToString::to_string);
            controller.finish_skipped(
                principal,
                workspace,
                source_id,
                "Local message understanding",
                "messages",
                CatchUpReplayMode::CheckpointedReplay,
                "Only metadata admitted by message sync is considered; bodies remain local and sensitive rows stay suppressed.",
                error.as_deref(),
                true,
            );
        }
    }
    match &queue_result {
        Ok(outcome) if outcome.drained > 0 => {
            info!(
                target: LOG_TARGET,
                drained = outcome.drained,
                groups = outcome.groups,
                coalesced_messages = outcome.coalesced_messages,
                concurrency = outcome.concurrency,
                distilled = outcome.distilled,
                skipped = outcome.skipped,
                failed = outcome.failed,
                suppressed = outcome.suppressed,
                awaiting = outcome.awaiting_content_path,
                unavailable = outcome.unavailable.is_some(),
                "distill pass completed"
            );
        },
        Ok(_) => {
            debug!(target: LOG_TARGET, "distill queue empty");
        },
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "distill pass failed");
        },
    }

    let runtime = match store.distill_backfill_runtime(principal, workspace).await {
        Ok(runtime) => runtime,
        Err(error) => {
            warn!(target: LOG_TARGET, principal, workspace, error = %error, "distill backfill runtime-state read failed");
            return;
        },
    };
    let manual_generation = backfill_manual_generation(&runtime);
    if !config.backfill_enabled && manual_generation.is_none() {
        return;
    }
    if runtime.paused {
        return;
    }
    let queue_idle = matches!(queue_result, Ok(ref outcome) if outcome.drained == 0 && outcome.unavailable.is_none());
    if !local_binding_available || !queue_idle {
        record_backfill_yield(store, principal, workspace, false).await;
        return;
    }
    match store
        .distill_queue_counts(
            principal,
            workspace,
            min_internal_date.unwrap_or(history_floor),
            MAX_DISTILL_ATTEMPTS,
        )
        .await
    {
        Ok(counts) if counts.pending == 0 && counts.retryable == 0 => {},
        Ok(_) => {
            record_backfill_yield(store, principal, workspace, false).await;
            return;
        },
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "distill backfill pending-depth read failed");
            return;
        },
    }
    let rows =
        match list_distill_backfill_rows(store, resurfacing_store, principal, workspace, config)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(target: LOG_TARGET, error = %error, "distill backfill selection failed");
                record_backfill_run(store, principal, workspace, None, manual_generation).await;
                return;
            },
        };
    let backfill_result = process_distill_rows(
        store,
        &ctx,
        &fetchers,
        llm,
        rows,
        DistillRunOptions {
            // Historical repair never adds parallel Ollama load.
            concurrency: 1,
            // Every historical row needs its own durable contract revision;
            // coalescing would repair only the newest carrier and reselect the
            // older rows on the next tick.
            coalesce_threads: false,
            brief_contract_version: config.brief_contract_version,
            summary_max_chars: config.summary_max_chars,
            min_internal_date: None,
        },
        DistillWorkKind::Backfill,
    )
    .await;
    match backfill_result {
        Ok(outcome) => {
            record_backfill_run(
                store,
                principal,
                workspace,
                Some(&outcome),
                manual_generation,
            )
            .await;
            if outcome.drained > 0 {
                info!(
                    target: LOG_TARGET,
                    selected = outcome.drained,
                    distilled = outcome.distilled,
                    failed = outcome.failed,
                    "bounded historical distill repair completed"
                );
            }
        },
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "distill backfill pass failed");
            record_backfill_run(store, principal, workspace, None, manual_generation).await;
        },
    }
}

// ---------------------------------------------------------------------------
// Tests — all seams injected; no live LLM, gws, or ollama calls EVER.
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use serde::Deserialize;
    use tempfile::TempDir;

    use super::super::adapter_registry::GMAIL_PROVIDER;
    use super::super::types::ChannelLane;
    use super::super::types::{
        MailRecordOrigin, MailThreadRecord, MessageDirection, MAIL_ASSIST_SCHEMA_VERSION,
    };
    use super::*;

    #[tokio::test]
    async fn distill_backfill_runtime_pause_resume_is_scope_local() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let principal = "pause-test-principal";
        let workspace = "pause-test-workspace";
        set_distill_backfill_paused(&store, principal, workspace, true)
            .await
            .unwrap();
        let runtime = store
            .distill_backfill_runtime(principal, workspace)
            .await
            .unwrap();
        assert_eq!(
            distill_backfill_runtime_snapshot_json(&runtime)["paused"],
            true
        );
        set_distill_backfill_paused(&store, principal, workspace, false)
            .await
            .unwrap();
        let runtime = store
            .distill_backfill_runtime(principal, workspace)
            .await
            .unwrap();
        assert_eq!(
            distill_backfill_runtime_snapshot_json(&runtime)["paused"],
            false
        );
    }

    // ─── fixtures ────────────────────────────────────────────────────────

    #[derive(Debug, Deserialize)]
    struct GoldenFixtureFile {
        schema_version: u32,
        privacy: String,
        cases: Vec<GoldenFixtureCase>,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenFixtureCase {
        id: String,
        synthetic: bool,
        input: GoldenFixtureInput,
        legacy_snapshot: GoldenLegacySnapshot,
        expected: GoldenExpected,
        v2_output: serde_json::Value,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenFixtureInput {
        channel: String,
        subject: String,
        body: String,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenLegacySnapshot {
        distill_summary: String,
        classification: String,
        route: String,
        candidate_digest: String,
        card_line: String,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenExpected {
        lane: String,
        information_type: String,
        detail_status: String,
        required_action: bool,
        required_fact_fragments: Vec<String>,
    }

    fn golden_fixtures() -> GoldenFixtureFile {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../magician/tests/fixtures/worth_a_look_information_briefs_v2.json"
        )))
        .expect("Worth a Look golden fixture file must parse")
    }

    fn message(id: &str, internal_date: i64) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: GMAIL_PROVIDER.to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "thread-1".to_string(),
            message_id: id.to_string(),
            provider_cursor: None,
            label_ids: vec!["INBOX".to_string()],
            subject: Some("synthetic subject".to_string()),
            from_name: None,
            from_address: Some("sender@example.com".to_string()),
            to_domains: vec!["example.org".to_string()],
            cc_domains: Vec::new(),
            internal_date,
            observed_at: internal_date + 100,
            direction: Some(MessageDirection::Inbound),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[test]
    fn historical_repair_prioritizes_surfaced_then_keeps_newest_order() {
        let newest = message("newest", 3_000);
        let surfaced = message("surfaced", 1_000);
        let middle = message("middle", 2_000);
        let surfaced_refs = HashSet::from([comm_source_ref_for_message(&surfaced)]);
        let mut rows = vec![newest, middle, surfaced];

        prioritize_surfaced_backfill_rows(&mut rows, &surfaced_refs);

        assert_eq!(
            rows.iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["surfaced", "newest", "middle"]
        );
    }

    #[tokio::test]
    async fn manual_repair_generation_does_not_lose_a_concurrent_request() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let principal = "backfill-generation-test";
        let workspace = "isolated";
        request_distill_backfill(&store, principal, workspace)
            .await
            .unwrap();
        let runtime = store
            .distill_backfill_runtime(principal, workspace)
            .await
            .unwrap();
        let first = backfill_manual_generation(&runtime).unwrap();
        request_distill_backfill(&store, principal, workspace)
            .await
            .unwrap();

        record_backfill_run(&store, principal, workspace, None, Some(first)).await;
        let runtime = store
            .distill_backfill_runtime(principal, workspace)
            .await
            .unwrap();
        let second =
            backfill_manual_generation(&runtime).expect("the later request must remain pending");
        assert!(second > first);
        record_backfill_run(&store, principal, workspace, None, Some(second)).await;
        let runtime = store
            .distill_backfill_runtime(principal, workspace)
            .await
            .unwrap();
        assert!(backfill_manual_generation(&runtime).is_none());
    }

    #[tokio::test]
    async fn distill_background_scopes_include_real_scopes_and_default_with_a_cap() {
        let tmp = TempDir::new().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(tmp.path());
        for index in 0..(BACKGROUND_SCOPE_CAP + 4) {
            std::fs::create_dir_all(
                workspace_layout.scope_root("aaa", &format!("workspace-{index:03}")),
            )
            .unwrap();
        }

        let scopes = background_scopes(&workspace_layout).await;

        assert_eq!(scopes.len(), BACKGROUND_SCOPE_CAP);
        assert!(scopes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(scopes.contains(&(
            DEFAULT_SCOPE_PRINCIPAL.to_string(),
            DEFAULT_SCOPE_WORKSPACE.to_string()
        )));
    }

    fn thread_record() -> MailThreadRecord {
        MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: GMAIL_PROVIDER.to_string(),
            account_alias: "acct-a".to_string(),
            account_email: None,
            thread_id: "thread-1".to_string(),
            lane: ChannelLane::UserAssist,
            subject: Some("synthetic subject".to_string()),
            latest_summary: None,
            latest_from_name: None,
            latest_from_address: None,
            recipient_domains: Vec::new(),
            label_ids: Vec::new(),
            message_count: 1,
            last_message_at: Some(1_000),
            provider_cursor: None,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: 1,
            last_observed_at: 1,
        }
    }

    fn test_ctx(root: &std::path::Path) -> DistillContext {
        let layout = ArtifactV2Workspace::new(root);
        let scope_paths = CapabilityWorkspaceManager::new(layout, root.to_path_buf())
            .scope_paths("alpha", "prod");
        DistillContext {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            workspace_layout: ArtifactV2Workspace::new(root),
            auth_root: root.join("nonexistent-auth-root"),
            scope_paths,
            chunk_chars: 6000,
            max_chunks: 8,
        }
    }

    /// The guard verdict used by tests that exercise the available path.
    fn local_binding() -> VerifiedLocalBinding {
        VerifiedLocalBinding {
            profile: "test-local-ollama".to_string(),
            kind: LLMProviderKind::Ollama,
        }
    }

    /// Scripted LLM: pops canned replies and records every call.
    struct ScriptedLlm {
        replies: Mutex<VecDeque<String>>,
        calls: Mutex<Vec<(String, String)>>,
        response_schemas: Mutex<Vec<Value>>,
    }

    impl ScriptedLlm {
        fn new(replies: &[&str]) -> Self {
            // A filtered satellite test run does not execute the runtime
            // builder. Install the real versioned prompt fixture explicitly;
            // keep production's missing-manager refusal unchanged.
            use magician::magician_v2::prompts::{
                json_storage::JsonStorageConfig, set_global_prompt_manager, JsonPromptStorage,
                PromptManager,
            };
            let storage = JsonPromptStorage::new(JsonStorageConfig {
                storage_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../data/magician_v2/prompts"),
                ..Default::default()
            })
            .unwrap();
            set_global_prompt_manager(Arc::new(PromptManager::new(Arc::new(storage))));
            Self {
                replies: Mutex::new(replies.iter().map(|r| r.to_string()).collect()),
                calls: Mutex::new(Vec::new()),
                response_schemas: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl DistillLlm for ScriptedLlm {
        async fn complete(&self, system: &str, user: &str) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((system.to_string(), user.to_string()));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("scripted llm exhausted"))
        }

        async fn complete_with_response_schema(
            &self,
            system: &str,
            user: &str,
            response_schema: &Value,
        ) -> Result<String> {
            self.response_schemas
                .lock()
                .unwrap()
                .push(response_schema.clone());
            self.complete(system, user).await
        }
    }

    /// LLM that must never be reached (guard tests).
    struct ForbiddenLlm {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl DistillLlm for ForbiddenLlm {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("LLM must not be called on this path"))
        }
    }

    /// Fetcher returning fixed chunks (or an error), recording fetch order.
    struct FixedFetcher {
        chunks: Vec<String>,
        truncated: bool,
        fail: bool,
        fetched: Arc<Mutex<Vec<String>>>,
    }

    impl FixedFetcher {
        fn boxed(
            chunks: &[&str],
            truncated: bool,
            fail: bool,
            fetched: Arc<Mutex<Vec<String>>>,
        ) -> Box<dyn ContentFetcher> {
            Box::new(Self {
                chunks: chunks.iter().map(|c| c.to_string()).collect(),
                truncated,
                fail,
                fetched,
            })
        }
    }

    #[async_trait]
    impl ContentFetcher for FixedFetcher {
        fn provider(&self) -> &'static str {
            GMAIL_PROVIDER
        }

        async fn fetch(
            &self,
            _ctx: &DistillContext,
            message: &MailMessageMeta,
        ) -> Result<DistillContent> {
            self.fetched
                .lock()
                .unwrap()
                .push(message.message_id.clone());
            if self.fail {
                anyhow::bail!("synthetic fetch failure");
            }
            Ok(DistillContent {
                chunks: self.chunks.clone(),
                truncated: self.truncated,
                had_html: false,
                attachment_count: 0,
            })
        }
    }

    const VALID_REPLY: &str = r#"{"summary": "Sender proposes moving the sync.", "intent": "scheduling", "needs_reply_hint": true, "follow_up_hint": {"kind": "schedule", "actor": "owner", "counterparty": "Sender", "due_text": "Friday morning", "urgency": "normal", "rationale": "The sender expects scheduling confirmation.", "key_details": ["Friday morning"]}}"#;
    const VALID_V2_REPLY: &str = r#"{"summary":"From July 1, voucher rewards are capped at 5,000 points per month.","intent":"fyi","needs_reply_hint":false,"follow_up_hint":{"kind":"none"},"brief":{"schema_version":2,"information_type":"change_notice","key_facts":["Affected: brand vouchers"],"changes":[{"aspect":"Monthly reward cap","before":null,"after":"5,000 points","effective_text":"July 1, 2026"}],"temporal_facts":[{"kind":"effective","text":"July 1, 2026","at_ms":999}],"stated_action":null,"detail_status":"complete","missing_details":[]}}"#;

    // ─── fail-closed guard ───────────────────────────────────────────────

    #[test]
    fn guard_requires_an_explicit_local_binding() {
        // Unbound → unavailable (the default-profile fallback never counts)
        // — and unavailability that PRESERVES the backlog.
        assert_eq!(
            require_local_provider(None),
            Err(DistillUnavailable::OperationUnbound)
        );
        assert!(DistillUnavailable::OperationUnbound.preserves_backlog());
        assert!(DistillUnavailable::RouterUnavailable.preserves_backlog());
        // Remote kinds → unavailable, naming the offender. Under the
        // locality policy every unavailability is idle: backlog preserved
        // (a misrouted binding means config disagrees with the selected
        // mode — the queue is never destroyed to signal that).
        assert_eq!(
            require_local_provider(Some(("p-remote", &LLMProviderKind::OpenAI))),
            Err(DistillUnavailable::NonLocalProvider("openai".to_string()))
        );
        assert_eq!(
            require_local_provider(Some(("p-remote", &LLMProviderKind::Anthropic))),
            Err(DistillUnavailable::NonLocalProvider(
                "anthropic".to_string()
            ))
        );
        assert!(DistillUnavailable::NonLocalProvider("openai".to_string()).preserves_backlog());
        // Custom kinds are NOT local, even ollama-sounding ones — only the
        // real local family passes.
        assert_eq!(
            require_local_provider(Some((
                "p-proxy",
                &LLMProviderKind::Custom("ollama-proxy".to_string())
            ))),
            Err(DistillUnavailable::NonLocalProvider(
                "ollama-proxy".to_string()
            ))
        );
        // The local family is the only Ok — and the verdict carries the
        // exact profile the dispatch must pin to.
        assert_eq!(
            require_local_provider(Some(("p-local", &LLMProviderKind::Ollama))),
            Ok(VerifiedLocalBinding {
                profile: "p-local".to_string(),
                kind: LLMProviderKind::Ollama,
            })
        );
        // No router at all → unavailable.
        assert_eq!(
            resolve_local_provider(None),
            Err(DistillUnavailable::RouterUnavailable)
        );
    }

    #[test]
    fn dispatch_wiring_decides_locality_through_the_constructor() {
        use magicllm::config::{LLMRouterConfig, OperationProfileSelector};

        // The guard core is exercised directly above. This pins the wiring
        // BETWEEN a constructor and that core — the part a call site can get
        // wrong while every existing assertion stays green.
        fn router_binding(operation: &str, provider: &str) -> Arc<OperationLlmRouter> {
            let mut config = LLMRouterConfig::default();
            config.profiles.insert(
                "p-under-test".to_string(),
                // Built through serde so a newly added profile field cannot
                // rot this fixture into a compile error.
                serde_json::from_value(serde_json::json!({
                    "provider": provider,
                    "model": "m",
                }))
                .expect("profile fixture"),
            );
            config.operation_mapping.insert(
                operation.to_string(),
                OperationProfileSelector::from("p-under-test"),
            );
            Arc::new(OperationLlmRouter::new(Some(config)))
        }

        // `new` is channel ingest: input that has reached no model, so a
        // remote binding must be refused through the dispatch, not merely in
        // the pure core.
        let remote = RouterDistillLlm::new(
            router_binding(CHANNEL_INGEST_DISTILL_OPERATION, "openai"),
            None,
            "alpha",
            "prod",
        );
        assert_eq!(
            remote.dispatch.verify_binding(),
            Err(DistillUnavailable::NonLocalProvider("openai".to_string()))
        );

        // ...and the local family is admitted, so the refusal above is the
        // guard working rather than the fixture failing to bind at all.
        let local = RouterDistillLlm::new(
            router_binding(CHANNEL_INGEST_DISTILL_OPERATION, "ollama"),
            None,
            "alpha",
            "prod",
        );
        assert_eq!(
            local.dispatch.verify_binding(),
            Ok(VerifiedLocalBinding {
                profile: "p-under-test".to_string(),
                kind: LLMProviderKind::Ollama,
            })
        );

        // `require_local = false` is the transcript case: the content already
        // went to whichever model conducted the session, so a remote binding
        // is admitted deliberately. This is the difference the flag makes.
        let permissive = RouterDistillLlm::new_for_operation(
            router_binding("transcript_distill_under_test", "openai"),
            None,
            "alpha",
            "prod",
            "transcript_distill_under_test",
            false,
        );
        assert_eq!(
            permissive.dispatch.verify_binding(),
            Ok(VerifiedLocalBinding {
                profile: "p-under-test".to_string(),
                kind: LLMProviderKind::OpenAI,
            })
        );

        // Unbound stays OFF even on the permissive path: dropping the
        // locality requirement must never promote `default_profile` into a
        // binding, or enabling a consumer stops being a deliberate act.
        let unbound = RouterDistillLlm::new_for_operation(
            router_binding("a_different_operation", "openai"),
            None,
            "alpha",
            "prod",
            "transcript_distill_under_test",
            false,
        );
        assert_eq!(
            unbound.dispatch.verify_binding(),
            Err(DistillUnavailable::OperationUnbound)
        );
    }

    #[tokio::test]
    async fn guard_failure_preserves_backlog_and_never_reaches_the_llm() {
        // §5 regression fence: a provider the locality policy refuses is a
        // config-vs-mode disagreement, NOT a reason to destroy a durable
        // queue. Pending rows survive untouched; no fetch, no LLM call.
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![message("m-1", 1_000), message("m-2", 2_000)],
            )
            .await
            .unwrap();

        let forbidden = ForbiddenLlm {
            calls: AtomicUsize::new(0),
        };
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["body"],
            false,
            false,
            fetched.clone(),
        )];

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Err(DistillUnavailable::NonLocalProvider("openai".to_string())),
            &fetchers,
            &forbidden,
            8,
        )
        .await
        .unwrap();

        // Idle, not degraded: nothing drained, nothing skipped, and the
        // refusal is reported so the streak logger can surface it loudly.
        assert_eq!(outcome.drained, 0);
        assert_eq!(outcome.skipped, 0);
        assert_eq!(
            outcome.unavailable,
            Some(DistillUnavailable::NonLocalProvider("openai".to_string()))
        );
        // The product promise: no content fetch, no LLM call.
        assert_eq!(forbidden.calls.load(Ordering::SeqCst), 0);
        assert!(fetched.lock().unwrap().is_empty());
        for id in ["m-1", "m-2"] {
            let row = store
                .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.distill_state, DistillState::Pending);
            assert!(row.summary.is_none());
        }
    }

    #[tokio::test]
    async fn startup_queue_floor_never_selects_older_pending_rows() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![message("old", 1_000), message("new", 3_000)],
            )
            .await
            .unwrap();

        let rows = list_batch(&store, &test_ctx(tmp.path()), 8, Some(2_000))
            .await
            .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["new"]
        );
        assert!(store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "old")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn distill_history_retirement_runs_without_a_model_and_preserves_in_range_work() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let now = now_ms();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![
                    message("old", now - 8 * 86_400_000),
                    message("current", now),
                ],
            )
            .await
            .unwrap();
        let ctx = test_ctx(tmp.path());
        run_scope_tick(
            &ctx.workspace_layout,
            &store,
            None,
            None,
            &ChannelDistillConfig::default(),
            None,
            Err(DistillUnavailable::RouterUnavailable),
            "alpha",
            "prod",
            None,
        )
        .await;
        assert_eq!(
            store
                .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "old")
                .await
                .unwrap()
                .unwrap()
                .distill_state,
            DistillState::Expired
        );
        assert_eq!(
            store
                .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "current")
                .await
                .unwrap()
                .unwrap()
                .distill_state,
            DistillState::Pending
        );
    }

    #[tokio::test]
    async fn unbound_guard_preserves_backlog_as_pending() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![message("m-1", 1_000), message("m-2", 2_000)],
            )
            .await
            .unwrap();
        let forbidden = ForbiddenLlm {
            calls: AtomicUsize::new(0),
        };
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["body"],
            false,
            false,
            fetched.clone(),
        )];

        for reason in [
            DistillUnavailable::OperationUnbound,
            DistillUnavailable::RouterUnavailable,
        ] {
            let outcome = run_distill_pass(
                &store,
                &test_ctx(tmp.path()),
                Err(reason.clone()),
                &fetchers,
                &forbidden,
                8,
            )
            .await
            .unwrap();

            // Idle, not degraded: nothing drained, nothing skipped — the
            // durable obligation outlives the unconfigured stretch.
            assert_eq!(outcome.drained, 0);
            assert_eq!(outcome.skipped, 0);
            assert_eq!(outcome.unavailable, Some(reason));
            assert_eq!(forbidden.calls.load(Ordering::SeqCst), 0);
            assert!(fetched.lock().unwrap().is_empty());
            for id in ["m-1", "m-2"] {
                let row = store
                    .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(row.distill_state, DistillState::Pending);
                assert_eq!(row.distill_attempts, 0);
            }
        }

        // The rows are still live queue members once a binding appears.
        assert_eq!(
            store
                .list_pending_distill("alpha", "prod", 8)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    // ─── dispatch pinning (the guard-verified profile IS the dispatch) ──

    /// Spy seam: returns a fixed guard verdict and records the binding
    /// every dispatch was pinned to.
    struct SpyPinnedDispatch {
        verdict: Result<VerifiedLocalBinding, DistillUnavailable>,
        dispatched: Mutex<Vec<VerifiedLocalBinding>>,
        response_schemas: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl PinnedLocalDispatch for SpyPinnedDispatch {
        fn verify_binding(&self) -> Result<VerifiedLocalBinding, DistillUnavailable> {
            self.verdict.clone()
        }

        async fn dispatch_pinned(
            &self,
            binding: &VerifiedLocalBinding,
            _system: &str,
            _user: &str,
        ) -> Result<String> {
            self.dispatched.lock().unwrap().push(binding.clone());
            Ok(VALID_REPLY.to_string())
        }

        async fn dispatch_pinned_with_response_schema(
            &self,
            binding: &VerifiedLocalBinding,
            _system: &str,
            _user: &str,
            response_schema: &Value,
        ) -> Result<String> {
            self.dispatched.lock().unwrap().push(binding.clone());
            self.response_schemas
                .lock()
                .unwrap()
                .push(response_schema.clone());
            Ok(VALID_V2_REPLY.to_string())
        }
    }

    #[tokio::test]
    async fn complete_pins_dispatch_to_the_guard_verified_binding() {
        let binding = VerifiedLocalBinding {
            profile: "guard-verified-profile".to_string(),
            kind: LLMProviderKind::Ollama,
        };
        let spy = Arc::new(SpyPinnedDispatch {
            verdict: Ok(binding.clone()),
            dispatched: Mutex::new(Vec::new()),
            response_schemas: Mutex::new(Vec::new()),
        });
        let llm = RouterDistillLlm::with_dispatch(spy.clone());

        llm.complete("sys", "user").await.unwrap();

        // The dispatch carried EXACTLY the binding the guard returned —
        // profile pin + provider lock both derive from it.
        let dispatched = spy.dispatched.lock().unwrap();
        assert_eq!(*dispatched, vec![binding]);
    }

    #[tokio::test]
    async fn structured_complete_pins_dispatch_and_forwards_the_exact_schema() {
        let binding = VerifiedLocalBinding {
            profile: "guard-verified-profile".to_string(),
            kind: LLMProviderKind::Ollama,
        };
        let spy = Arc::new(SpyPinnedDispatch {
            verdict: Ok(binding.clone()),
            dispatched: Mutex::new(Vec::new()),
            response_schemas: Mutex::new(Vec::new()),
        });
        let llm = RouterDistillLlm::with_dispatch(spy.clone());
        let schema = distill_response_schema(CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION);

        llm.complete_with_response_schema("sys", "user", &schema)
            .await
            .unwrap();

        assert_eq!(*spy.dispatched.lock().unwrap(), vec![binding]);
        assert_eq!(*spy.response_schemas.lock().unwrap(), vec![schema]);
    }

    #[tokio::test]
    async fn complete_refuses_dispatch_when_the_guard_fails() {
        for verdict in [
            DistillUnavailable::OperationUnbound,
            DistillUnavailable::NonLocalProvider("openai".to_string()),
        ] {
            let spy = Arc::new(SpyPinnedDispatch {
                verdict: Err(verdict),
                dispatched: Mutex::new(Vec::new()),
                response_schemas: Mutex::new(Vec::new()),
            });
            let llm = RouterDistillLlm::with_dispatch(spy.clone());

            let err = llm.complete("sys", "user").await.unwrap_err();
            assert!(err.to_string().contains("distill guard refused dispatch"));
            assert!(spy.dispatched.lock().unwrap().is_empty());
        }
    }

    // ─── strict-JSON parsing + retry ─────────────────────────────────────

    #[test]
    fn provider_schema_tracks_the_managed_v1_and_v2_contracts() {
        let v1 = distill_response_schema(1);
        assert_eq!(v1["type"], "object");
        assert_eq!(v1["additionalProperties"], false);
        assert!(v1["properties"].get("brief").is_none());
        assert_eq!(
            v1["required"],
            json!(["summary", "intent", "needs_reply_hint", "follow_up_hint"])
        );

        let v2 = distill_response_schema(CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION);
        assert!(v2["required"]
            .as_array()
            .is_some_and(|fields| fields.contains(&json!("brief"))));
        assert_eq!(v2["properties"]["brief"]["type"], "object");
        assert_eq!(
            v2["properties"]["brief"]["properties"]["schema_version"]["enum"],
            json!([CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION])
        );
        assert_eq!(
            v2["properties"]["brief"]["properties"]["temporal_facts"]["items"]
                ["additionalProperties"],
            false
        );
    }

    #[test]
    fn parse_accepts_strict_json_and_normalizes_intent() {
        let parsed = parse_distill_output(VALID_REPLY).unwrap();
        assert_eq!(parsed.summary, "Sender proposes moving the sync.");
        assert_eq!(parsed.intent, "scheduling");
        assert!(parsed.needs_reply_hint);
        let hint = parsed.follow_up_hint.as_ref().unwrap();
        assert_eq!(hint.kind, "schedule");
        assert_eq!(hint.actor.as_deref(), Some("owner"));
        assert_eq!(hint.due_text.as_deref(), Some("Friday morning"));
        assert_eq!(hint.key_details, vec!["Friday morning"]);

        // Code-fenced replies are tolerated (outermost-object slice).
        let fenced = format!("```json\n{VALID_REPLY}\n```");
        assert_eq!(parse_distill_output(&fenced).unwrap().intent, "scheduling");

        // Missing hint defaults false; off-taxonomy intent → other.
        let odd = r#"{"summary": "Note.", "intent": "Marketing Blast"}"#;
        let parsed = parse_distill_output(odd).unwrap();
        assert_eq!(parsed.intent, "other");
        assert!(!parsed.needs_reply_hint);
        assert!(parsed.follow_up_hint.is_none());
        // Case/whitespace-normalized taxonomy hits stay themselves.
        assert_eq!(normalize_intent("  Needs_Reply "), "needs_reply");

        // Failures: no object, wrong shape, empty summary.
        assert!(parse_distill_output("no json here").is_err());
        let shape_error = parse_distill_output(r#"{"intent": "fyi"}"#).unwrap_err();
        assert!(shape_error.to_string().contains("missing field `summary`"));
        assert!(parse_distill_output(r#"{"summary": "   ", "intent": "fyi"}"#).is_err());

        // Oversized summaries are clamped, not failed.
        let long = format!(r#"{{"summary": "{}", "intent": "fyi"}}"#, "s".repeat(2000));
        assert_eq!(
            parse_distill_output(&long).unwrap().summary.chars().count(),
            MAX_SUMMARY_CHARS
        );
    }

    #[test]
    fn parse_redacts_every_persisted_follow_up_text_field() {
        let raw = r#"{
            "summary":"Pay INR 125000 by 2026-07-12 using https://pay.example/x",
            "intent":"action_request",
            "follow_up_hint":{
                "kind":"owner_owes",
                "counterparty":"owner@example.com account ABCDE12345",
                "due_text":"2026-07-12 token=super-secret-value",
                "rationale":"Open https://pay.example/x with password hunter2",
                "key_details":["Amount INR 125000 due 2026-07-12 card 4111111111111234"]
            }
        }"#;

        let parsed = parse_distill_output(raw).unwrap();
        let hint = parsed.follow_up_hint.unwrap();
        assert_eq!(
            parsed.summary,
            "Pay INR 125000 by 2026-07-12 using [link omitted]"
        );
        assert_eq!(
            hint.counterparty.as_deref(),
            Some("[email omitted] account ****2345")
        );
        assert_eq!(
            hint.due_text.as_deref(),
            Some("2026-07-12 token [secret omitted]")
        );
        assert_eq!(
            hint.rationale.as_deref(),
            Some("Open [link omitted] with password [secret omitted]")
        );
        assert_eq!(
            hint.key_details,
            vec!["Amount INR 125000 due 2026-07-12 card ****1234"]
        );
    }

    #[test]
    fn golden_information_briefs_are_synthetic_complete_and_contract_valid() {
        let fixtures = golden_fixtures();
        assert_eq!(fixtures.schema_version, 1);
        assert_eq!(fixtures.privacy, "synthetic_only_no_private_source_content");
        assert!(fixtures.cases.len() >= 12);

        let mut ids = std::collections::HashSet::new();
        for fixture in fixtures.cases {
            assert!(ids.insert(fixture.id.clone()), "duplicate fixture id");
            assert!(fixture.synthetic, "{} is not marked synthetic", fixture.id);
            assert!(
                matches!(fixture.input.channel.as_str(), "email" | "chat" | "message"),
                "{} has unsupported fixture channel",
                fixture.id
            );
            assert!(!fixture.input.subject.trim().is_empty());
            assert!(!fixture.input.body.trim().is_empty());
            assert!(!fixture.legacy_snapshot.distill_summary.trim().is_empty());
            assert!(!fixture.legacy_snapshot.classification.trim().is_empty());
            assert!(!fixture.legacy_snapshot.candidate_digest.trim().is_empty());
            assert!(!fixture.legacy_snapshot.card_line.trim().is_empty());
            assert!(matches!(
                fixture.legacy_snapshot.route.as_str(),
                "worth_a_look" | "follow_ups"
            ));
            assert!(matches!(
                fixture.expected.lane.as_str(),
                "worth_a_look" | "follow_ups"
            ));

            let raw = serde_json::to_string(&fixture.v2_output).unwrap();
            let output =
                parse_distill_output_for_contract(&raw, CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION)
                    .unwrap_or_else(|error| panic!("{} failed v2 parsing: {error}", fixture.id));
            let brief = output.brief.as_ref().expect("v2 parser requires a brief");
            assert_eq!(
                serde_json::to_value(brief.information_type).unwrap(),
                serde_json::Value::String(fixture.expected.information_type.clone()),
                "{} information type",
                fixture.id
            );
            assert_eq!(
                serde_json::to_value(brief.detail_status).unwrap(),
                serde_json::Value::String(fixture.expected.detail_status.clone()),
                "{} detail status",
                fixture.id
            );
            let normalized = serde_json::to_string(brief).unwrap().to_lowercase();
            for fact in &fixture.expected.required_fact_fragments {
                assert!(
                    normalized.contains(&fact.to_lowercase()),
                    "{} lost required fact {fact:?}: {normalized}",
                    fixture.id
                );
            }
            let required_action = output.needs_reply_hint
                || output.follow_up_hint.is_some()
                || output.intent == "action_request";
            assert_eq!(
                required_action, fixture.expected.required_action,
                "{} required-action contract",
                fixture.id
            );
        }
    }

    #[test]
    fn v2_contract_requires_semantic_evidence_and_ignores_model_epochs() {
        let parsed = parse_distill_output_for_contract(
            VALID_V2_REPLY,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .unwrap();
        let temporal = &parsed.brief.unwrap().temporal_facts[0];
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 7, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(
            temporal.at_ms,
            Some(expected),
            "model epoch is ignored and source text is materialized deterministically"
        );

        let missing_brief = r#"{"summary":"Rules changed.","intent":"fyi"}"#;
        assert!(parse_distill_output_for_contract(
            missing_brief,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION
        )
        .unwrap_err()
        .to_string()
        .contains("required"));

        let vague_change = r#"{"summary":"Rules changed.","intent":"fyi","brief":{"schema_version":2,"information_type":"change_notice","key_facts":[],"changes":[],"temporal_facts":[],"stated_action":null,"detail_status":"complete","missing_details":[]}}"#;
        assert!(parse_distill_output_for_contract(
            vague_change,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION
        )
        .unwrap_err()
        .to_string()
        .contains("concrete"));

        let generic_summary_with_hidden_fact = r#"{"summary":"The policy changed.","intent":"fyi","brief":{"schema_version":2,"information_type":"change_notice","key_facts":[],"changes":[{"aspect":"Monthly cap","before":null,"after":"5,000 points","effective_text":null}],"temporal_facts":[],"stated_action":null,"detail_status":"complete","missing_details":[]}}"#;
        let reconciled_change = parse_distill_output_for_contract(
            generic_summary_with_hidden_fact,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .expect("structured change value repairs the redundant summary");
        assert!(reconciled_change.summary.contains("5,000 points"));
        assert_eq!(
            reconciled_change.brief.unwrap().summary,
            reconciled_change.summary
        );

        let long_summary_change = serde_json::json!({
            "summary": "Background context ".repeat(100),
            "intent": "fyi",
            "brief": {
                "schema_version": 2,
                "information_type": "change_notice",
                "key_facts": [],
                "changes": [{
                    "aspect": "Monthly cap",
                    "before": null,
                    "after": "5,000 points",
                    "effective_text": null
                }],
                "temporal_facts": [],
                "stated_action": null,
                "detail_status": "complete",
                "missing_details": []
            }
        })
        .to_string();
        let reconciled_long_change = parse_distill_output_for_contract(
            &long_summary_change,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .expect("length clamp retains the structured compatibility fact");
        assert!(reconciled_long_change
            .summary
            .starts_with("Change to Monthly cap: 5,000 points."));
        assert!(reconciled_long_change.summary.chars().count() <= MAX_SUMMARY_CHARS);
        let tightly_bounded_change = parse_distill_output_with_policy(
            &long_summary_change,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
            64,
        )
        .expect("compatibility repair retains the caller's summary bound");
        assert!(tightly_bounded_change
            .summary
            .starts_with("Change to Monthly cap: 5,000 points."));
        assert!(tightly_bounded_change.summary.chars().count() <= 64);

        let request_with_structured_action = r#"{"summary":"A response is requested.","intent":"fyi","needs_reply_hint":false,"follow_up_hint":null,"brief":{"schema_version":2,"information_type":"request","key_facts":[],"changes":[],"temporal_facts":[],"stated_action":"Confirm attendance","detail_status":"complete","missing_details":[]}}"#;
        let reconciled_request = parse_distill_output_for_contract(
            request_with_structured_action,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .expect("structured action repairs compatibility hints");
        assert_eq!(reconciled_request.intent, "action_request");
        assert!(reconciled_request.needs_reply_hint);

        let structured_source_gap = r#"{"summary":"Rules changed.","intent":"fyi","brief":{"schema_version":2,"information_type":"change_notice","key_facts":[],"changes":[],"temporal_facts":[],"stated_action":null,"detail_status":"source_omits_details","missing_details":["the new limit"]}}"#;
        let reconciled_gap = parse_distill_output_for_contract(
            structured_source_gap,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .expect("structured missing detail repairs the compatibility summary");
        assert!(reconciled_gap
            .summary
            .starts_with("Source omits: the new limit."));

        let omitted_without_gap = r#"{"summary":"Rules changed.","intent":"fyi","brief":{"schema_version":2,"information_type":"change_notice","key_facts":[],"changes":[],"temporal_facts":[],"stated_action":null,"detail_status":"source_omits_details","missing_details":[]}}"#;
        assert!(parse_distill_output_for_contract(
            omitted_without_gap,
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION
        )
        .unwrap_err()
        .to_string()
        .contains("name the missing"));
    }

    #[tokio::test]
    async fn v2_semantic_failure_gets_one_targeted_repair_retry() {
        let vague_change = r#"{"summary":"Rules changed.","intent":"fyi","brief":{"schema_version":2,"information_type":"change_notice","key_facts":[],"changes":[],"temporal_facts":[],"stated_action":null,"detail_status":"complete","missing_details":[]}}"#;
        let llm = ScriptedLlm::new(&[vague_change, VALID_V2_REPLY]);
        let output = distill_with_retry_for_contract(
            &llm,
            "system",
            "user",
            CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        )
        .await
        .unwrap();
        assert!(output.brief.is_some());
        assert_eq!(llm.call_count(), 2);
        let schemas = llm.response_schemas.lock().unwrap();
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0], schemas[1]);
        assert!(schemas[0]["required"]
            .as_array()
            .is_some_and(|fields| fields.contains(&json!("brief"))));
        let calls = llm.calls.lock().unwrap();
        assert!(calls[1].1.contains("Validation error"));
        assert!(calls[1].1.contains("concrete before/after change"));
    }

    #[test]
    fn managed_v2_prompt_files_match_the_contract_metadata() {
        for (raw, version) in [
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../data/magician_v2/prompts/channel_ingest_distill_system_v1.1.0.json"
                )),
                "1.1.0",
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../data/magician_v2/prompts/channel_ingest_distill_user_v1.1.1.json"
                )),
                "1.1.1",
            ),
            (
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../data/magician_v2/prompts/channel_ingest_distill_repair_user_v1.0.0.json"
                )),
                "1.0.0",
            ),
        ] {
            let prompt: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(prompt["version"], version);
            assert_eq!(prompt["metadata"]["category"], "ChannelAssist");
        }
        let system: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../data/magician_v2/prompts/channel_ingest_distill_system_v1.1.0.json"
        )))
        .unwrap();
        let content =
            system["content"]
                .as_array()
                .unwrap()
                .iter()
                .fold(String::new(), |mut joined, line| {
                    joined.push_str(line.as_str().unwrap());
                    joined.push('\n');
                    joined
                });
        assert!(content.contains("\"schema_version\": 2"));
        assert!(content.contains("source_omits_details"));
        assert!(content.contains("Never invent"));
    }

    #[tokio::test]
    async fn retry_appends_fixit_suffix_then_fails() {
        // Garbage then valid: succeeds on the retry, whose prompt carries
        // the fix-it suffix.
        let llm = ScriptedLlm::new(&["not json at all", VALID_REPLY]);
        let out = distill_with_retry(&llm, "sys", "user prompt")
            .await
            .unwrap();
        assert_eq!(out.intent, "scheduling");
        assert_eq!(llm.call_count(), 2);
        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls[0].1, "user prompt");
        assert!(calls[1].1.starts_with("user prompt"));
        assert!(calls[1].1.contains("ONLY the corrected JSON object"));
        drop(calls);

        // Garbage twice: fails after exactly two calls.
        let llm = ScriptedLlm::new(&["still not json", "also not json"]);
        assert!(distill_with_retry(&llm, "sys", "user prompt")
            .await
            .is_err());
        assert_eq!(llm.call_count(), 2);
    }

    // ─── per-row planning ────────────────────────────────────────────────

    #[test]
    fn suppressed_rows_are_defensively_suppressed_and_cap_math_holds() {
        let mut suppressed_flag = message("m-1", 1_000);
        suppressed_flag.sensitive_suppressed = true; // inconsistent upstream row
        assert_eq!(
            plan_row_action(&suppressed_flag, true),
            RowAction::DefensiveSuppress
        );
        let mut suppressed_state = message("m-2", 1_000);
        suppressed_state.distill_state = DistillState::Suppressed;
        assert_eq!(
            plan_row_action(&suppressed_state, true),
            RowAction::DefensiveSuppress
        );
        assert_eq!(
            plan_row_action(&message("m-3", 1_000), false),
            RowAction::AwaitContentPath
        );
        assert_eq!(
            plan_row_action(&message("m-4", 1_000), true),
            RowAction::Distill
        );

        // Attempt cap: the third failure is terminal.
        assert!(!attempts_exhausted_after(0));
        assert!(!attempts_exhausted_after(1));
        assert!(attempts_exhausted_after(2));
    }

    #[test]
    fn channel_labels_stay_generic() {
        assert_eq!(channel_label("gmail"), "email");
        assert_eq!(channel_label("whatsapp"), "chat");
        assert_eq!(channel_label("whatsapp_kapso"), "chat");
        assert_eq!(channel_label("some_future_provider"), "message");
    }

    // ─── full pass over a real (temp) store ──────────────────────────────

    #[tokio::test]
    async fn pass_distills_pending_rows_newest_first_and_rolls_the_thread_summary() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", thread_record())
            .await
            .unwrap();
        // The queue drains newest-first so recent correspondence catches up first.
        store
            .append_messages(
                "alpha",
                "prod",
                vec![message("m-new", 2_000), message("m-old", 1_000)],
            )
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["synthetic message body"],
            false,
            false,
            fetched.clone(),
        )];
        let llm = ScriptedLlm::new(&[VALID_REPLY, VALID_REPLY]);

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.drained, 2);
        assert_eq!(outcome.distilled, 2);
        assert_eq!(outcome.failed, 0);
        assert_eq!(*fetched.lock().unwrap(), vec!["m-new", "m-old"]);

        for id in ["m-old", "m-new"] {
            let row = store
                .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.distill_state, DistillState::Done);
            assert_eq!(
                row.summary.as_deref(),
                Some("Sender proposes moving the sync.")
            );
            assert_eq!(row.intent.as_deref(), Some("scheduling"));
        }
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                GMAIL_PROVIDER,
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some("Sender proposes moving the sync.")
        );
        // Queue fully drained.
        assert!(store
            .list_pending_distill("alpha", "prod", 8)
            .await
            .unwrap()
            .is_empty());
        // The user prompts carry the header metadata, body, channel word, and
        // an explicit false truncation field for single-chunk content.
        let calls = llm.calls.lock().unwrap();
        assert!(calls
            .iter()
            .all(|(_, user)| user.contains("synthetic message body")));
        assert!(calls
            .iter()
            .all(|(_, user)| user.contains("Subject: synthetic subject")));
        assert!(calls.iter().all(|(_, user)| user.contains("email")));
        assert!(calls
            .iter()
            .all(|(_, user)| user.contains("Content truncated: false")));
    }

    #[tokio::test]
    async fn v2_pass_persists_safe_brief_revision_and_recent_status_in_one_call() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", thread_record())
            .await
            .unwrap();
        store
            .append_messages("alpha", "prod", vec![message("m-v2", 2_000)])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["Voucher rewards change on July 1."],
            false,
            false,
            fetched,
        )];
        let llm = ScriptedLlm::new(&[VALID_V2_REPLY]);

        let outcome = run_distill_pass_with_options(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            1,
            DistillRunOptions {
                brief_contract_version: CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                summary_max_chars: MAX_SUMMARY_CHARS,
                ..DistillRunOptions::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.distilled, 1);
        assert_eq!(
            llm.call_count(),
            1,
            "V2 must reuse the existing distill call"
        );

        let row = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-v2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_contract_version, Some(2));
        assert_eq!(row.distill_revision, Some(1));
        assert!(row.distilled_at.is_some());
        let brief = row.distill_brief.expect("V2 brief must persist atomically");
        assert_eq!(brief.detail_status, ChannelDetailStatus::Complete);
        assert_eq!(brief.changes[0].after.as_deref(), Some("5,000 points"));

        let recent = store.recent_distill("alpha", "prod", 1);
        assert_eq!(recent[0].brief_contract_version, Some(2));
        assert_eq!(recent[0].detail_status, Some(ChannelDetailStatus::Complete));
        assert_eq!(recent[0].distill_revision, Some(1));
    }

    #[tokio::test]
    async fn coalesced_pass_distills_one_thread_update_and_skips_included_older_rows() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", thread_record())
            .await
            .unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![message("m-new", 2_000), message("m-old", 1_000)],
            )
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["synthetic message body"],
            false,
            false,
            fetched.clone(),
        )];
        let llm = ScriptedLlm::new(&[VALID_REPLY]);

        let outcome = run_distill_pass_with_options(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            8,
            DistillRunOptions {
                concurrency: 2,
                coalesce_threads: true,
                ..DistillRunOptions::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome.drained, 2);
        assert_eq!(outcome.groups, 1);
        assert_eq!(outcome.coalesced_messages, 1);
        assert_eq!(outcome.concurrency, 2);
        assert_eq!(outcome.distilled, 1);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(*fetched.lock().unwrap(), vec!["m-new", "m-old"]);
        assert_eq!(llm.call_count(), 1);

        let newer = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(newer.distill_state, DistillState::Done);
        assert_eq!(
            newer.summary.as_deref(),
            Some("Sender proposes moving the sync.")
        );
        let older = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(older.distill_state, DistillState::Skipped);
        assert!(older.summary.is_none());

        let calls = llm.calls.lock().unwrap();
        assert!(calls[0].1.contains("## Thread message 1"));
        assert!(calls[0].1.contains("## Thread message 2"));
    }

    #[tokio::test]
    async fn pass_notes_truncation_and_distills_first_chunk_only() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages("alpha", "prod", vec![message("m-long", 1_000)])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["first chunk text", "second chunk text"],
            false,
            false,
            fetched,
        )];
        let llm = ScriptedLlm::new(&[VALID_REPLY]);

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.distilled, 1);
        let calls = llm.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        // First chunk only, with the truncation disclosed to the model.
        assert!(calls[0].1.contains("first chunk text"));
        assert!(!calls[0].1.contains("second chunk text"));
        assert!(calls[0].1.contains("Content truncated: true"));
    }

    #[tokio::test]
    async fn empty_content_skips_without_an_llm_call() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages("alpha", "prod", vec![message("m-empty", 1_000)])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(&[], false, false, fetched)];
        let forbidden = ForbiddenLlm {
            calls: AtomicUsize::new(0),
        };

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &forbidden,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.skipped, 1);
        assert_eq!(forbidden.calls.load(Ordering::SeqCst), 0);
        let row = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-empty")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Skipped);
        // No attempt was burned: an empty body is not a failure.
        assert_eq!(row.distill_attempts, 0);
    }

    #[tokio::test]
    async fn suppressed_row_in_queue_is_defensively_suppressed_without_fetching() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        // An inconsistent row: flagged sensitive but (wrongly) pending —
        // the appender short-circuit should have made it suppressed.
        let mut inconsistent = message("m-otp", 1_000);
        inconsistent.sensitive_suppressed = true;
        store
            .append_messages("alpha", "prod", vec![inconsistent])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(
            &["body"],
            false,
            false,
            fetched.clone(),
        )];
        let forbidden = ForbiddenLlm {
            calls: AtomicUsize::new(0),
        };

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &forbidden,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.suppressed, 1);
        assert!(
            fetched.lock().unwrap().is_empty(),
            "content must not be fetched"
        );
        assert_eq!(forbidden.calls.load(Ordering::SeqCst), 0);
        let row = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-otp")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Suppressed);
        assert!(row.summary.is_none());
    }

    #[tokio::test]
    async fn failures_retry_via_the_failed_pass_and_cap_to_skipped() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages("alpha", "prod", vec![message("m-flaky", 1_000)])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let fetchers = vec![FixedFetcher::boxed(&["body"], false, true, fetched.clone())];
        let llm = ScriptedLlm::new(&[]);

        // Attempt 1 (pending pass) and attempt 2 (retry pass): failed.
        for expected_attempts in [1, 2] {
            let outcome = run_distill_pass(
                &store,
                &test_ctx(tmp.path()),
                Ok(local_binding()),
                &fetchers,
                &llm,
                8,
            )
            .await
            .unwrap();
            assert_eq!(outcome.drained, 1);
            assert_eq!(outcome.failed, 1);
            assert_eq!(outcome.skipped, 0);
            let row = store
                .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-flaky")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.distill_state, DistillState::Failed);
            assert_eq!(row.distill_attempts, expected_attempts);
        }

        // Attempt 3: cap reached → terminal skipped (metadata-only forever).
        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.skipped, 1);
        let row = store
            .get_message("alpha", "prod", GMAIL_PROVIDER, "acct-a", "m-flaky")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Skipped);
        assert_eq!(row.distill_attempts, MAX_DISTILL_ATTEMPTS);

        // The queue is quiet now: nothing pending, nothing retryable.
        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &llm,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.drained, 0);
        assert_eq!(
            fetched.lock().unwrap().len(),
            3,
            "exactly three fetch attempts"
        );
    }

    #[tokio::test]
    async fn rows_without_a_content_fetcher_stay_pending() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut foreign = message("m-wa", 1_000);
        foreign.provider = "whatsapp".to_string();
        store
            .append_messages("alpha", "prod", vec![foreign])
            .await
            .unwrap();
        let fetched = Arc::new(Mutex::new(Vec::new()));
        // Only the gmail fetcher is registered.
        let fetchers = vec![FixedFetcher::boxed(&["body"], false, false, fetched)];
        let forbidden = ForbiddenLlm {
            calls: AtomicUsize::new(0),
        };

        let outcome = run_distill_pass(
            &store,
            &test_ctx(tmp.path()),
            Ok(local_binding()),
            &fetchers,
            &forbidden,
            8,
        )
        .await
        .unwrap();
        assert_eq!(outcome.awaiting_content_path, 1);
        assert_eq!(forbidden.calls.load(Ordering::SeqCst), 0);
        let row = store
            .get_message("alpha", "prod", "whatsapp", "acct-a", "m-wa")
            .await
            .unwrap()
            .unwrap();
        // The durable obligation survives until N4 registers a fetcher.
        assert_eq!(row.distill_state, DistillState::Pending);
        assert_eq!(row.distill_attempts, 0);
    }
}
