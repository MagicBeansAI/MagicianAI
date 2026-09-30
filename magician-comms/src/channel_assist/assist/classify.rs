//! Body-blind thread classifier for comms Follow-ups.
//!
//! Reads distilled, non-suppressed, not-yet-annotated threads (the neutral
//! `ThreadContext`: metadata + the locally-derived `latest_summary`, NEVER a
//! raw body) and writes a `MailThreadAnnotation` with a `label` +
//! `confidence` + `reason` + `proposed_action`. The shared attention router
//! decides whether high-confidence actionable labels surface as Follow-ups
//! (`needs_approval`) or remain quiet `classified` drops. Thread coalescing and
//! material-change detection keep classification idempotent without repeatedly
//! picking unchanged threads.
//!
//! ## Body-blind ⇒ no locality guard (unlike distill)
//!
//! The distiller sees raw content, so it is pinned to a local ollama profile
//! (`channel_ingest_distill`, fail-closed). The classifier sees ONLY the
//! already-local summaries, so it's an ordinary op dispatch bound to
//! [`CHANNEL_CLASSIFY_OPERATION`] — the operator can point it at a capable
//! remote model or a local one; that's a config choice. UNBOUND ⇒ the pass is
//! idle (no default-remote surprise), surfaced in `sync/status` via
//! `pending_classify`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use magician::magician_v2::analytics::runtime_activity_layer::{
    KIND_BACKGROUND, WORKLOAD_COMMS_ASSIST,
};
use uuid::Uuid;

use crate::channel_assist::attention_learning::{
    SemanticExtractionInput, SemanticFeatureExtractor,
};
use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::attention::learning::{
    serialize_semantic_envelope, ChannelAttentionSemanticEnvelope, SemanticExtractorIdentity,
};
use magician::magician_v2::attention_funnel::{
    route_attention_candidate, AttentionAction, AttentionActionKind, AttentionCandidate,
    AttentionFunnelStage, AttentionLane, AttentionRouteContext, AttentionRouteEvent,
    AttentionScope, AttentionSource, AttentionSourceFamily, AttentionSourceKind,
    AttentionTraceStatus, AttentionUrgency, RouteOutcome,
};
use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;
use magician::magician_v2::prompts::{
    managed_prompt, names as prompt_names, rendered_prompt, versions as prompt_versions,
};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::channel_providers::channel_label;
use super::store::{
    ClassificationAnnotationApplyResult, ClassificationAnnotationDisposition, MailAssistStore,
    RecentHandledFollowUp, ThreadClassifyRow,
};
use super::types::{
    derive_channel_required_action, sanitize_follow_up_key_detail, ChannelLane,
    ChannelRequiredAction, ChannelRequiredActionKind, MailAnnotationState, MailThreadAnnotation,
    MAIL_ASSIST_SCHEMA_VERSION,
};

const LOG_TARGET: &str = "magician::channel_assist::classify";

/// The operation the classifier LLM dispatches to (bind in
/// `operation_mapping`; unbound ⇒ idle). Body-blind ⇒ local OR remote is fine.
pub const CHANNEL_CLASSIFY_OPERATION: &str = "channel_classify";

/// Actionable labels at/above this confidence surface as `needs_approval`.
const CLASSIFY_HIGH_CONFIDENCE: f64 = 0.7;

/// After the owner explicitly acknowledges/dismisses a follow-up in a thread,
/// pass that decision as bounded classifier context for a short window. The
/// model, not this constant, decides whether a newer item is a pure repeat.
const RECENT_HANDLED_FOLLOW_UP_SUPPRESS_MS: i64 = 14 * 24 * 60 * 60 * 1000;
const FEEDBACK_TUNING_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const FEEDBACK_TUNING_SAMPLE_LIMIT: usize = 1_000;

const DEFAULT_INTERVAL_SECS: u64 = 120;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 180;
const DEFAULT_BATCH: usize = 8;
const DEFAULT_CONCURRENCY: usize = 1;

/// The v1 label vocabulary — mutually exclusive. The prompt store owns the
/// definitions; this is the validation allow-list (an off-vocabulary label
/// from the model is coerced to `fyi`, never trusted). `needs_reply` /
/// `follow_up` are actionable (they can produce a Today approval).
///
/// `pub` since plan 3.1 so the assist seam's policy-table tests can pin the
/// vocabulary; production callers remain in-module.
pub const LABELS: [&str; 4] = ["needs_reply", "follow_up", "fyi", "no_action"];

/// Whether a label is actionable (can produce a Today approval). `pub` since
/// plan 3.1 for the seam's policy-table tests; production callers remain
/// in-module.
pub fn is_actionable(label: &str) -> bool {
    matches!(label, "needs_reply" | "follow_up")
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// LLM seam (body-blind — no guard/pin, just op dispatch)
// ---------------------------------------------------------------------------

/// The single seam through which classifier prompts reach a model. Tests
/// inject canned implementations; production is [`RouterClassifyLlm`].
#[async_trait]
pub trait ClassifyLlm: Send + Sync {
    /// Whether `channel_classify` is explicitly bound in `operation_mapping`.
    /// `false` ⇒ the pass stays idle (no default-remote dispatch).
    fn bound(&self) -> bool;
    /// Stable configured identity recorded with extracted features. Test seams
    /// and legacy implementations may omit it without affecting routing.
    fn semantic_extractor_identity(&self) -> SemanticExtractorIdentity {
        SemanticExtractorIdentity::default()
    }
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

/// Production seam: ordinary (unpinned) op dispatch through the router. Holds
/// the broadcaster + scope so each call emits `LLMResponseReceived` telemetry
/// (the channel-assist ops bypass the executor layer that normally emits — see
/// `super::telemetry`).
pub struct RouterClassifyLlm {
    router: Arc<OperationLlmRouter>,
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    principal: String,
    workspace: String,
}

impl RouterClassifyLlm {
    pub fn new(
        router: Arc<OperationLlmRouter>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        let principal = principal.into();
        let workspace = workspace.into();
        Self {
            router: Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(
                principal.clone(),
                workspace.clone(),
            )))),
            broadcaster,
            principal,
            workspace,
        }
    }

    /// Reuse the exact channel-classify producer contract while submitting
    /// asynchronous coverage calls through the shared background lane.
    pub fn for_semantic_backfill(mut self) -> Self {
        self.router = Arc::new(
            self.router
                .with_dispatch_priority(magicllm::dispatch::Priority::Background),
        );
        self
    }
}

#[async_trait]
impl ClassifyLlm for RouterClassifyLlm {
    fn bound(&self) -> bool {
        self.router
            .explicit_binding_for_operation(CHANNEL_CLASSIFY_OPERATION)
            .is_some()
    }

    fn semantic_extractor_identity(&self) -> SemanticExtractorIdentity {
        let operation = LLMOperation::Other(CHANNEL_CLASSIFY_OPERATION.to_string());
        SemanticExtractorIdentity {
            profile: self
                .router
                .explicit_binding_for_operation(CHANNEL_CLASSIFY_OPERATION)
                .map(|(profile, _)| profile),
            model: self
                .router
                .get_config_for_operation(&operation)
                .ok()
                .map(|profile| profile.model.clone()),
        }
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let operation = LLMOperation::Other(CHANNEL_CLASSIFY_OPERATION.to_string());
        let started = std::time::Instant::now();
        let response = self
            .router
            .generate_for_operation_with_system(&operation, Some(system), user)
            .await
            .context("channel classify LLM call failed")?;
        super::telemetry::emit_mail_llm_call(
            self.broadcaster.as_ref(),
            CHANNEL_CLASSIFY_OPERATION,
            &self.principal,
            &self.workspace,
            &response,
            true,
            started.elapsed().as_millis() as u64,
        );
        Ok(response.content)
    }
}

/// The asynchronous coverage worker deliberately reuses the same managed
/// operation, prompt versions, output parser, and extractor identity as the
/// foreground classifier. Its caller supplies only locally derived safe input.
#[async_trait]
impl SemanticFeatureExtractor for RouterClassifyLlm {
    fn available(&self) -> bool {
        ClassifyLlm::bound(self)
    }

    fn identity(&self) -> SemanticExtractorIdentity {
        ClassifyLlm::semantic_extractor_identity(self)
    }

    fn prompt_version(&self) -> &'static str {
        prompt_versions::CHANNEL_CLASSIFY
    }

    fn foreground_pressure(&self) -> bool {
        self.router.has_foreground_dispatch_pressure()
    }

    async fn extract(
        &self,
        input: &SemanticExtractionInput,
    ) -> Result<ChannelAttentionSemanticEnvelope> {
        let system = classify_system_prompt().await?;
        let metadata = &input.source_metadata;
        let value = |key: &str, default: &str| {
            metadata
                .get(key)
                .cloned()
                .unwrap_or_else(|| default.to_string())
        };
        let mut vars = HashMap::new();
        vars.insert("channel".to_string(), value("channel", "message"));
        vars.insert("lane".to_string(), value("lane", "the owner's"));
        vars.insert("subject".to_string(), value("subject", "(none)"));
        vars.insert("sender".to_string(), value("sender", "(unknown)"));
        vars.insert(
            "recipient_domains".to_string(),
            value("recipient_domains", "(none)"),
        );
        vars.insert("label_ids".to_string(), value("label_ids", "(none)"));
        vars.insert("message_count".to_string(), value("message_count", "1"));
        vars.insert("age".to_string(), value("age", "unknown"));
        vars.insert(
            "latest_message_id".to_string(),
            value("latest_message_id", "safe-source"),
        );
        vars.insert(
            "latest_message_age".to_string(),
            value("latest_message_age", "unknown"),
        );
        vars.insert(
            "latest_direction".to_string(),
            value("latest_direction", "unknown"),
        );
        vars.insert(
            "latest_intent".to_string(),
            value("latest_intent", "unknown"),
        );
        vars.insert(
            "needs_reply_hint".to_string(),
            value("needs_reply_hint", "false"),
        );
        vars.insert(
            "follow_up_hint".to_string(),
            value("follow_up_hint", "none"),
        );
        vars.insert("recent_handled_followups".to_string(), "none".to_string());
        vars.insert(
            "safe_brief".to_string(),
            input
                .safe_brief
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_else(|| "none".to_string()),
        );
        vars.insert(
            "summary".to_string(),
            input.safe_summary.clone().unwrap_or_default(),
        );
        let user = rendered_prompt(
            prompt_names::CHANNEL_CLASSIFY_USER,
            prompt_versions::CHANNEL_CLASSIFY,
            vars,
        )
        .await
        .context("rendering managed semantic extraction prompt")?;
        // Constrain the existing feature contract at generation time as well
        // as validation time. Backfill consumes only semantics; legacy routing
        // fields are not regenerated or applied to the source annotation.
        let operation = LLMOperation::Other(CHANNEL_CLASSIFY_OPERATION.to_string());
        let (profile, _) = self
            .router
            .explicit_binding_for_operation(CHANNEL_CLASSIFY_OPERATION)
            .context("semantic extraction operation is unbound")?;
        let started = std::time::Instant::now();
        let response = self
            .router
            .generate_for_operation_with_system_pinned_and_response_format(
                &operation,
                Some(&system),
                &user,
                &profile,
                None,
                magicllm::LLMResponseFormat::JsonSchema {
                    schema: semantic_backfill_response_schema(),
                },
            )
            .await
            .context("semantic extraction LLM call failed")?;
        super::telemetry::emit_mail_llm_call(
            self.broadcaster.as_ref(),
            CHANNEL_CLASSIFY_OPERATION,
            &self.principal,
            &self.workspace,
            &response,
            true,
            started.elapsed().as_millis() as u64,
        );
        let mut envelope = parse_classification(&response.content)?.semantic_features;
        let identity = ClassifyLlm::semantic_extractor_identity(self);
        envelope.input_revision = input.source_revision_number;
        envelope.source_revision = Some(input.source_revision.clone());
        envelope.model = identity.model;
        envelope.profile = identity.profile;
        Ok(envelope)
    }
}

