//! Versioned semantic features and calibrated Slice-2 actionability inference.
//!
//! The LLM extraction is deliberately only a feature source. Routing remains
//! unchanged when extraction is missing or invalid, and serving accepts only
//! an immutable snapshot whose feature contract exactly matches this module.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const ATTENTION_SEMANTIC_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT: &str = "channel_attention_semantics_v1";
pub const ACTIONABILITY_FEATURE_CONTRACT: &str = "attention_actionability_features_v2";
/// Versioned representation of clock-derived serving features. Inference and
/// persisted training evidence both flow through `feature_values`, so the
/// vector attributed to an exposure is byte-identical to the one scored.
pub const ATTENTION_TEMPORAL_FEATURE_CONTRACT: &str = "attention_temporal_buckets_v1";

const EVIDENCE_ALLOWLIST: &[&str] = &[
    "subject",
    "sender",
    "recipient_domains",
    "label_ids",
    "message_count",
    "latest_message_age",
    "latest_direction",
    "latest_intent",
    "needs_reply_hint",
    "follow_up_hint",
    "safe_brief.information_type",
    "safe_brief.key_facts",
    "safe_brief.changes",
    "safe_brief.temporal_facts",
    "safe_brief.stated_action",
    "safe_brief.detail_status",
    "summary",
];