fn semantic_backfill_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object", "additionalProperties": false,
        "required": ["label", "semantic_features"],
        "properties": {
            "label": {"type": "string", "enum": LABELS},
            "semantic_features": magician::magician_v2::attention::learning::actionability::semantic_features_response_schema()
        }
    })
}

fn lane_word(lane: &str) -> &str {
    match lane {
        "envoy" => "the agent's (Presto's)",
        _ => "the owner's",
    }
}

fn age_phrase(last_message_at: Option<i64>, now: i64) -> String {
    match last_message_at {
        Some(ts) if ts > 0 => {
            let days = (now - ts).max(0) / 86_400_000;
            match days {
                0 => "today".to_string(),
                1 => "1 day ago".to_string(),
                n => format!("{n} days ago"),
            }
        },
        _ => "unknown".to_string(),
    }
}

fn recent_handled_context_text(recent: &[RecentHandledFollowUp], now: i64) -> String {
    if recent.is_empty() {
        return "none".to_string();
    }
    let rows = recent
        .iter()
        .map(|item| {
            serde_json::json!({
                "state": item.state.as_db_str(),
                "label": item.label,
                "reason": item.reason,
                "proposed_action": item.proposed_action,
                "handled": age_phrase(Some(item.updated_at), now),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows).unwrap_or_else(|_| "none".to_string())
}

fn user_vars(
    row: &ThreadClassifyRow,
    now: i64,
    recent_handled: &[RecentHandledFollowUp],
) -> HashMap<String, String> {
    let sender = match (&row.from_name, &row.from_address) {
        (Some(name), Some(addr)) => format!("{name} <{addr}>"),
        (Some(name), None) => name.clone(),
        (None, Some(addr)) => addr.clone(),
        (None, None) => "(unknown)".to_string(),
    };
    let mut vars = HashMap::new();
    vars.insert("channel".into(), channel_label(&row.provider).to_string());
    vars.insert("lane".into(), lane_word(&row.lane).to_string());
    vars.insert(
        "subject".into(),
        row.subject.clone().unwrap_or_else(|| "(none)".into()),
    );
    vars.insert("sender".into(), sender);
    vars.insert(
        "recipient_domains".into(),
        if row.recipient_domains.is_empty() {
            "(none)".into()
        } else {
            row.recipient_domains.join(", ")
        },
    );
    vars.insert(
        "label_ids".into(),
        if row.label_ids.is_empty() {
            "(none)".into()
        } else {
            row.label_ids.join(", ")
        },
    );
    vars.insert("message_count".into(), row.message_count.to_string());
    vars.insert("age".into(), age_phrase(row.last_message_at, now));
    vars.insert("latest_message_id".into(), row.latest_message_id.clone());
    vars.insert(
        "latest_message_age".into(),
        age_phrase(Some(row.latest_message_at), now),
    );
    vars.insert(
        "latest_direction".into(),
        row.latest_direction
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
    );
    vars.insert(
        "latest_intent".into(),
        row.latest_intent
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
    );
    vars.insert(
        "needs_reply_hint".into(),
        if row.needs_reply_hint {
            "true"
        } else {
            "false"
        }
        .to_string(),
    );
    vars.insert(
        "follow_up_hint".into(),
        row.follow_up_hint
            .as_ref()
            .and_then(|hint| serde_json::to_string(hint).ok())
            .unwrap_or_else(|| "none".to_string()),
    );
    vars.insert(
        "recent_handled_followups".into(),
        recent_handled_context_text(recent_handled, now),
    );
    vars.insert(
        "summary".into(),
        row.latest_summary.clone().unwrap_or_default(),
    );
    vars.insert(
        "safe_brief".into(),
        row.distill_brief
            .as_ref()
            .and_then(|brief| serde_json::to_string(brief).ok())
            .unwrap_or_else(|| "none".to_string()),
    );
    vars
}

// ---------------------------------------------------------------------------
// Output parsing (pure, strict-ish JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ClassifierOutput {
    label: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    needs_reply: Option<bool>,
    #[serde(default)]
    action_kind: Option<String>,
    #[serde(default)]
    follow_up_kind: Option<String>,
    #[serde(default)]
    action_owner: Option<String>,
    #[serde(default)]
    due_text: Option<String>,
    #[serde(default)]
    urgency: Option<String>,
    #[serde(default)]
    key_details: Option<serde_json::Value>,
    #[serde(default)]
    repeat_of_recently_handled: Option<bool>,
    #[serde(default)]
    proposed_action: Option<serde_json::Value>,
    /// Optional Slice-2 feature block. It is parsed independently below: a
    /// malformed block degrades to `invalid` and never fails classification.
    #[serde(default)]
    semantic_features: Option<serde_json::Value>,
}

/// A validated classification. `label` is guaranteed in [`LABELS`];
/// `confidence` is clamped to `[0, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub label: String,
    pub confidence: f64,
    pub reason: Option<String>,
    pub proposed_action: Option<serde_json::Value>,
    pub repeat_of_recently_handled: bool,
    pub semantic_features: ChannelAttentionSemanticEnvelope,
}

/// Strip a ```json … ``` fence (or bare ```) if the model wrapped its JSON.
fn strip_code_fence(raw: &str) -> &str {
    let t = raw.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop an optional language tag on the first line, and the trailing fence.
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.trim_start_matches(['\n', '\r']);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Extract the first balanced JSON object from model text, tolerating stray
/// prose before/after the object while respecting braces inside strings.
fn first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&raw[start..end]);
                }
            },
            _ => {},
        }
    }
    None
}

fn clean_classifier_text(raw: Option<String>, max_chars: usize) -> Option<String> {
    let value = raw?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(max_chars)
            .collect(),
    )
}

fn normalize_classifier_token(raw: Option<String>, allowed: &[&str], default: &str) -> String {
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

fn normalize_follow_up_kind(raw: Option<String>) -> String {
    let normalized = normalize_classifier_token(
        raw,
        &[
            "reply",
            "needs_reply",
            "owner_owes",
            "other_owes",
            "waiting_on",
            "check_back",
            "schedule",
            "none",
        ],
        "none",
    );
    if normalized == "reply" {
        "needs_reply".to_string()
    } else {
        normalized
    }
}

fn object_string(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn value_string_array(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Array(values) => Some(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        serde_json::Value::String(value) => Some(vec![value.clone()]),
        _ => None,
    }
}

fn object_string_array(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Vec<String>> {
    value_string_array(map.get(key)?)
}

fn clean_key_details(raw: Option<Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    for detail in raw.unwrap_or_default() {
        let Some(cleaned) = sanitize_follow_up_key_detail(&detail, 120) else {
            continue;
        };
        if out
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&cleaned))
        {
            continue;
        }
        out.push(cleaned);
        if out.len() >= 6 {
            break;
        }
    }
    out
}

fn normalized_proposed_action(out: &ClassifierOutput) -> Option<serde_json::Value> {
    let mut map = match out.proposed_action.clone() {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("raw".to_string(), other);
            map
        },
        None => serde_json::Map::new(),
    };
    let follow_up_kind = out
        .follow_up_kind
        .clone()
        .or_else(|| out.action_kind.clone())
        .or_else(|| object_string(&map, "follow_up_kind"))
        .map(|kind| normalize_follow_up_kind(Some(kind)));
    if let Some(kind) = follow_up_kind.filter(|kind| kind != "none") {
        map.insert(
            "follow_up_kind".to_string(),
            serde_json::Value::String(kind),
        );
    } else {
        map.remove("follow_up_kind");
    }
    if let Some(owner) = clean_classifier_text(
        out.action_owner
            .clone()
            .or_else(|| object_string(&map, "action_owner")),
        40,
    ) {
        let owner = normalize_classifier_token(
            Some(owner),
            &["owner", "counterparty", "agent", "unknown"],
            "unknown",
        );
        map.insert("action_owner".to_string(), serde_json::Value::String(owner));
    }
    if let Some(due_text) = clean_classifier_text(
        out.due_text
            .clone()
            .or_else(|| object_string(&map, "due_text")),
        80,
    ) {
        map.insert("due_text".to_string(), serde_json::Value::String(due_text));
    }
    if let Some(urgency) = out
        .urgency
        .clone()
        .or_else(|| object_string(&map, "urgency"))
    {
        let urgency =
            normalize_classifier_token(Some(urgency), &["low", "normal", "high"], "normal");
        map.insert("urgency".to_string(), serde_json::Value::String(urgency));
    }
    if let Some(needs_reply) = out.needs_reply {
        map.entry("needs_reply".to_string())
            .or_insert(serde_json::Value::Bool(needs_reply));
    }
    let key_details = clean_key_details(
        out.key_details
            .as_ref()
            .and_then(value_string_array)
            .or_else(|| object_string_array(&map, "key_details")),
    );
    if key_details.is_empty() {
        map.remove("key_details");
    } else {
        map.insert(
            "key_details".to_string(),
            serde_json::Value::Array(
                key_details
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(map))
    }
}

/// Parse + VALIDATE the model output. An off-vocabulary label is coerced to
/// `fyi` at low confidence (never trusted); missing confidence defaults to a
/// conservative 0.5. `pub` since plan 3.1 so the assist seam's policy-table
/// tests can pin the normalization; production callers remain in-module.
pub fn parse_classification(raw: &str) -> Result<Classification> {
    let cleaned = strip_code_fence(raw);
    let candidate = first_json_object(cleaned).unwrap_or(cleaned);
    let out: ClassifierOutput = serde_json::from_str(candidate)
        .with_context(|| format!("classifier output was not JSON: {:.120}", cleaned))?;
    let label = out.label.trim().to_ascii_lowercase();
    let (label, confidence) = if LABELS.contains(&label.as_str()) {
        (label, out.confidence.unwrap_or(0.5))
    } else {
        // Unknown label → treat as fyi, and don't trust the confidence.
        ("fyi".to_string(), out.confidence.unwrap_or(0.5).min(0.4))
    };
    let proposed_action = normalized_proposed_action(&out);
    Ok(Classification {
        label,
        confidence: confidence.clamp(0.0, 1.0),
        reason: out
            .reason
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty()),
        proposed_action,
        repeat_of_recently_handled: out.repeat_of_recently_handled.unwrap_or(false),
        semantic_features: ChannelAttentionSemanticEnvelope::from_optional_value(
            out.semantic_features.as_ref(),
            0,
            prompt_versions::CHANNEL_CLASSIFY,
            &SemanticExtractorIdentity::default(),
        ),
    })
}

/// The annotation state a classification lands in: actionable + confident →
/// `needs_approval` (a Today card); everything else → `classified` (quiet).
#[cfg(any(test, feature = "test-fixtures"))]
pub fn state_for(classification: &Classification) -> MailAnnotationState {
    if is_actionable(&classification.label) && classification.confidence >= CLASSIFY_HIGH_CONFIDENCE
    {
        MailAnnotationState::NeedsApproval
    } else {
        MailAnnotationState::Classified
    }
}

#[derive(Debug, Clone)]
struct ClassificationRoute {
    candidate: AttentionCandidate,
    outcome: RouteOutcome,
}

fn proposed_action_string(action: Option<&serde_json::Value>, key: &str) -> Option<String> {
    action
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn attention_urgency(classification: &Classification) -> AttentionUrgency {
    match proposed_action_string(classification.proposed_action.as_ref(), "urgency").as_deref() {
        Some("high") => AttentionUrgency::High,
        Some("low") => AttentionUrgency::Low,
        _ => AttentionUrgency::Normal,
    }
}

fn required_action(row: &ThreadClassifyRow) -> Option<ChannelRequiredAction> {
    derive_channel_required_action(
        row.latest_intent.as_deref(),
        row.needs_reply_hint,
        row.follow_up_hint.as_ref(),
        row.distill_brief.as_ref(),
    )
}

fn required_attention_action_kind(action: ChannelRequiredActionKind) -> AttentionActionKind {
    match action {
        ChannelRequiredActionKind::Reply => AttentionActionKind::Reply,
        ChannelRequiredActionKind::FollowUp => AttentionActionKind::FollowUp,
        ChannelRequiredActionKind::Schedule => AttentionActionKind::Schedule,
    }
}

fn effective_label<'a>(row: &ThreadClassifyRow, classification: &'a Classification) -> &'a str {
    match required_action(row).map(|action| action.kind) {
        Some(ChannelRequiredActionKind::Reply) => "needs_reply",
        Some(ChannelRequiredActionKind::FollowUp | ChannelRequiredActionKind::Schedule) => {
            "follow_up"
        },
        None => &classification.label,
    }
}

fn normalize_classification_for_row(row: &ThreadClassifyRow, classification: &mut Classification) {
    let deterministic_action = required_action(row);

    // A non-actionable update with no evidence-side required action closes the
    // prior lifecycle. Do not let stale model repeat metadata survive into the
    // annotation and make a fulfilled item look like another reminder.
    if deterministic_action.is_none() && !is_actionable(&classification.label) {
        classification.repeat_of_recently_handled = false;
    }

    let outbound = row
        .latest_direction
        .as_deref()
        .is_some_and(|direction| direction.eq_ignore_ascii_case("outbound"));
    let declared_actor = row
        .follow_up_hint
        .as_ref()
        .and_then(|hint| hint.actor.as_deref())
        .map(str::to_string)
        .or_else(|| proposed_action_string(classification.proposed_action.as_ref(), "action_owner"))
        .map(|actor| actor.trim().to_ascii_lowercase());
    let model_kind =
        proposed_action_string(classification.proposed_action.as_ref(), "follow_up_kind");
    let distilled_kind = row
        .follow_up_hint
        .as_ref()
        .map(|hint| hint.kind.trim().to_ascii_lowercase());

    // Outbound scheduling-shaped commitments are work the local owner/agent
    // owes, not a generic scheduling category. Keep scheduling as the
    // deterministic action kind used by the UI, but normalize the lifecycle
    // taxonomy used for reconciliation and observability.
    if outbound
        && declared_actor.as_deref() != Some("counterparty")
        && (model_kind.as_deref() == Some("schedule")
            || distilled_kind.as_deref() == Some("schedule"))
    {
        let local_actor = match declared_actor.as_deref() {
            Some("agent") => "agent",
            _ if row.lane.eq_ignore_ascii_case("envoy") => "agent",
            _ => "owner",
        };
        let action = classification
            .proposed_action
            .get_or_insert_with(|| serde_json::json!({}));
        if !action.is_object() {
            *action = serde_json::json!({});
        }
        if let Some(map) = action.as_object_mut() {
            map.insert(
                "follow_up_kind".to_string(),
                serde_json::Value::String("owner_owes".to_string()),
            );
            map.insert(
                "action_owner".to_string(),
                serde_json::Value::String(local_actor.to_string()),
            );
        }
    }
}

fn should_apply_recent_handled_cooldown(
    row: &ThreadClassifyRow,
    classification: &Classification,
    has_recent_handled: bool,
) -> bool {
    has_recent_handled
        && classification.repeat_of_recently_handled
        && is_actionable(effective_label(row, classification))
        && classification.confidence >= CLASSIFY_HIGH_CONFIDENCE
}

fn attention_action_kind(
    row: &ThreadClassifyRow,
    classification: &Classification,
) -> Option<AttentionActionKind> {
    if let Some(action) = required_action(row) {
        return Some(required_attention_action_kind(action.kind));
    }
    if classification.label == "needs_reply" {
        return Some(AttentionActionKind::Reply);
    }
    match proposed_action_string(classification.proposed_action.as_ref(), "follow_up_kind")
        .as_deref()
    {
        Some("needs_reply") => Some(AttentionActionKind::Reply),
        Some("schedule") => Some(AttentionActionKind::Schedule),
        Some("owner_owes" | "other_owes" | "waiting_on" | "check_back") => {
            Some(AttentionActionKind::FollowUp)
        },
        _ if classification.label == "follow_up" => Some(AttentionActionKind::FollowUp),
        _ => None,
    }
}

fn attention_candidate_from(
    row: &ThreadClassifyRow,
    classification: &Classification,
) -> AttentionCandidate {
    let deterministic_action = required_action(row);
    let action_kind = attention_action_kind(row, classification);
    let source_family = match deterministic_action.map(|action| action.kind) {
        Some(ChannelRequiredActionKind::FollowUp | ChannelRequiredActionKind::Schedule) => {
            AttentionSourceFamily::Promise
        },
        _ => AttentionSourceFamily::from_follow_up_signals(
            Some(effective_label(row, classification)),
            classification.proposed_action.as_ref(),
        ),
    };
    let title = row
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| channel_label(&row.provider))
        .to_string();
    AttentionCandidate {
        candidate_key: channel_candidate_key(row),
        source: channel_source(row),
        source_family,
        evidence_refs: row
            .evidence_message_ids
            .iter()
            .filter(|message_id| !message_id.trim().is_empty())
            .map(|message_id| format!("message:{message_id}"))
            .collect(),
        title,
        summary: row.latest_summary.clone().unwrap_or_default(),
        action: action_kind.map(|kind| AttentionAction {
            kind,
            label: kind.as_str().to_string(),
            payload: classification
                .proposed_action
                .clone()
                .unwrap_or(serde_json::Value::Null),
        }),
        urgency: attention_urgency(classification),
        confidence: Some(classification.confidence as f32),
        metadata: serde_json::json!({
            "label": effective_label(row, classification),
            "model_label": classification.label,
            "required_action": deterministic_action.map(|action| action.kind.as_str()),
            "required_action_source": deterministic_action.map(|action| action.source.as_str()),
            "routing_mismatch": deterministic_action.is_some()
                && !is_actionable(&classification.label),
            "distill_revision": row.distill_revision,
            "latest_intent": row.latest_intent,
            "latest_direction": row.latest_direction,
            "needs_reply_hint": row.needs_reply_hint,
            "message_count": row.message_count,
        }),
    }
}

fn route_context_for_classification(
    row: &ThreadClassifyRow,
    classification: &Classification,
    repeat_of_recently_handled: bool,
) -> AttentionRouteContext {
    let has_required_action = required_action(row).is_some();
    let inconsistent_action_hint = !has_required_action
        && !is_actionable(&classification.label)
        && proposed_action_string(classification.proposed_action.as_ref(), "follow_up_kind")
            .is_some();
    AttentionRouteContext {
        missing_safe_summary: row
            .latest_summary
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty(),
        weak_signal: !has_required_action
            && (classification.confidence < CLASSIFY_HIGH_CONFIDENCE
                || classification.label == "no_action"
                || inconsistent_action_hint),
        cooldown_active: repeat_of_recently_handled,
        ..Default::default()
    }
}

fn route_classification(
    row: &ThreadClassifyRow,
    classification: &Classification,
    repeat_of_recently_handled: bool,
) -> ClassificationRoute {
    let candidate = attention_candidate_from(row, classification);
    let context = route_context_for_classification(row, classification, repeat_of_recently_handled);
    let outcome = route_attention_candidate(&candidate, &context);
    ClassificationRoute { candidate, outcome }
}