/// Provider-side shape constraints for the existing semantic feature contract.
/// This does not change the prompt/extractor identity or invalidate previously
/// validated features. The local validator remains authoritative.
pub fn semantic_features_response_schema() -> serde_json::Value {
    use serde_json::json;
    let probability = json!({"type": "number", "minimum": 0, "maximum": 1});
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["communication_type", "requested_action", "action_owner",
            "direct_request_probability", "broadcast_probability",
            "personal_obligation_probability", "information_value_probability",
            "deadline", "campaign_or_event_identity", "evidence_refs"],
        "properties": {
            "communication_type": {"type": "string", "enum": ["direct_request", "personal_update", "transaction", "newsletter", "promotion", "system_notice", "unknown"]},
            "requested_action": {"type": "string", "enum": ["reply", "decide", "pay", "schedule", "review", "attend", "none", "unknown"]},
            "action_owner": {"type": "string", "enum": ["owner", "sender", "third_party", "shared", "unknown"]},
            "direct_request_probability": probability,
            "broadcast_probability": probability,
            "personal_obligation_probability": probability,
            "information_value_probability": probability,
            "deadline": {"type": "object", "additionalProperties": false,
                "required": ["kind", "value"], "properties": {
                    "kind": {"type": "string", "enum": ["explicit", "implied", "none"]},
                    "value": {"type": ["string", "null"], "maxLength": 80}
                }},
            "campaign_or_event_identity": {"type": ["string", "null"], "maxLength": 120},
            "evidence_refs": {"type": "array", "minItems": 1, "maxItems": 12,
                "items": {"type": "string", "enum": EVIDENCE_ALLOWLIST}}
        }
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticExtractionStatus {
    Succeeded,
    Missing,
    Invalid,
}

impl SemanticExtractionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelAttentionSemanticEnvelope {
    pub schema_version: u32,
    pub extractor_contract: String,
    pub prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub input_revision: i64,
    /// Exact source-owned revision consumed by the extractor. Older Follow-up
    /// envelopes omit this and remain compatible through their historical
    /// `distill:<input_revision>` binding. Worth-a-look always sets it because
    /// its source revision is an opaque string, not a mail distill counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub status: SemanticExtractionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<ChannelAttentionSemanticFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason_code: Option<String>,
}

impl ChannelAttentionSemanticEnvelope {
    pub fn missing(
        input_revision: i64,
        prompt_version: impl Into<String>,
        identity: &SemanticExtractorIdentity,
    ) -> Self {
        Self {
            schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            prompt_version: prompt_version.into(),
            model: identity.model.clone(),
            profile: identity.profile.clone(),
            input_revision,
            source_revision: None,
            status: SemanticExtractionStatus::Missing,
            features: None,
            invalid_reason_code: None,
        }
    }

    pub fn from_optional_value(
        raw: Option<&serde_json::Value>,
        input_revision: i64,
        prompt_version: &str,
        identity: &SemanticExtractorIdentity,
    ) -> Self {
        let Some(raw) = raw else {
            return Self::missing(input_revision, prompt_version, identity);
        };
        match validate_semantic_features(raw) {
            Ok(features) => Self {
                schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
                extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
                prompt_version: prompt_version.to_string(),
                model: identity.model.clone(),
                profile: identity.profile.clone(),
                input_revision,
                source_revision: None,
                status: SemanticExtractionStatus::Succeeded,
                features: Some(features),
                invalid_reason_code: None,
            },
            Err(error) => Self {
                schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
                extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
                prompt_version: prompt_version.to_string(),
                model: identity.model.clone(),
                profile: identity.profile.clone(),
                input_revision,
                source_revision: None,
                status: SemanticExtractionStatus::Invalid,
                features: None,
                invalid_reason_code: Some(error.code().to_string()),
            },
        }
    }

    /// Build the same validated envelope while binding it to an opaque,
    /// source-owned revision. This is used by asynchronous extraction and does
    /// not change the schema or validation rules used by channel classification.
    pub fn from_optional_value_for_source(
        raw: Option<&serde_json::Value>,
        source_revision: impl Into<String>,
        input_revision: i64,
        prompt_version: &str,
        identity: &SemanticExtractorIdentity,
    ) -> Self {
        let mut envelope = Self::from_optional_value(raw, input_revision, prompt_version, identity);
        envelope.source_revision = Some(source_revision.into());
        envelope
    }

    pub fn is_compatible(&self, source_revision: Option<&str>) -> bool {
        self.status == SemanticExtractionStatus::Succeeded
            && self.schema_version == ATTENTION_SEMANTIC_SCHEMA_VERSION
            && self.extractor_contract == ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT
            && match (self.source_revision.as_deref(), source_revision) {
                (Some(extracted), Some(current)) => extracted == current,
                (None, Some(current)) => parse_distill_revision(current)
                    .is_some_and(|revision| revision == self.input_revision),
                _ => false,
            }
    }

    /// Full producer compatibility used by coverage diagnostics and backfill.
    /// Revision equality alone is insufficient after a prompt/profile/model
    /// migration.
    pub fn is_compatible_with_extractor(
        &self,
        source_revision: Option<&str>,
        prompt_version: &str,
        identity: &SemanticExtractorIdentity,
    ) -> bool {
        self.is_compatible(source_revision)
            && self.prompt_version == prompt_version
            && self.model == identity.model
            && self.profile == identity.profile
    }

    pub(super) fn is_compatible_with_snapshot(
        &self,
        snapshot: &ActionabilityModelSnapshot,
        source_revision: Option<&str>,
    ) -> bool {
        self.is_compatible(source_revision)
            && self.extractor_contract == snapshot.semantic_extractor_contract
            && self.prompt_version == snapshot.semantic_prompt_version
            && self.model == snapshot.semantic_model
            && self.profile == snapshot.semantic_profile
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticExtractorIdentity {
    pub model: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelAttentionSemanticFeatures {
    pub communication_type: CommunicationType,
    pub requested_action: RequestedAction,
    pub action_owner: SemanticActionOwner,
    pub direct_request_probability: f64,
    pub broadcast_probability: f64,
    pub personal_obligation_probability: f64,
    pub information_value_probability: f64,
    pub deadline: SemanticDeadline,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_or_event_identity: Option<String>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationType {
    DirectRequest,
    PersonalUpdate,
    Transaction,
    Newsletter,
    Promotion,
    SystemNotice,
    Unknown,
}

impl CommunicationType {
    const fn feature_token(self) -> &'static str {
        match self {
            Self::DirectRequest => "direct_request",
            Self::PersonalUpdate => "personal_update",
            Self::Transaction => "transaction",
            Self::Newsletter => "newsletter",
            Self::Promotion => "promotion",
            Self::SystemNotice => "system_notice",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestedAction {
    Reply,
    Decide,
    Pay,
    Schedule,
    Review,
    Attend,
    None,
    Unknown,
}

impl RequestedAction {
    const fn feature_token(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Decide => "decide",
            Self::Pay => "pay",
            Self::Schedule => "schedule",
            Self::Review => "review",
            Self::Attend => "attend",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticActionOwner {
    Owner,
    Sender,
    ThirdParty,
    Shared,
    Unknown,
}

impl SemanticActionOwner {
    const fn feature_token(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Sender => "sender",
            Self::ThirdParty => "third_party",
            Self::Shared => "shared",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticDeadline {
    pub kind: SemanticDeadlineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDeadlineKind {
    Explicit,
    Implied,
    None,
}

impl SemanticDeadlineKind {
    const fn feature_token(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implied => "implied",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSemanticFeatures {
    communication_type: CommunicationType,
    requested_action: RequestedAction,
    action_owner: SemanticActionOwner,
    direct_request_probability: f64,
    broadcast_probability: f64,
    personal_obligation_probability: f64,
    information_value_probability: f64,
    deadline: SemanticDeadline,
    #[serde(default)]
    campaign_or_event_identity: Option<String>,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct SemanticValidationError(&'static str);

impl SemanticValidationError {
    const fn code(self) -> &'static str {
        self.0
    }
}

fn validate_semantic_features(
    value: &serde_json::Value,
) -> std::result::Result<ChannelAttentionSemanticFeatures, SemanticValidationError> {
    let raw: RawSemanticFeatures = serde_json::from_value(value.clone())
        .map_err(|_| SemanticValidationError("schema_mismatch"))?;
    for probability in [
        raw.direct_request_probability,
        raw.broadcast_probability,
        raw.personal_obligation_probability,
        raw.information_value_probability,
    ] {
        if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
            return Err(SemanticValidationError("probability_out_of_bounds"));
        }
    }
    if raw.evidence_refs.is_empty() || raw.evidence_refs.len() > 12 {
        return Err(SemanticValidationError("evidence_count_invalid"));
    }
    let allowed: HashSet<&str> = EVIDENCE_ALLOWLIST.iter().copied().collect();
    if raw
        .evidence_refs
        .iter()
        .any(|reference| !allowed.contains(reference.trim()))
    {
        return Err(SemanticValidationError("evidence_ref_not_allowed"));
    }
    let deadline_value = bounded_single_line(raw.deadline.value, 80)
        .map_err(|_| SemanticValidationError("deadline_value_invalid"))?;
    if raw.deadline.kind == SemanticDeadlineKind::None && deadline_value.is_some() {
        return Err(SemanticValidationError("deadline_value_without_deadline"));
    }
    let campaign_or_event_identity = bounded_single_line(raw.campaign_or_event_identity, 120)
        .map_err(|_| SemanticValidationError("campaign_identity_invalid"))?;
    Ok(ChannelAttentionSemanticFeatures {
        communication_type: raw.communication_type,
        requested_action: raw.requested_action,
        action_owner: raw.action_owner,
        direct_request_probability: raw.direct_request_probability,
        broadcast_probability: raw.broadcast_probability,
        personal_obligation_probability: raw.personal_obligation_probability,
        information_value_probability: raw.information_value_probability,
        deadline: SemanticDeadline {
            kind: raw.deadline.kind,
            value: deadline_value,
        },
        campaign_or_event_identity,
        evidence_refs: raw
            .evidence_refs
            .into_iter()
            .map(|value| value.trim().to_string())
            .collect(),
    })
}

fn bounded_single_line(
    value: Option<String>,
    max_chars: usize,
) -> std::result::Result<Option<String>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(Some(value.to_string()))
}

fn parse_distill_revision(source_revision: &str) -> Option<i64> {
    source_revision
        .strip_prefix("distill:")
        .and_then(|value| value.parse::<i64>().ok())
}

#[derive(Debug, Clone, Default)]
pub struct ActionabilityFeatureInput {
    pub semantic: Option<ChannelAttentionSemanticEnvelope>,
    pub classifier_label: Option<String>,
    pub classifier_confidence: Option<f64>,
    pub latest_direction: Option<String>,
    pub latest_intent: Option<String>,
    pub needs_reply_hint: bool,
    pub message_count: i64,
    pub age_days: Option<f64>,
    pub slice1_actionability_probability: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionabilityModelSnapshot {
    pub snapshot_id: String,
    pub model_version: String,
    pub feature_contract: String,
    pub semantic_schema_version: u32,
    /// Exact semantic producer contract represented by the training rows.
    /// Serving does not treat a prompt/model/profile change as equivalent data.
    pub semantic_extractor_contract: String,
    pub semantic_prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_profile: Option<String>,
    pub feature_names: Vec<String>,
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    pub l2_lambda: f64,
    pub platt_a: f64,
    pub platt_b: f64,
    pub trained_at: i64,
    pub training_manifest: ActionabilityTrainingManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionabilityTrainingManifest {
    pub dataset_digest: String,
    pub data_cutoff_at: i64,
    pub split_strategy: String,
    pub group_keys: Vec<String>,
    pub positive_outcomes: Vec<String>,
    pub negative_outcomes: Vec<String>,
    pub excluded_outcomes: Vec<String>,
    pub metrics: BTreeMap<String, f64>,
}

impl ActionabilityModelSnapshot {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.snapshot_id.trim().is_empty(),
            "actionability snapshot id is empty"
        );
        anyhow::ensure!(
            !self.model_version.trim().is_empty(),
            "actionability model version is empty"
        );
        anyhow::ensure!(
            self.feature_contract == ACTIONABILITY_FEATURE_CONTRACT,
            "actionability feature contract is incompatible"
        );
        anyhow::ensure!(
            self.semantic_schema_version == ATTENTION_SEMANTIC_SCHEMA_VERSION,
            "actionability semantic schema is incompatible"
        );
        anyhow::ensure!(
            self.semantic_extractor_contract == ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
            "actionability semantic extractor contract is incompatible"
        );
        anyhow::ensure!(
            !self.semantic_prompt_version.trim().is_empty(),
            "actionability semantic prompt version is empty"
        );
        anyhow::ensure!(
            self.semantic_model
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty()),
            "actionability semantic model identity is empty"
        );
        anyhow::ensure!(
            self.semantic_profile
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty()),
            "actionability semantic profile identity is empty"
        );
        anyhow::ensure!(
            !self.feature_names.is_empty(),
            "actionability snapshot has no features"
        );
        anyhow::ensure!(
            self.feature_names.len() == self.coefficients.len(),
            "actionability coefficient dimension mismatch"
        );
        anyhow::ensure!(
            self.intercept.is_finite() && self.l2_lambda.is_finite() && self.l2_lambda >= 0.0,
            "actionability model parameters are invalid"
        );
        anyhow::ensure!(
            self.platt_a.is_finite() && self.platt_b.is_finite(),
            "actionability calibrator is invalid"
        );
        let mut unique = HashSet::new();
        for (name, coefficient) in self.feature_names.iter().zip(&self.coefficients) {
            anyhow::ensure!(
                unique.insert(name),
                "duplicate actionability feature: {name}"
            );
            anyhow::ensure!(
                supported_feature(name),
                "unsupported actionability feature: {name}"
            );
            anyhow::ensure!(
                coefficient.is_finite(),
                "non-finite actionability coefficient"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionabilityExplanation {
    pub code: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionabilityInference {
    pub probability: f64,
    pub explanation: ActionabilityExplanation,
    pub model_version: String,
    pub snapshot_id: String,
    /// Digest of the exact compatible semantic envelope and ordered model
    /// feature vector used for this inference.
    pub input_digest: String,
}

#[derive(Serialize)]
struct ActionabilityInputDigest<'a> {
    source_revision: &'a str,
    semantic_envelope: &'a ChannelAttentionSemanticEnvelope,
    feature_contract: &'a str,
    feature_names: &'a [String],
    feature_values: Vec<f64>,
}

pub fn actionability_input_digest(
    snapshot: &ActionabilityModelSnapshot,
    source_revision: Option<&str>,
    input: &ActionabilityFeatureInput,
) -> Result<Option<String>> {
    snapshot.validate()?;
    let Some(source_revision) = source_revision else {
        return Ok(None);
    };
    let Some(semantic) = input.semantic.as_ref() else {
        return Ok(None);
    };
    if !semantic.is_compatible_with_snapshot(snapshot, Some(source_revision)) {
        return Ok(None);
    }
    let Some(features) = semantic.features.as_ref() else {
        return Ok(None);
    };
    let feature_map = feature_values(features, input);
    let ordered_values = snapshot
        .feature_names
        .iter()
        .map(|name| feature_map.get(name).copied().unwrap_or(0.0))
        .collect();
    let payload = ActionabilityInputDigest {
        source_revision,
        semantic_envelope: semantic,
        feature_contract: &snapshot.feature_contract,
        feature_names: &snapshot.feature_names,
        feature_values: ordered_values,
    };
    let encoded = serde_json::to_vec(&payload)
        .context("serializing deterministic actionability inference input")?;
    Ok(Some(blake3::hash(&encoded).to_hex().to_string()))
}

pub fn infer_actionability(
    snapshot: &ActionabilityModelSnapshot,
    source_revision: Option<&str>,
    input: &ActionabilityFeatureInput,
) -> Result<Option<ActionabilityInference>> {
    let Some(input_digest) = actionability_input_digest(snapshot, source_revision, input)? else {
        return Ok(None);
    };
    let features = input
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.features.as_ref())
        .expect("digest generation requires validated semantic features");
    let values = feature_values(features, input);
    let mut linear = snapshot.intercept;
    let mut strongest: Option<(&str, f64)> = None;
    for (name, coefficient) in snapshot.feature_names.iter().zip(&snapshot.coefficients) {
        let value = values.get(name).copied().unwrap_or(0.0);
        let contribution = coefficient * value;
        linear += contribution;
        if value != 0.0
            && strongest
                .as_ref()
                .is_none_or(|(_, current)| contribution.abs() > current.abs())
        {
            strongest = Some((name.as_str(), contribution));
        }
    }
    let probability = sigmoid(snapshot.platt_a * linear + snapshot.platt_b);
    anyhow::ensure!(
        probability.is_finite(),
        "actionability inference was non-finite"
    );
    let (feature, contribution) = strongest.unwrap_or(("model_intercept", snapshot.intercept));
    Ok(Some(ActionabilityInference {
        probability,
        explanation: explanation_for(feature, contribution),
        model_version: snapshot.model_version.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        input_digest,
    }))
}

/// Ordered feature vector that serving and training must share.
///
/// `names` order is part of the snapshot contract: a later trainer that
/// permutes the same keys produces a different model even if the values match.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionabilityFeatureVector {
    pub names: Vec<String>,
    pub values: Vec<f64>,
    pub contract: &'static str,
}

/// Serving-side extraction. Training must call this rather than reimplement it.
pub fn feature_vector(input: &ActionabilityFeatureInput) -> Option<ActionabilityFeatureVector> {
    let values = captured_feature_values(input)?;
    let names = values.keys().cloned().collect::<Vec<_>>();
    let ordered = names
        .iter()
        .map(|name| values.get(name).copied().unwrap_or(0.0))
        .collect();
    Some(ActionabilityFeatureVector {
        names,
        values: ordered,
        contract: ACTIONABILITY_FEATURE_CONTRACT,
    })
}

/// The feature vector as it stood when this candidate was served.
///
/// Training reads this and serving reads [`feature_values`] — the same
/// function, so a trainer cannot drift from the model it is fitting. The
/// values are captured at decision time rather than recomputed later because
/// recomputation is not possible: labelling an item is what retires it, and a
/// retired item's semantics are gone by the time any trainer runs.
///
/// `None` when the candidate has no semantic envelope, which is the honest
/// answer — there is no feature vector to record, and inventing zeroes would
/// make an absent extraction indistinguishable from a confident zero.
pub fn captured_feature_values(input: &ActionabilityFeatureInput) -> Option<BTreeMap<String, f64>> {
    let semantic = input.semantic.as_ref()?.features.as_ref()?;
    Some(feature_values(semantic, input))
}

fn feature_values(
    semantic: &ChannelAttentionSemanticFeatures,
    input: &ActionabilityFeatureInput,
) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    out.insert(
        "semantic.direct_request_probability".to_string(),
        semantic.direct_request_probability,
    );
    out.insert(
        "semantic.broadcast_probability".to_string(),
        semantic.broadcast_probability,
    );
    out.insert(
        "semantic.personal_obligation_probability".to_string(),
        semantic.personal_obligation_probability,
    );
    out.insert(
        "semantic.information_value_probability".to_string(),
        semantic.information_value_probability,
    );
    out.insert(
        format!(
            "semantic.communication_type.{}",
            semantic.communication_type.feature_token()
        ),
        1.0,
    );
    out.insert(
        format!(
            "semantic.requested_action.{}",
            semantic.requested_action.feature_token()
        ),
        1.0,
    );
    out.insert(
        format!(
            "semantic.action_owner.{}",
            semantic.action_owner.feature_token()
        ),
        1.0,
    );
    out.insert(
        format!(
            "semantic.deadline.{}",
            semantic.deadline.kind.feature_token()
        ),
        1.0,
    );
    if let Some(label) = input.classifier_label.as_deref() {
        out.insert(
            format!("classifier.label.{}", normalized_feature_token(label)),
            1.0,
        );
    }
    if let Some(confidence) = input
        .classifier_confidence
        .filter(|value| value.is_finite())
    {
        out.insert(
            "classifier.confidence".to_string(),
            confidence.clamp(0.0, 1.0),
        );
    }
    if let Some(direction) = input.latest_direction.as_deref() {
        out.insert(
            format!("direction.{}", normalized_feature_token(direction)),
            1.0,
        );
    }
    if let Some(intent) = input.latest_intent.as_deref() {
        out.insert(format!("intent.{}", normalized_feature_token(intent)), 1.0);
    }
    out.insert(
        "needs_reply_hint".to_string(),
        if input.needs_reply_hint { 1.0 } else { 0.0 },
    );
    out.insert(
        "message_count_log1p".to_string(),
        (input.message_count.max(0) as f64).ln_1p(),
    );
    if let Some(age_days) = input
        .age_days
        .filter(|value| value.is_finite() && *value >= 0.0)
    {
        out.insert(
            "age_days_log1p".to_string(),
            bucket_age_days(age_days).ln_1p(),
        );
    }
    if let Some(probability) = input
        .slice1_actionability_probability
        .filter(|value| value.is_finite())
    {
        out.insert(
            "slice1.actionability_probability".to_string(),
            probability.clamp(0.0, 1.0),
        );
    }
    out
}

/// Stable temporal buckets prevent the wall clock from minting a distinct
/// training vector on every projection while retaining higher resolution for
/// the recent items where age affects actionability most.
pub fn bucket_age_days(age_days: f64) -> f64 {
    let width_days = if age_days < 1.0 {
        15.0 / (24.0 * 60.0)
    } else if age_days < 7.0 {
        1.0 / 24.0
    } else if age_days < 30.0 {
        6.0 / 24.0
    } else if age_days < 180.0 {
        1.0
    } else {
        7.0
    };
    (age_days / width_days).floor() * width_days
}

fn normalized_feature_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

pub fn supported_training_feature(name: &str) -> bool {
    supported_feature(name)
}

fn supported_feature(name: &str) -> bool {
    matches!(
        name,
        "semantic.direct_request_probability"
            | "semantic.broadcast_probability"
            | "semantic.personal_obligation_probability"
            | "semantic.information_value_probability"
            | "classifier.confidence"
            | "needs_reply_hint"
            | "message_count_log1p"
            | "age_days_log1p"
            | "slice1.actionability_probability"
    ) || name.starts_with("semantic.communication_type.")
        || name.starts_with("semantic.requested_action.")
        || name.starts_with("semantic.action_owner.")
        || name.starts_with("semantic.deadline.")
        || name.starts_with("classifier.label.")
        || name.starts_with("direction.")
        || name.starts_with("intent.")
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn explanation_for(feature: &str, contribution: f64) -> ActionabilityExplanation {
    let positive = contribution >= 0.0;
    let (code, positive_label, negative_label) = if feature == "semantic.direct_request_probability"
        || feature == "needs_reply_hint"
        || feature.starts_with("semantic.communication_type.direct_request")
    {
        (
            "direct_request",
            "Direct request for you",
            "Little evidence of a direct request",
        )
    } else if feature == "semantic.broadcast_probability"
        || feature.starts_with("semantic.communication_type.newsletter")
        || feature.starts_with("semantic.communication_type.promotion")
    {
        (
            "broadcast",
            "Looks like a broad communication",
            "Not shaped like a broadcast",
        )
    } else if feature == "semantic.personal_obligation_probability"
        || feature.starts_with("semantic.action_owner.owner")
    {
        (
            "personal_obligation",
            "Likely your obligation",
            "Weak evidence that you own the action",
        )
    } else if feature.starts_with("semantic.deadline.explicit") {
        (
            "explicit_deadline",
            "Has a supported deadline",
            "Deadline evidence reduced actionability",
        )
    } else if feature.starts_with("semantic.requested_action.none") {
        (
            "no_requested_action",
            "No explicit action was extracted",
            "No-action evidence was discounted",
        )
    } else if feature == "slice1.actionability_probability" {
        (
            "owner_feedback_similarity",
            "Similar owner feedback was actionable",
            "Similar owner feedback was not actionable",
        )
    } else {
        (
            "model_evidence",
            "Model evidence supports actionability",
            "Model evidence lowers actionability",
        )
    };
    ActionabilityExplanation {
        code: if positive {
            code.to_string()
        } else {
            format!("{code}_negative")
        },
        label: if positive {
            positive_label
        } else {
            negative_label
        }
        .to_string(),
    }
}

pub fn deserialize_semantic_envelope(
    value: Option<&serde_json::Value>,
) -> Option<ChannelAttentionSemanticEnvelope> {
    value
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub fn serialize_semantic_envelope(
    value: &ChannelAttentionSemanticEnvelope,
) -> Result<serde_json::Value> {
    serde_json::to_value(value).context("serializing attention semantic envelope")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn valid_value() -> serde_json::Value {
        serde_json::json!({
            "communication_type": "direct_request",
            "requested_action": "reply",
            "action_owner": "owner",
            "direct_request_probability": 0.9,
            "broadcast_probability": 0.05,
            "personal_obligation_probability": 0.85,
            "information_value_probability": 0.4,
            "deadline": {"kind": "none", "value": null},
            "campaign_or_event_identity": null,
            "evidence_refs": ["latest_intent", "needs_reply_hint"]
        })
    }

    #[test]
    fn invalid_semantics_do_not_become_features() {
        let mut value = valid_value();
        value["direct_request_probability"] = serde_json::json!(1.5);
        let envelope = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&value),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        assert_eq!(envelope.status, SemanticExtractionStatus::Invalid);
        assert!(envelope.features.is_none());
    }

    #[test]
    fn evidence_is_a_field_reference_not_copied_prose() {
        let mut value = valid_value();
        value["evidence_refs"] = serde_json::json!(["Please reply by tomorrow"]);
        let envelope = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&value),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        assert_eq!(
            envelope.invalid_reason_code.as_deref(),
            Some("evidence_ref_not_allowed")
        );
    }

    #[test]
    fn calibrated_inference_requires_exact_revision() {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&valid_value()),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        let snapshot = ActionabilityModelSnapshot {
            snapshot_id: "snapshot-1".to_string(),
            model_version: "logistic-v1".to_string(),
            feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: None,
            semantic_profile: None,
            feature_names: vec!["semantic.direct_request_probability".to_string()],
            coefficients: vec![2.0],
            intercept: -0.5,
            l2_lambda: 1.0,
            platt_a: 1.0,
            platt_b: 0.0,
            trained_at: 1,
            training_manifest: ActionabilityTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["sender".to_string()],
                positive_outcomes: vec!["action_completed".to_string()],
                negative_outcomes: vec!["not_actionable".to_string()],
                excluded_outcomes: vec!["useful".to_string()],
                metrics: BTreeMap::new(),
            },
        };
        let input = ActionabilityFeatureInput {
            semantic: Some(semantic),
            ..Default::default()
        };
        let original = infer_actionability(&snapshot, Some("distill:7"), &input)
            .unwrap()
            .unwrap();
        let mut changed_input = input.clone();
        changed_input
            .semantic
            .as_mut()
            .unwrap()
            .features
            .as_mut()
            .unwrap()
            .direct_request_probability = 0.8;
        let changed = infer_actionability(&snapshot, Some("distill:7"), &changed_input)
            .unwrap()
            .unwrap();
        assert_ne!(original.input_digest, changed.input_digest);
        assert!(infer_actionability(&snapshot, Some("distill:8"), &input)
            .unwrap()
            .is_none());
    }

    #[test]
    fn calibrated_inference_requires_exact_extractor_identity() {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&valid_value()),
            7,
            "1.1.0",
            &SemanticExtractorIdentity {
                model: Some("model-a".to_string()),
                profile: Some("profile-a".to_string()),
            },
        );
        let mut snapshot = ActionabilityModelSnapshot {
            snapshot_id: "snapshot-1".to_string(),
            model_version: "logistic-v1".to_string(),
            feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: Some("model-b".to_string()),
            semantic_profile: Some("profile-a".to_string()),
            feature_names: vec!["semantic.direct_request_probability".to_string()],
            coefficients: vec![2.0],
            intercept: -0.5,
            l2_lambda: 1.0,
            platt_a: 1.0,
            platt_b: 0.0,
            trained_at: 1,
            training_manifest: ActionabilityTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["sender".to_string()],
                positive_outcomes: vec!["action_completed".to_string()],
                negative_outcomes: vec!["not_actionable".to_string()],
                excluded_outcomes: vec!["useful".to_string()],
                metrics: BTreeMap::new(),
            },
        };
        let input = ActionabilityFeatureInput {
            semantic: Some(semantic),
            ..Default::default()
        };
        assert!(infer_actionability(&snapshot, Some("distill:7"), &input)
            .unwrap()
            .is_none());
        snapshot.semantic_model = Some("model-a".to_string());
        assert!(infer_actionability(&snapshot, Some("distill:7"), &input)
            .unwrap()
            .is_some());
        snapshot.semantic_prompt_version = "1.2.0".to_string();
        assert!(infer_actionability(&snapshot, Some("distill:7"), &input)
            .unwrap()
            .is_none());
    }

    #[test]
    fn clock_movement_inside_a_temporal_bucket_reuses_the_exact_feature_vector() {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&valid_value()),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        let first = captured_feature_values(&ActionabilityFeatureInput {
            semantic: Some(semantic.clone()),
            age_days: Some(0.501),
            ..Default::default()
        })
        .unwrap();
        let same_bucket = captured_feature_values(&ActionabilityFeatureInput {
            semantic: Some(semantic),
            age_days: Some(0.509),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(first, same_bucket);
        assert_eq!(
            ATTENTION_TEMPORAL_FEATURE_CONTRACT,
            "attention_temporal_buckets_v1"
        );
    }

    #[test]
    fn crossing_a_temporal_bucket_changes_inference_and_capture_consistently() {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&valid_value()),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        let before = ActionabilityFeatureInput {
            semantic: Some(semantic.clone()),
            age_days: Some(0.509),
            ..Default::default()
        };
        let after = ActionabilityFeatureInput {
            semantic: Some(semantic),
            age_days: Some(0.512),
            ..Default::default()
        };
        let captured_before = captured_feature_values(&before).unwrap();
        let captured_after = captured_feature_values(&after).unwrap();
        assert_ne!(captured_before, captured_after);
        assert_eq!(
            captured_before.get("age_days_log1p"),
            Some(&bucket_age_days(0.509).ln_1p())
        );
        assert_eq!(
            captured_after.get("age_days_log1p"),
            Some(&bucket_age_days(0.512).ln_1p())
        );
    }
}

#[cfg(test)]
mod attention_recovery_tests {
    use super::*;
    #[test]
    fn provider_schema_and_local_validator_agree_on_evidence() {
        let schema = semantic_features_response_schema();
        assert_eq!(
            schema["properties"]["evidence_refs"]["items"]["enum"],
            serde_json::json!(EVIDENCE_ALLOWLIST)
        );
        let mut valid = serde_json::json!({
            "communication_type":"direct_request", "requested_action":"reply", "action_owner":"owner",
            "direct_request_probability":0.8, "broadcast_probability":0.1,
            "personal_obligation_probability":0.8, "information_value_probability":0.5,
            "deadline":{"kind":"none","value":null}, "campaign_or_event_identity":null,
            "evidence_refs":["summary"]
        });
        for refs in [
            serde_json::json!([]),
            serde_json::json!(["copied private prose"]),
            serde_json::json!(vec!["summary"; 13]),
        ] {
            valid["evidence_refs"] = refs;
            assert!(validate_semantic_features(&valid).is_err());
        }
        valid["evidence_refs"] = serde_json::json!(["summary"]);
        assert!(validate_semantic_features(&valid).is_ok());
    }
}