fn route_annotation_state(route: &ClassificationRoute) -> MailAnnotationState {
    match &route.outcome {
        RouteOutcome::Routed {
            lane: AttentionLane::FollowUp,
            ..
        } => MailAnnotationState::NeedsApproval,
        _ => MailAnnotationState::Classified,
    }
}

fn proposed_action_with_route_metadata(
    row: &ThreadClassifyRow,
    classification: &Classification,
    route: &ClassificationRoute,
) -> Option<serde_json::Value> {
    let mut map = match classification.proposed_action.clone() {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("raw".to_string(), other);
            map
        },
        None => serde_json::Map::new(),
    };
    map.insert(
        "attention_source_family".to_string(),
        serde_json::Value::String(route.candidate.source_family.as_str().to_string()),
    );
    map.insert(
        "classification_input_revision".to_string(),
        serde_json::Value::from(row.distill_revision),
    );
    map.insert(
        "distill_revision".to_string(),
        serde_json::Value::from(row.distill_revision),
    );
    if let Some(required_action) = required_action(row) {
        map.insert(
            "required_action".to_string(),
            serde_json::Value::String(required_action.kind.as_str().to_string()),
        );
        map.insert(
            "required_action_source".to_string(),
            serde_json::Value::String(required_action.source.as_str().to_string()),
        );
        map.insert(
            "routing_mismatch".to_string(),
            serde_json::Value::Bool(!is_actionable(&classification.label)),
        );
    }
    if classification.repeat_of_recently_handled {
        map.insert(
            "repeat_of_recently_handled".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    match &route.outcome {
        RouteOutcome::Routed {
            lane,
            reason,
            priority,
        } => {
            map.insert(
                "attention_lane".to_string(),
                serde_json::Value::String(lane.as_str().to_string()),
            );
            map.insert(
                "attention_route_reason".to_string(),
                serde_json::Value::String(reason.as_str().to_string()),
            );
            map.insert(
                "attention_priority".to_string(),
                serde_json::Value::String(priority.as_str().to_string()),
            );
        },
        RouteOutcome::Dropped { reason } => {
            map.insert(
                "attention_drop_reason".to_string(),
                serde_json::Value::String(reason.as_str().to_string()),
            );
        },
        RouteOutcome::Traced { status } => {
            map.insert(
                "attention_trace_status".to_string(),
                serde_json::Value::String(status.as_str().to_string()),
            );
        },
    }
    Some(serde_json::Value::Object(map))
}

fn annotation_from(
    row: &ThreadClassifyRow,
    classification: &Classification,
    route: &ClassificationRoute,
    run_id: &str,
    now: i64,
) -> MailThreadAnnotation {
    let mut evidence_refs = vec![format!("thread:{}", row.thread_id)];
    let mut message_ids = row.evidence_message_ids.clone();
    if !message_ids.iter().any(|id| id == &row.latest_message_id) {
        message_ids.insert(0, row.latest_message_id.clone());
    }
    for message_id in message_ids {
        if !message_id.trim().is_empty() {
            evidence_refs.push(format!("message:{message_id}"));
        }
    }
    MailThreadAnnotation {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        provider: row.provider.clone(),
        account_alias: row.account_alias.clone(),
        thread_id: row.thread_id.clone(),
        // create_annotation inherits the real lane from the thread row.
        lane: ChannelLane::default(),
        state: route_annotation_state(route),
        label: Some(effective_label(row, classification).to_string()),
        confidence: Some(classification.confidence),
        reason: classification.reason.clone(),
        evidence_refs,
        evidence_message_id: Some(row.latest_message_id.clone()),
        evidence_message_at: Some(row.latest_message_at),
        classification_input_revision: Some(row.distill_revision),
        semantic_features: serialize_semantic_envelope(&classification.semantic_features).ok(),
        proposed_action: proposed_action_with_route_metadata(row, classification, route),
        provenance: Some(format!("{CHANNEL_CLASSIFY_OPERATION}:{run_id}")),
        created_at: now,
        updated_at: now,
    }
}

// ---------------------------------------------------------------------------
// Classify one thread (shared by the worker pass + the eval CLI)
// ---------------------------------------------------------------------------

async fn classify_system_prompt() -> Result<String> {
    managed_prompt(
        prompt_names::CHANNEL_CLASSIFY_USER,
        prompt_versions::CHANNEL_CLASSIFY,
    )
    .await
    .context("loading managed channel classification user prompt")?;
    rendered_prompt(
        prompt_names::CHANNEL_CLASSIFY_SYSTEM,
        prompt_versions::CHANNEL_CLASSIFY,
        HashMap::new(),
    )
    .await
    .context("rendering managed channel classification system prompt")
}

async fn classify_with_system(
    llm: &dyn ClassifyLlm,
    system: &str,
    row: &ThreadClassifyRow,
    recent_handled: &[RecentHandledFollowUp],
) -> Result<Classification> {
    let user = rendered_prompt(
        prompt_names::CHANNEL_CLASSIFY_USER,
        prompt_versions::CHANNEL_CLASSIFY,
        user_vars(row, now_millis(), recent_handled),
    )
    .await
    .context("rendering managed channel classification user prompt")?;
    let raw = llm.complete(system, &user).await?;
    let mut classification = parse_classification(&raw)?;
    classification.semantic_features.input_revision = row.distill_revision;
    let identity = llm.semantic_extractor_identity();
    classification.semantic_features.model = identity.model;
    classification.semantic_features.profile = identity.profile;
    normalize_classification_for_row(row, &mut classification);
    Ok(classification)
}

/// Classify one thread end-to-end — the unit the eval CLI drives per fixture
/// row (fetches the system prompt each call; the worker fetches it once and
/// uses the internal path).
pub async fn classify_row(
    llm: &dyn ClassifyLlm,
    row: &ThreadClassifyRow,
) -> Result<Classification> {
    let system = classify_system_prompt().await?;
    classify_with_system(llm, &system, row, &[]).await
}

// ---------------------------------------------------------------------------
// Pass
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClassifyPassOutcome {
    /// `channel_classify` unbound — nothing dispatched, queue preserved.
    pub idle: bool,
    pub drained: usize,
    pub classified: usize,
    pub needs_approval: usize,
    pub reclassified: usize,
    pub refreshed: usize,
    pub preserved: usize,
    pub stale_inputs: usize,
    pub failed: usize,
    pub concurrency: usize,
}

fn classification_disposition_key(
    disposition: ClassificationAnnotationDisposition,
) -> &'static str {
    match disposition {
        ClassificationAnnotationDisposition::Created => "created",
        ClassificationAnnotationDisposition::Reclassified => "reclassified",
        ClassificationAnnotationDisposition::RefreshedNeedsApproval => "refreshed_needs_approval",
        ClassificationAnnotationDisposition::PreservedLifecycle => "preserved_lifecycle",
        ClassificationAnnotationDisposition::PreservedDismissal => "preserved_dismissal",
    }
}

/// Drain up to `batch` classifiable threads for one scope. Bound check first:
/// unbound ⇒ idle (no dispatch). A per-thread LLM/parse failure is logged and
/// skipped (the thread stays un-annotated until its durable retry cap/backoff
/// allows another pass); it never blocks the batch.
/// One span per pass, not one per classified thread: the pass is what an
/// operator reads ("the mail worker ran and was idle"), and a per-thread span
/// would put a row on the wire for every message in the batch.
///
/// `comms_assist` is the whole reason this span exists. The worker calls the
/// operation router DIRECTLY — the module header on `telemetry.rs` says so in
/// as many words, because it is not an agent run — so its `llm_dispatch` spans
/// have no declaring ancestor and every one of them lands in the Undeclared
/// lane. It is also the class `workload_for_operation` already stamps on any
/// operation whose name contains `mail`, so the live row and the stored
/// dispatch row join.
///
/// `skip_all`: the store, the LLM handle and the attention store are not
/// identifiers, and span fields reach a browser unredacted.
#[instrument(
    name = "mail_classify_pass",
    skip_all,
    fields(
        activity_kind = KIND_BACKGROUND,
        workload_class = WORKLOAD_COMMS_ASSIST,
        principal = %principal,
        workspace = %workspace,
    )
)]
pub async fn run_classify_pass_with_concurrency_and_attention(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    llm: &dyn ClassifyLlm,
    batch: usize,
    concurrency: usize,
    attention_store: Option<&AttentionFunnelStore>,
) -> Result<ClassifyPassOutcome> {
    let concurrency = concurrency.max(1);
    if !llm.bound() {
        return Ok(ClassifyPassOutcome {
            idle: true,
            concurrency,
            ..Default::default()
        });
    }
    let system = classify_system_prompt().await?;
    let rows = store
        .list_threads_to_classify(principal, workspace, batch)
        .await?;
    let mut outcome = ClassifyPassOutcome {
        concurrency,
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(outcome);
    }
    let run_id = Uuid::new_v4().to_string();
    let results = stream::iter(rows)
        .map(|row| {
            classify_one_thread(
                store,
                principal,
                workspace,
                llm,
                &system,
                &run_id,
                row,
                attention_store,
            )
        })
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;
    for next in results {
        outcome.drained += next.drained;
        outcome.classified += next.classified;
        outcome.needs_approval += next.needs_approval;
        outcome.reclassified += next.reclassified;
        outcome.refreshed += next.refreshed;
        outcome.preserved += next.preserved;
        outcome.stale_inputs += next.stale_inputs;
        outcome.failed += next.failed;
    }
    Ok(outcome)
}

async fn classify_one_thread(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    llm: &dyn ClassifyLlm,
    system: &str,
    run_id: &str,
    row: ThreadClassifyRow,
    attention_store: Option<&AttentionFunnelStore>,
) -> ClassifyPassOutcome {
    let mut outcome = ClassifyPassOutcome {
        drained: 1,
        ..Default::default()
    };
    let now = now_millis();
    record_channel_pre_classify_trace_event(
        attention_store,
        principal,
        workspace,
        &row,
        AttentionFunnelStage::Distilled,
        AttentionTraceStatus::Succeeded,
        run_id,
        now,
        serde_json::json!({
            "trace": "distilled_thread_ready",
            "has_latest_summary": row.latest_summary.as_deref().map(str::trim).is_some_and(|value| !value.is_empty()),
        }),
    )
    .await;
    let recent_handled = match store
        .recent_handled_follow_ups(
            principal,
            workspace,
            &row.provider,
            &row.account_alias,
            &row.thread_id,
            now.saturating_sub(RECENT_HANDLED_FOLLOW_UP_SUPPRESS_MS),
            3,
        )
        .await
    {
        Ok(items) => items,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                provider = row.provider.as_str(),
                account = row.account_alias.as_str(),
                thread = row.thread_id.as_str(),
                %error,
                "failed to load recent handled follow-up context; classifying without cooldown context"
            );
            Vec::new()
        },
    };
    let classification = match classify_with_system(llm, system, &row, &recent_handled).await {
        Ok(c) => c,
        Err(error) => {
            record_channel_pre_classify_trace_event(
                attention_store,
                principal,
                workspace,
                &row,
                AttentionFunnelStage::Extracted,
                AttentionTraceStatus::Failed,
                run_id,
                now,
                serde_json::json!({
                    "trace": "classification_failed",
                    "error": error.to_string(),
                }),
            )
            .await;
            let retry_state =
                record_classify_failure_logged(store, principal, workspace, &row, now, &error)
                    .await;
            warn!(
                target: LOG_TARGET,
                provider = row.provider.as_str(),
                account = row.account_alias.as_str(),
                thread = row.thread_id.as_str(),
                attempts = retry_state.map(|(attempts, _)| attempts).unwrap_or(0),
                terminal = retry_state.map(|(_, terminal)| terminal).unwrap_or(false),
                %error,
                "classify failed for thread; recorded retry state"
            );
            outcome.failed += 1;
            return outcome;
        },
    };
    let repeat_of_recently_handled =
        should_apply_recent_handled_cooldown(&row, &classification, !recent_handled.is_empty());
    let feedback_tuning = match store
        .feedback_tuning_profile(
            principal,
            workspace,
            &row.provider,
            &row.account_alias,
            row.from_address.as_deref(),
            now.saturating_sub(FEEDBACK_TUNING_WINDOW_MS),
            FEEDBACK_TUNING_SAMPLE_LIMIT,
        )
        .await
    {
        Ok(profile) => profile,
        Err(error) => {
            warn!(
                target: LOG_TARGET,
                provider = row.provider.as_str(),
                account = row.account_alias.as_str(),
                thread = row.thread_id.as_str(),
                %error,
                "failed to load feedback tuning profile; classifying without sender/domain cooldown"
            );
            Default::default()
        },
    };
    let feedback_cooldown = feedback_tuning.reduces_noise()
        && required_action(&row).is_none()
        && is_actionable(&classification.label)
        && classification.confidence >= CLASSIFY_HIGH_CONFIDENCE;
    let route = route_classification(
        &row,
        &classification,
        repeat_of_recently_handled || feedback_cooldown,
    );
    let mut annotation = annotation_from(&row, &classification, &route, run_id, now);
    if feedback_cooldown {
        let action = annotation
            .proposed_action
            .get_or_insert_with(|| serde_json::json!({}));
        if let Some(map) = action.as_object_mut() {
            map.insert(
                "feedback_tuning_suppressed".to_string(),
                serde_json::Value::Bool(true),
            );
            map.insert(
                "feedback_tuning_scope".to_string(),
                serde_json::Value::String(
                    feedback_tuning
                        .dominant_scope()
                        .unwrap_or("sender")
                        .to_string(),
                ),
            );
            map.insert(
                "feedback_tuning_profile".to_string(),
                serde_json::to_value(&feedback_tuning).unwrap_or(serde_json::Value::Null),
            );
            map.insert(
                "feedback_tuning_explanation".to_string(),
                serde_json::Value::String(
                    "Repeated owner dismissals reduced this model-only recommendation.".to_string(),
                ),
            );
        }
    }
    match store
        .apply_classification_annotation(principal, workspace, annotation)
        .await
    {
        Ok(ClassificationAnnotationApplyResult::Applied {
            annotation: stored,
            disposition,
        }) => {
            record_attention_funnel_events(
                attention_store,
                principal,
                workspace,
                &row,
                &classification,
                &route,
                &stored,
                disposition,
                run_id,
                now,
            )
            .await;
            outcome.classified += 1;
            if stored.state == MailAnnotationState::NeedsApproval {
                outcome.needs_approval += 1;
            }
            match disposition {
                ClassificationAnnotationDisposition::Created => {},
                ClassificationAnnotationDisposition::Reclassified => outcome.reclassified += 1,
                ClassificationAnnotationDisposition::RefreshedNeedsApproval => {
                    outcome.refreshed += 1
                },
                ClassificationAnnotationDisposition::PreservedLifecycle
                | ClassificationAnnotationDisposition::PreservedDismissal => outcome.preserved += 1,
            }
        },
        Ok(ClassificationAnnotationApplyResult::StaleInput { current_revision }) => {
            record_channel_pre_classify_trace_event(
                attention_store,
                principal,
                workspace,
                &row,
                AttentionFunnelStage::Filtered,
                AttentionTraceStatus::Skipped,
                run_id,
                now,
                serde_json::json!({
                    "trace": "stale_classification_input_rejected",
                    "classification_input_revision": row.distill_revision,
                    "current_distill_revision": current_revision,
                }),
            )
            .await;
            outcome.stale_inputs += 1;
        },
        Err(error) => {
            record_attention_persistence_failure_events(
                attention_store,
                principal,
                workspace,
                &row,
                &classification,
                &route,
                run_id,
                now,
                &error,
            )
            .await;
            let retry_state =
                record_classify_failure_logged(store, principal, workspace, &row, now, &error)
                    .await;
            warn!(target: LOG_TARGET, thread = row.thread_id.as_str(), %error, "persisting classification failed");
            if let Some((attempts, terminal)) = retry_state {
                warn!(
                    target: LOG_TARGET,
                    thread = row.thread_id.as_str(),
                    attempts,
                    terminal,
                    "classification persistence failure recorded retry state"
                );
            }
            outcome.failed += 1;
        },
    }
    outcome
}

async fn record_attention_funnel_events(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    classification: &Classification,
    route: &ClassificationRoute,
    annotation: &MailThreadAnnotation,
    disposition: ClassificationAnnotationDisposition,
    run_id: &str,
    now: i64,
) {
    let Some(attention_store) = attention_store else {
        return;
    };
    let effective_route = ClassificationRoute {
        candidate: route.candidate.clone(),
        outcome: match disposition {
            ClassificationAnnotationDisposition::PreservedDismissal => RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::OwnerDismissed,
            },
            ClassificationAnnotationDisposition::PreservedLifecycle => RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::ActionAlreadyHandled,
            },
            _ => route.outcome.clone(),
        },
    };
    let route = &effective_route;
    let stage = match &route.outcome {
        RouteOutcome::Routed { .. } => AttentionFunnelStage::Routed,
        RouteOutcome::Dropped { .. } => AttentionFunnelStage::Dropped,
        RouteOutcome::Traced { .. } => AttentionFunnelStage::Filtered,
    };
    let mut events = vec![
        channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            AttentionFunnelStage::Extracted,
            AttentionTraceStatus::Succeeded,
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({ "trace": "classification_extracted" }),
        ),
        channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            AttentionFunnelStage::Filtered,
            filtered_trace_status(&route.outcome),
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({
                "trace": "route_filter_evaluated",
                "route_outcome": route_outcome_key(&route.outcome),
            }),
        ),
        AttentionRouteEvent {
            event_id: channel_route_event_id(principal, workspace, row, route),
            scope: AttentionScope {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
            },
            source: route.candidate.source.clone(),
            source_family: route.candidate.source_family,
            candidate_key: route.candidate.candidate_key.clone(),
            stage,
            outcome: route.outcome.clone(),
            occurred_at: row.latest_message_at,
            created_at: now,
            confidence: Some(classification.confidence as f32),
            metadata: channel_event_metadata(
                row,
                &classification.label,
                run_id,
                serde_json::json!({
                    "annotation_id": annotation.id,
                    "annotation_state": annotation.state.as_db_str(),
                    "annotation_disposition": classification_disposition_key(disposition),
                    "trace": "terminal_route_decision",
                }),
            ),
        },
    ];
    if annotation.state == MailAnnotationState::NeedsApproval {
        events.push(channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            AttentionFunnelStage::Surfaced,
            AttentionTraceStatus::Succeeded,
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({
                "trace": "annotation_surfaced",
                "annotation_id": annotation.id,
                "annotation_state": annotation.state.as_db_str(),
            }),
        ));
    }
    append_channel_attention_events(attention_store, row, events).await;
}

async fn record_attention_persistence_failure_events(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    classification: &Classification,
    route: &ClassificationRoute,
    run_id: &str,
    now: i64,
    error: &anyhow::Error,
) {
    let Some(attention_store) = attention_store else {
        return;
    };
    let failed_stage = match &route.outcome {
        RouteOutcome::Routed { .. } => AttentionFunnelStage::Surfaced,
        RouteOutcome::Dropped { .. } | RouteOutcome::Traced { .. } => {
            AttentionFunnelStage::Filtered
        },
    };
    let events = vec![
        channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            AttentionFunnelStage::Extracted,
            AttentionTraceStatus::Succeeded,
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({ "trace": "classification_extracted" }),
        ),
        channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            AttentionFunnelStage::Filtered,
            filtered_trace_status(&route.outcome),
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({
                "trace": "route_filter_evaluated",
                "route_outcome": route_outcome_key(&route.outcome),
            }),
        ),
        channel_trace_event(
            principal,
            workspace,
            row,
            &route.candidate,
            failed_stage,
            AttentionTraceStatus::Failed,
            Some(classification.confidence as f32),
            run_id,
            now,
            serde_json::json!({
                "trace": "annotation_persist_failed",
                "route_outcome": route_outcome_key(&route.outcome),
                "error": error.to_string(),
            }),
        ),
    ];
    append_channel_attention_events(attention_store, row, events).await;
}

async fn record_channel_pre_classify_trace_event(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
    run_id: &str,
    now: i64,
    detail: serde_json::Value,
) {
    let Some(attention_store) = attention_store else {
        return;
    };
    let candidate = channel_trace_candidate_from_row(row);
    let event = channel_trace_event(
        principal, workspace, row, &candidate, stage, status, None, run_id, now, detail,
    );
    append_channel_attention_events(attention_store, row, vec![event]).await;
}

async fn append_channel_attention_events(
    attention_store: &AttentionFunnelStore,
    row: &ThreadClassifyRow,
    events: Vec<AttentionRouteEvent>,
) {
    for event in events {
        let event_id = event.event_id.clone();
        if let Err(error) = attention_store.append_event(event).await {
            warn!(
                target: LOG_TARGET,
                provider = row.provider.as_str(),
                account = row.account_alias.as_str(),
                thread = row.thread_id.as_str(),
                event_id = %event_id,
                %error,
                "failed to record attention funnel event for classification"
            );
        }
    }
}

fn channel_trace_candidate_from_row(row: &ThreadClassifyRow) -> AttentionCandidate {
    let title = row
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| channel_label(&row.provider))
        .to_string();
    AttentionCandidate {
        candidate_key: channel_candidate_key(row),
        source: channel_source(row),
        source_family: AttentionSourceFamily::CommsIngest,
        evidence_refs: row
            .evidence_message_ids
            .iter()
            .filter(|message_id| !message_id.trim().is_empty())
            .map(|message_id| format!("message:{message_id}"))
            .collect(),
        title,
        summary: row.latest_summary.clone().unwrap_or_default(),
        action: None,
        urgency: AttentionUrgency::Normal,
        confidence: None,
        metadata: serde_json::json!({
            "latest_intent": row.latest_intent,
            "latest_direction": row.latest_direction,
            "needs_reply_hint": row.needs_reply_hint,
            "message_count": row.message_count,
        }),
    }
}

fn channel_source(row: &ThreadClassifyRow) -> AttentionSource {
    AttentionSource {
        kind: AttentionSourceKind::Comm,
        source_ref: format!("{}:{}:{}", row.provider, row.account_alias, row.thread_id),
        provider: Some(row.provider.clone()),
        account_alias: Some(row.account_alias.clone()),
    }
}

fn channel_candidate_key(row: &ThreadClassifyRow) -> String {
    format!(
        "channel:{}:{}:{}:{}",
        row.provider, row.account_alias, row.thread_id, row.latest_message_id
    )
}

fn channel_trace_event(
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    candidate: &AttentionCandidate,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
    confidence: Option<f32>,
    run_id: &str,
    now: i64,
    detail: serde_json::Value,
) -> AttentionRouteEvent {
    AttentionRouteEvent {
        event_id: channel_trace_event_id(principal, workspace, row, stage, status),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: candidate.source.clone(),
        source_family: candidate.source_family,
        candidate_key: candidate.candidate_key.clone(),
        stage,
        outcome: RouteOutcome::Traced { status },
        occurred_at: row.latest_message_at,
        created_at: now,
        confidence,
        metadata: channel_event_metadata(
            row,
            candidate
                .metadata
                .get("model_label")
                .or_else(|| candidate.metadata.get("label"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
            run_id,
            detail,
        ),
    }
}

fn channel_event_metadata(
    row: &ThreadClassifyRow,
    label: &str,
    run_id: &str,
    detail: serde_json::Value,
) -> serde_json::Value {
    let required_action = required_action(row);
    let effective_label = match required_action.map(|action| action.kind) {
        Some(ChannelRequiredActionKind::Reply) => "needs_reply",
        Some(ChannelRequiredActionKind::FollowUp | ChannelRequiredActionKind::Schedule) => {
            "follow_up"
        },
        None => label,
    };
    serde_json::json!({
        "producer": "channel_classify",
        "run_id": run_id,
        "label": effective_label,
        "model_label": label,
        "routing_mismatch": required_action.is_some() && !is_actionable(label),
        "required_action": required_action.map(|action| action.kind.as_str()),
        "required_action_source": required_action.map(|action| action.source.as_str()),
        "classification_input_revision": row.distill_revision,
        "provider": row.provider,
        "account_alias": row.account_alias,
        "thread_id": row.thread_id,
        "message_id": row.latest_message_id,
        "detail": detail,
    })
}

fn filtered_trace_status(outcome: &RouteOutcome) -> AttentionTraceStatus {
    match outcome {
        RouteOutcome::Routed { .. } => AttentionTraceStatus::Succeeded,
        RouteOutcome::Dropped { .. } | RouteOutcome::Traced { .. } => AttentionTraceStatus::Skipped,
    }
}

fn channel_trace_event_id(
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
) -> String {
    let raw = format!(
        "channel_trace\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        row.provider,
        row.account_alias,
        row.thread_id,
        row.latest_message_id,
        row.distill_revision,
        stage.as_str(),
        status.as_str(),
    );
    format!("channel-trace:{}", blake3::hash(raw.as_bytes()).to_hex())
}

fn channel_route_event_id(
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    route: &ClassificationRoute,
) -> String {
    let raw = format!(
        "channel_route\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        row.provider,
        row.account_alias,
        row.thread_id,
        row.latest_message_id,
        row.distill_revision,
        route.candidate.source_family.as_str(),
        route_outcome_key(&route.outcome),
    );
    format!("channel-route:{}", blake3::hash(raw.as_bytes()).to_hex())
}

fn route_outcome_key(outcome: &RouteOutcome) -> String {
    match outcome {
        RouteOutcome::Routed {
            lane,
            reason,
            priority,
        } => format!(
            "routed:{}:{}:{}",
            lane.as_str(),
            reason.as_str(),
            priority.as_str()
        ),
        RouteOutcome::Dropped { reason } => format!("dropped:{}", reason.as_str()),
        RouteOutcome::Traced { status } => format!("traced:{}", status.as_str()),
    }
}

async fn record_classify_failure_logged(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    row: &ThreadClassifyRow,
    now: i64,
    error: &anyhow::Error,
) -> Option<(i64, bool)> {
    let error_text = error.to_string();
    match store
        .record_classify_failure(
            principal,
            workspace,
            &row.provider,
            &row.account_alias,
            &row.latest_message_id,
            now,
            &error_text,
        )
        .await
    {
        Ok(state) => Some(state),
        Err(persist_error) => {
            warn!(
                target: LOG_TARGET,
                provider = row.provider.as_str(),
                account = row.account_alias.as_str(),
                thread = row.thread_id.as_str(),
                message_id = row.latest_message_id.as_str(),
                error = %persist_error,
                "failed to record classify retry state"
            );
            None
        },
    }
}

// ---------------------------------------------------------------------------
// Eval scoring (pure) — the Phase-8 pull-forward precision gate
// ---------------------------------------------------------------------------

/// Per-label precision/recall over a labeled fixture set.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelScore {
    pub precision: f64,
    pub recall: f64,
    /// Gold occurrences of this label.
    pub support: usize,
    pub true_positive: usize,
    pub predicted: usize,
}

/// The classifier's precision/recall over `(gold, predicted)` pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalReport {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub per_label: std::collections::BTreeMap<String, LabelScore>,
}

/// Score `(gold, predicted)` label pairs → per-label precision/recall +
/// overall accuracy. Pure + deterministic; the CLI feeds it the classifier's
/// predictions over hand-labeled fixtures.
pub fn score(pairs: &[(String, String)]) -> EvalReport {
    use std::collections::{BTreeMap, BTreeSet};
    let mut tp: BTreeMap<String, usize> = BTreeMap::new();
    let mut predicted: BTreeMap<String, usize> = BTreeMap::new();
    let mut support: BTreeMap<String, usize> = BTreeMap::new();
    let mut correct = 0usize;
    for (gold, pred) in pairs {
        *support.entry(gold.clone()).or_default() += 1;
        *predicted.entry(pred.clone()).or_default() += 1;
        if gold == pred {
            correct += 1;
            *tp.entry(gold.clone()).or_default() += 1;
        }
    }
    let mut labels: BTreeSet<String> = BTreeSet::new();
    labels.extend(support.keys().cloned());
    labels.extend(predicted.keys().cloned());
    let mut per_label = BTreeMap::new();
    for label in labels {
        let t = *tp.get(&label).unwrap_or(&0);
        let p = *predicted.get(&label).unwrap_or(&0);
        let s = *support.get(&label).unwrap_or(&0);
        per_label.insert(
            label,
            LabelScore {
                precision: if p > 0 { t as f64 / p as f64 } else { 0.0 },
                recall: if s > 0 { t as f64 / s as f64 } else { 0.0 },
                support: s,
                true_positive: t,
                predicted: p,
            },
        );
    }
    let total = pairs.len();
    EvalReport {
        total,
        correct,
        accuracy: if total > 0 {
            correct as f64 / total as f64
        } else {
            0.0
        },
        per_label,
    }
}

fn default_eval_provider() -> String {
    "gmail".to_string()
}
fn default_eval_lane() -> String {
    "user_assist".to_string()
}

/// One hand-labeled eval fixture — the classifier's `ThreadContext` inputs +
/// the `gold_label`. `channel-assist classify-eval` reads a JSONL of these
/// (subject + a `summary` + light metadata; the same body-blind inputs the
/// live classifier sees). Everything but `gold_label`/`summary` is optional.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalFixture {
    pub gold_label: String,
    #[serde(default = "default_eval_provider")]
    pub provider: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default)]
    pub from_address: Option<String>,
    #[serde(default)]
    pub recipient_domains: Vec<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub last_message_at: Option<i64>,
    #[serde(default = "default_eval_lane")]
    pub lane: String,
    #[serde(default)]
    pub summary: Option<String>,
}

impl EvalFixture {
    /// Project onto the store row shape the classifier consumes (dummy
    /// account/thread ids — the eval never persists).
    pub fn to_row(&self) -> ThreadClassifyRow {
        ThreadClassifyRow {
            provider: self.provider.clone(),
            account_alias: "eval".to_string(),
            thread_id: "eval".to_string(),
            latest_message_id: "eval-message".to_string(),
            latest_message_at: self.last_message_at.unwrap_or(0),
            evidence_message_ids: vec!["eval-message".to_string()],
            subject: self.subject.clone(),
            from_name: self.from_name.clone(),
            from_address: self.from_address.clone(),
            recipient_domains: self.recipient_domains.clone(),
            label_ids: self.label_ids.clone(),
            message_count: self.message_count,
            last_message_at: self.last_message_at,
            lane: self.lane.clone(),
            latest_summary: self.summary.clone(),
            latest_direction: None,
            latest_intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_revision: 1,
        }
    }
}

impl EvalReport {
    /// Human-readable multi-line report for the CLI.
    pub fn render(&self) -> String {
        let mut out = format!(
            "classify-eval: {}/{} correct (accuracy {:.1}%)\n",
            self.correct,
            self.total,
            self.accuracy * 100.0
        );
        out.push_str("label          precision  recall  support\n");
        for (label, s) in &self.per_label {
            out.push_str(&format!(
                "{:<14} {:>8.1}% {:>6.1}% {:>8}\n",
                label,
                s.precision * 100.0,
                s.recall * 100.0,
                s.support
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChannelClassifyConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub startup_delay: Duration,
    pub batch: usize,
    pub concurrency: usize,
}

impl Default for ChannelClassifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
            batch: DEFAULT_BATCH,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

impl ChannelClassifyConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("CHANNEL_CLASSIFY_ENABLED") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_CLASSIFY_INTERVAL_SECS") {
            if secs > 0 {
                config.interval = Duration::from_secs(secs);
            }
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_CLASSIFY_STARTUP_DELAY_SECS") {
            config.startup_delay = Duration::from_secs(secs);
        }
        if let Some(batch) = env_parse::<usize>("CHANNEL_CLASSIFY_BATCH") {
            if batch > 0 {
                config.batch = batch;
            }
        }
        if let Some(concurrency) = env_parse::<usize>("CHANNEL_CLASSIFY_CONCURRENCY") {
            if concurrency > 0 {
                config.concurrency = concurrency;
            }
        }
        config
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// Background classifier worker handle (mirrors [`super::distill::ChannelDistillWorker`]).
#[derive(Debug)]
pub struct ChannelClassifyWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelClassifyWorker {
    pub fn spawn_with_attention(
        store: MailAssistStore,
        router: Option<Arc<OperationLlmRouter>>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        attention_store: Option<AttentionFunnelStore>,
        config: ChannelClassifyConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_periodic(
                store,
                router,
                broadcaster,
                attention_store,
                config,
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
    store: MailAssistStore,
    router: Option<Arc<OperationLlmRouter>>,
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    attention_store: Option<AttentionFunnelStore>,
    config: ChannelClassifyConfig,
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
    let mut idle_logged = false;
    loop {
        run_tick(
            &store,
            router.as_ref(),
            broadcaster.as_ref(),
            attention_store.as_ref(),
            &config,
            &mut idle_logged,
        )
        .await;
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {},
            _ = cancel.cancelled() => return,
        }
    }
}

async fn run_tick(
    store: &MailAssistStore,
    router: Option<&Arc<OperationLlmRouter>>,
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    attention_store: Option<&AttentionFunnelStore>,
    config: &ChannelClassifyConfig,
    idle_logged: &mut bool,
) {
    let Some(router) = router else {
        return;
    };
    // Classification is already submitted to the bounded background lane.
    // Always admit one bounded pass so sustained background activity cannot
    // keep pending threads outside the queue indefinitely.
    let llm = RouterClassifyLlm::new(
        Arc::clone(router),
        broadcaster.cloned(),
        DEFAULT_SCOPE_PRINCIPAL,
        DEFAULT_SCOPE_WORKSPACE,
    );
    match run_classify_pass_with_concurrency_and_attention(
        store,
        DEFAULT_SCOPE_PRINCIPAL,
        DEFAULT_SCOPE_WORKSPACE,
        &llm,
        config.batch,
        config.concurrency,
        attention_store,
    )
    .await
    {
        Ok(outcome) if outcome.idle => {
            // One info on the edge, not per tick.
            if !*idle_logged {
                info!(
                    target: LOG_TARGET,
                    "classification idle: '{CHANNEL_CLASSIFY_OPERATION}' unbound — threads stay \
                     pending; bind it to an LLM profile in config (local or remote) to enable"
                );
                *idle_logged = true;
            }
        },
        Ok(outcome) => {
            if *idle_logged {
                info!(target: LOG_TARGET, "classification available again ('{CHANNEL_CLASSIFY_OPERATION}' bound)");
                *idle_logged = false;
            }
            if outcome.drained > 0 {
                info!(
                    target: LOG_TARGET,
                    drained = outcome.drained,
                    classified = outcome.classified,
                    needs_approval = outcome.needs_approval,
                    reclassified = outcome.reclassified,
                    refreshed = outcome.refreshed,
                    preserved = outcome.preserved,
                    stale_inputs = outcome.stale_inputs,
                    failed = outcome.failed,
                    concurrency = outcome.concurrency,
                    "channel classify pass"
                );
            }
        },
        Err(error) => {
            debug!(target: LOG_TARGET, %error, "channel classify tick errored");
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn row() -> ThreadClassifyRow {
        ThreadClassifyRow {
            provider: "gmail".into(),
            account_alias: "business".into(),
            thread_id: "t1".into(),
            latest_message_id: "m1".into(),
            latest_message_at: 1_783_209_600_000,
            evidence_message_ids: vec!["m1".into()],
            subject: Some("Re: follow-up".into()),
            from_name: Some("Sender".into()),
            from_address: Some("sender@example.com".into()),
            recipient_domains: vec!["example.org".into()],
            label_ids: vec!["inbox".into()],
            message_count: 3,
            last_message_at: Some(1_783_209_600_000),
            lane: "user_assist".into(),
            latest_summary: Some("Sender asked for a response about the pending item.".into()),
            latest_direction: Some("inbound".into()),
            latest_intent: Some("action_request".into()),
            needs_reply_hint: true,
            follow_up_hint: Some(super::super::types::ChannelFollowUpHint {
                kind: "needs_reply".into(),
                actor: Some("owner".into()),
                counterparty: Some("Sender".into()),
                due_text: Some("soon".into()),
                urgency: Some("normal".into()),
                rationale: Some("Sender is waiting for the response.".into()),
                key_details: vec!["Due soon".into()],
            }),
            distill_brief: None,
            distill_revision: 1,
        }
    }

    fn informational_row() -> ThreadClassifyRow {
        let mut row = row();
        row.latest_intent = Some("fyi".to_string());
        row.needs_reply_hint = false;
        row.follow_up_hint = None;
        row
    }

    #[test]
    fn user_vars_render_body_blind_context() {
        let recent = vec![RecentHandledFollowUp {
            state: MailAnnotationState::Acknowledged,
            label: Some("needs_reply".to_string()),
            reason: Some("Sender was waiting".to_string()),
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "needs_reply",
                "due_text": "soon"
            })),
            updated_at: 1_783_209_600_000 + 86_400_000,
        }];
        let vars = user_vars(&row(), 1_783_209_600_000 + 2 * 86_400_000, &recent);
        assert_eq!(vars.get("channel").map(String::as_str), Some("email"));
        assert_eq!(vars.get("lane").map(String::as_str), Some("the owner's"));
        assert_eq!(
            vars.get("sender").map(String::as_str),
            Some("Sender <sender@example.com>")
        );
        assert_eq!(vars.get("age").map(String::as_str), Some("2 days ago"));
        assert_eq!(
            vars.get("latest_direction").map(String::as_str),
            Some("inbound")
        );
        assert_eq!(
            vars.get("latest_intent").map(String::as_str),
            Some("action_request")
        );
        assert_eq!(
            vars.get("needs_reply_hint").map(String::as_str),
            Some("true")
        );
        assert!(vars.get("follow_up_hint").unwrap().contains("needs_reply"));
        assert!(vars
            .get("recent_handled_followups")
            .unwrap()
            .contains("acknowledged"));
        assert!(vars.get("summary").unwrap().contains("pending item"));
    }

    #[test]
    fn parses_plain_and_fenced_json() {
        let a = parse_classification(r#"{"label":"needs_reply","confidence":0.9,"reason":"Sender awaits a response","follow_up_kind":"reply","action_owner":"owner","due_text":"soon","urgency":"high","key_details":["Amount: 1250","Card ending 1234"],"repeat_of_recently_handled":true,"proposed_action":{"kind":"respond"}}"#).unwrap();
        assert_eq!(a.label, "needs_reply");
        assert_eq!(a.confidence, 0.9);
        assert_eq!(a.reason.as_deref(), Some("Sender awaits a response"));
        assert!(a.repeat_of_recently_handled);
        let proposed = a.proposed_action.unwrap();
        assert_eq!(proposed["follow_up_kind"], "needs_reply");
        assert_eq!(proposed["due_text"], "soon");
        assert_eq!(proposed["key_details"][0], "Amount: 1250");
        assert_eq!(proposed["key_details"][1], "Card ending 1234");

        let fenced = "```json\n{\"label\":\"fyi\",\"confidence\":0.3}\n```";
        let b = parse_classification(fenced).unwrap();
        assert_eq!(b.label, "fyi");
        assert_eq!(b.confidence, 0.3);
    }

    #[test]
    fn parses_json_object_with_trailing_text() {
        let parsed = parse_classification(
            r#"{"label":"follow_up","confidence":0.8,"reason":"Owner should check back"} Extra explanation."#,
        )
        .unwrap();
        assert_eq!(parsed.label, "follow_up");
        assert_eq!(parsed.confidence, 0.8);
        assert_eq!(parsed.reason.as_deref(), Some("Owner should check back"));
    }

    #[test]
    fn off_vocabulary_label_coerced_to_fyi_low_confidence() {
        let c = parse_classification(r#"{"label":"URGENT_ESCALATE","confidence":0.99}"#).unwrap();
        assert_eq!(c.label, "fyi");
        assert!(c.confidence <= 0.4);
    }

    #[test]
    fn malformed_semantic_block_is_independent_from_legacy_classification() {
        let parsed = parse_classification(
            r#"{
                "label":"needs_reply",
                "confidence":0.91,
                "follow_up_kind":"needs_reply",
                "semantic_features":{
                    "communication_type":"direct_request",
                    "requested_action":"reply",
                    "action_owner":"owner",
                    "direct_request_probability":7.0,
                    "broadcast_probability":0.0,
                    "personal_obligation_probability":0.9,
                    "information_value_probability":0.2,
                    "deadline":{"kind":"none","value":null},
                    "evidence_refs":["latest_intent"]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.label, "needs_reply");
        assert_eq!(parsed.confidence, 0.91);
        assert_eq!(
            parsed.semantic_features.status,
            magician::magician_v2::attention::learning::SemanticExtractionStatus::Invalid
        );
        assert!(parsed.semantic_features.features.is_none());
    }

    #[test]
    fn safe_brief_is_added_to_the_existing_body_blind_call() {
        let mut row = informational_row();
        row.distill_brief = Some(crate::channel_assist::channel::ChannelInformationBrief {
            schema_version: 1,
            information_type: Default::default(),
            summary: "Safe local summary".to_string(),
            key_facts: vec!["Supported fact".to_string()],
            changes: Vec::new(),
            temporal_facts: Vec::new(),
            stated_action: None,
            detail_status: Default::default(),
            missing_details: Vec::new(),
        });
        let vars = user_vars(&row, 1, &[]);
        assert!(vars
            .get("safe_brief")
            .is_some_and(|value| value.contains("Safe local summary")));
    }

    #[test]
    fn confidence_clamped_and_missing_defaults() {
        assert_eq!(
            parse_classification(r#"{"label":"follow_up","confidence":2.5}"#)
                .unwrap()
                .confidence,
            1.0
        );
        assert_eq!(
            parse_classification(r#"{"label":"follow_up"}"#)
                .unwrap()
                .confidence,
            0.5
        );
    }

    #[test]
    fn state_needs_approval_only_for_actionable_and_confident() {
        let approve = Classification {
            label: "needs_reply".into(),
            confidence: 0.8,
            reason: None,
            proposed_action: None,
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        assert_eq!(state_for(&approve), MailAnnotationState::NeedsApproval);
        // Actionable but low confidence → quiet.
        let quiet = Classification {
            label: "needs_reply".into(),
            confidence: 0.5,
            reason: None,
            proposed_action: None,
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        assert_eq!(state_for(&quiet), MailAnnotationState::Classified);
        // Confident but not actionable → quiet.
        let fyi = Classification {
            label: "fyi".into(),
            confidence: 0.95,
            reason: None,
            proposed_action: None,
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        assert_eq!(state_for(&fyi), MailAnnotationState::Classified);
    }

    #[test]
    fn route_confident_actionable_classification_to_follow_up_annotation() {
        let classification = Classification {
            label: "needs_reply".into(),
            confidence: 0.9,
            reason: Some("Sender is waiting".into()),
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "needs_reply",
                "urgency": "high"
            })),
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        let row = informational_row();
        let route = route_classification(&row, &classification, false);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                ..
            }
        ));
        let annotation = annotation_from(&row, &classification, &route, "run-1", 1);
        assert_eq!(annotation.state, MailAnnotationState::NeedsApproval);
        let action = annotation.proposed_action.unwrap();
        assert_eq!(action["attention_source_family"], "comms_ingest");
        assert_eq!(action["attention_lane"], "follow_up");
        assert_eq!(action["attention_route_reason"], "actionable_communication");
    }

    #[test]
    fn route_low_confidence_actionable_classification_to_quiet_drop() {
        let classification = Classification {
            label: "needs_reply".into(),
            confidence: 0.45,
            reason: Some("Unclear".into()),
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "needs_reply"
            })),
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        let row = informational_row();
        let route = route_classification(&row, &classification, false);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::WeakSignal
            }
        ));
        let annotation = annotation_from(&row, &classification, &route, "run-1", 1);
        assert_eq!(annotation.state, MailAnnotationState::Classified);
        let action = annotation.proposed_action.unwrap();
        assert_eq!(action["attention_source_family"], "comms_ingest");
        assert_eq!(action["attention_drop_reason"], "weak_signal");
    }

    #[test]
    fn route_high_confidence_fyi_classification_to_quiet_drop() {
        let classification = Classification {
            label: "fyi".into(),
            confidence: 0.95,
            reason: Some("Informational update".into()),
            proposed_action: None,
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        let row = informational_row();
        let route = route_classification(&row, &classification, false);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::UnsupportedSource
            }
        ));
        let annotation = annotation_from(&row, &classification, &route, "run-1", 1);
        assert_eq!(annotation.state, MailAnnotationState::Classified);
        let action = annotation.proposed_action.unwrap();
        assert_eq!(action["attention_source_family"], "comms_ingest");
        assert_eq!(action["attention_drop_reason"], "unsupported_source");
        assert!(action.get("attention_lane").is_none());
    }

    #[test]
    fn deterministic_required_action_reroutes_model_fyi_mismatch() {
        let classification = Classification {
            label: "fyi".into(),
            confidence: 0.3,
            reason: Some("Model missed the required reply".into()),
            proposed_action: None,
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        let row = row();
        let route = route_classification(&row, &classification, false);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                ..
            }
        ));
        let annotation = annotation_from(&row, &classification, &route, "run-1", 1);
        assert_eq!(annotation.state, MailAnnotationState::NeedsApproval);
        assert_eq!(annotation.label.as_deref(), Some("needs_reply"));
        assert_eq!(annotation.classification_input_revision, Some(1));
        let action = annotation.proposed_action.unwrap();
        assert_eq!(action["routing_mismatch"], true);
        assert_eq!(action["required_action"], "reply");
    }

    #[test]
    fn route_recently_handled_actionable_classification_to_cooldown_drop() {
        let classification = Classification {
            label: "needs_reply".into(),
            confidence: 0.9,
            reason: Some("Sender is waiting".into()),
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "needs_reply",
                "urgency": "high"
            })),
            repeat_of_recently_handled: true,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };
        let route = route_classification(&row(), &classification, true);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::CooldownActive
            }
        ));
        let annotation = annotation_from(&row(), &classification, &route, "run-1", 1);
        assert_eq!(annotation.state, MailAnnotationState::Classified);
        let action = annotation.proposed_action.unwrap();
        assert_eq!(action["repeat_of_recently_handled"], true);
        assert_eq!(action["attention_drop_reason"], "cooldown_active");
    }

    #[test]
    fn fulfilled_non_actionable_update_clears_stale_repeat_metadata() {
        let row = informational_row();
        let mut classification = Classification {
            label: "no_action".into(),
            confidence: 0.98,
            reason: Some("The previously requested work is complete.".into()),
            proposed_action: None,
            repeat_of_recently_handled: true,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };

        normalize_classification_for_row(&row, &mut classification);

        assert!(!classification.repeat_of_recently_handled);
        let route = route_classification(&row, &classification, false);
        let annotation = annotation_from(&row, &classification, &route, "run-1", 1);
        assert!(annotation
            .proposed_action
            .as_ref()
            .and_then(|action| action.get("repeat_of_recently_handled"))
            .is_none());
    }

    #[test]
    fn deterministic_action_applies_repeat_cooldown_even_when_model_label_is_no_action() {
        let row = row();
        let mut classification = Classification {
            label: "no_action".into(),
            confidence: 0.95,
            reason: Some("This is unchanged from the handled item.".into()),
            proposed_action: None,
            repeat_of_recently_handled: true,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };

        normalize_classification_for_row(&row, &mut classification);
        assert!(classification.repeat_of_recently_handled);
        assert!(should_apply_recent_handled_cooldown(
            &row,
            &classification,
            true
        ));
        let route = route_classification(&row, &classification, true);
        assert!(matches!(
            route.outcome,
            RouteOutcome::Dropped {
                reason: magician::magician_v2::attention_funnel::DropReason::CooldownActive
            }
        ));
    }

    #[test]
    fn outbound_local_schedule_is_normalized_to_owner_owes() {
        let mut row = informational_row();
        row.latest_direction = Some("outbound".into());
        row.latest_intent = Some("follow_up".into());
        row.follow_up_hint = Some(super::super::types::ChannelFollowUpHint {
            kind: "schedule".into(),
            actor: Some("owner".into()),
            ..Default::default()
        });
        let mut classification = Classification {
            label: "follow_up".into(),
            confidence: 0.94,
            reason: Some("The owner committed to arrange the next step.".into()),
            proposed_action: Some(serde_json::json!({
                "follow_up_kind": "schedule",
                "action_owner": "owner"
            })),
            repeat_of_recently_handled: false,
            semantic_features: ChannelAttentionSemanticEnvelope::missing(
                1,
                prompt_versions::CHANNEL_CLASSIFY,
                &SemanticExtractorIdentity::default(),
            ),
        };

        normalize_classification_for_row(&row, &mut classification);

        let action = classification.proposed_action.as_ref().unwrap();
        assert_eq!(action["follow_up_kind"], "owner_owes");
        assert_eq!(action["action_owner"], "owner");
    }

    struct StubLlm {
        bound: bool,
        reply: String,
    }
    #[async_trait]
    impl ClassifyLlm for StubLlm {
        fn bound(&self) -> bool {
            self.bound
        }
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn score_computes_per_label_precision_recall() {
        let pairs = vec![
            ("needs_reply".to_string(), "needs_reply".to_string()), // TP
            ("needs_reply".to_string(), "fyi".to_string()),         // FN for needs_reply
            ("fyi".to_string(), "fyi".to_string()),                 // TP
            ("no_action".to_string(), "fyi".to_string()),           // FP for fyi
        ];
        let report = score(&pairs);
        assert_eq!(report.total, 4);
        assert_eq!(report.correct, 2);
        assert_eq!(report.accuracy, 0.5);
        // needs_reply: 1 TP, 1 predicted, 2 gold → precision 1.0, recall 0.5.
        let nr = &report.per_label["needs_reply"];
        assert_eq!(nr.precision, 1.0);
        assert_eq!(nr.recall, 0.5);
        // fyi: 1 TP, 3 predicted, 1 gold → precision 1/3, recall 1.0.
        let fyi = &report.per_label["fyi"];
        assert!((fyi.precision - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(fyi.recall, 1.0);
    }

    #[test]
    fn committed_classifier_fixtures_parse_and_cover_every_label() {
        let raw = include_str!("../../../../magician/tests/fixtures/channel_classifier_eval.jsonl");
        let fixtures = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<EvalFixture>)
            .collect::<Result<Vec<_>, _>>()
            .expect("valid classifier fixture JSONL");
        assert!(fixtures.len() >= 16);
        for label in LABELS {
            assert!(
                fixtures
                    .iter()
                    .filter(|fixture| fixture.gold_label == label)
                    .count()
                    >= 4,
                "expected at least four fixtures for {label}"
            );
        }
        assert!(fixtures.iter().all(|fixture| fixture
            .summary
            .as_deref()
            .is_some_and(|value| !value.is_empty())));
    }

    #[tokio::test]
    async fn unbound_pass_is_idle() {
        // bound()==false short-circuits before any store query — the temp
        // store is just needed to satisfy the signature.
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let llm = StubLlm {
            bound: false,
            reply: String::new(),
        };
        let outcome = run_classify_pass_with_concurrency_and_attention(
            &store,
            "anonymous",
            "default",
            &llm,
            8,
            DEFAULT_CONCURRENCY,
            None,
        )
        .await
        .unwrap();
        assert!(outcome.idle);
        assert_eq!(outcome.drained, 0);
    }
}
