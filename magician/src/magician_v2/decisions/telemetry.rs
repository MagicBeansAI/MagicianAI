//! Content-free classification evidence. Costs reuse the existing dated registry.
use super::runner::Input;
use crate::magician_v2::{
    analytics::{decision_model_telemetry, llm_trace_recorder::LlmPricingFact},
    decision_host::{self, classification::Participation},
    realtime_events::RuntimeTransportEvent,
};
use decision_engine_contract::{
    batch::DecisionItemResult, telemetry::DecisionModelCall, DecideResponse,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

/// Closed-set reference outputs only: item -> question -> label/score.
pub(crate) type Labels = BTreeMap<String, BTreeMap<String, serde_json::Value>>;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Reference {
    pub labels: Labels,
    pub call: Option<ReferenceCall>,
    pub latency_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReferenceCall {
    pub receipt: Option<magicllm::LlmTraceReceipt>,
    pub provider: String,
    pub model: String,
    pub profile: Option<String>,
    pub operation: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
    pub pricing_version: Option<String>,
    pub attempts_complete: bool,
}
impl From<Labels> for Reference {
    fn from(labels: Labels) -> Self {
        Self {
            labels,
            call: None,
            latency_ms: 0,
        }
    }
}
impl Reference {
    pub fn from_response(
        labels: Labels,
        response: &crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
        latency_ms: u64,
    ) -> Self {
        let call = response.telemetry.as_ref().map(|tel| {
            let provider = magicllm::LLMProviderKind::from_str(&tel.provider);
            let table = magicllm::pricing::active_table();
            let attempts_complete = tel
                .trace_receipt
                .as_ref()
                .is_some_and(|r| r.provider_attempt_count <= 1);
            let cost_known = tel.usage_availability.map(|a| a.cost).unwrap_or_else(|| {
                tel.usage_reported
                    && table
                        .lookup_at(&provider, &tel.model, tel.started_at_ms)
                        .is_some()
            });
            ReferenceCall {
                receipt: tel.trace_receipt.clone(),
                provider: tel.provider.clone(),
                model: tel.model.clone(),
                profile: tel.profile.clone(),
                operation: tel.operation.clone(),
                input_tokens: tel.usage_reported.then_some(tel.input_tokens),
                output_tokens: tel.usage_reported.then_some(tel.output_tokens),
                cache_read_tokens: (tel.cache_read_tokens > 0
                    || tel.usage_availability.is_some_and(|a| a.cache_read))
                .then_some(tel.cache_read_tokens),
                cache_write_tokens: (tel.cache_creation_tokens > 0
                    || tel.usage_availability.is_some_and(|a| a.cache_write))
                .then_some(tel.cache_creation_tokens),
                cost_usd: (cost_known
                    && attempts_complete
                    && tel.cost_usd.is_finite()
                    && tel.cost_usd >= 0.0)
                    .then_some(tel.cost_usd),
                pricing_version: table.pricing_version_at(&provider, &tel.model, tel.started_at_ms),
                attempts_complete,
            }
        });
        Self {
            labels,
            call,
            latency_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShadowRecord {
    pub schema_version: u32,
    pub comparison_id: String,
    pub case_id: String,
    pub operation: String,
    pub projection_version: String,
    pub reference_version: String,
    pub behavior_fingerprint: String,
    pub engine_instance: String,
    pub policy_revision: String,
    pub mode: String,
    pub scope: magicllm::LlmTraceContext,
    pub offered: usize,
    pub required_questions: Vec<String>,
    pub offered_item_ids: Vec<String>,
    pub items: Vec<DecisionItemResult>,
    pub threshold_fingerprints: BTreeMap<String, String>,
    /// None is missing evidence, never a negative label.
    pub reference: Option<Reference>,
    pub calls: Vec<DecisionModelCall>,
    pub pricing: BTreeMap<String, LlmPricingFact>,
    pub wall_latency_ms: u64,
    /// Missing on historical records; never infer zero discovery time for them.
    #[serde(default)]
    pub discovery_latency_ms: Option<u64>,
    pub foreground_latency_ms: Option<u64>,
    pub engine_latency_ms: Option<u64>,
    pub accounting_complete: bool,
    pub application_cache_hit: bool,
    pub reference_attempted: bool,
    pub requires_completion: bool,
}
pub(crate) fn record(
    input: &Input,
    policy: &Participation,
    reply: Option<&DecideResponse>,
    reference: Option<Reference>,
    elapsed: Duration,
    mode: &str,
    reference_attempted: bool,
    foreground_latency_ms: Option<u64>,
) -> Observation {
    let calls = reply.map(|r| r.model_calls.clone()).unwrap_or_default();
    let pricing = calls
        .iter()
        .map(|call| {
            (
                call.call_id.clone(),
                decision_model_telemetry::pricing(
                    &call.provider,
                    &call.model,
                    call.started_at_ms,
                    &decision_model_telemetry::call_usage(call),
                ),
            )
        })
        .collect();
    let items: Vec<DecisionItemResult> = reply
        .map(|r| {
            r.batch
                .items
                .iter()
                .cloned()
                .map(|mut item| {
                    // Provider error bodies can contain source text. Only status is exported.
                    if item.error.is_some() {
                        item.error = Some(format!("{:?}", item.status));
                    }
                    item
                })
                .collect()
        })
        .unwrap_or_default();
    let observation = Observation {
        comparison_id: ulid::Ulid::new().to_string(),
        principal: input.scope.scope.principal.clone(),
        workspace: input.scope.scope.workspace.clone(),
    };
    let record = ShadowRecord {
        schema_version: 1,
        comparison_id: observation.comparison_id.clone(),
        case_id: input.case_id.clone(),
        operation: input.operation.clone(),
        projection_version: input.projection_version.clone(),
        reference_version: input.reference_version.clone(),
        behavior_fingerprint: policy.policy.classification.behavior_fingerprint.clone(),
        engine_instance: policy.engine_instance.clone(),
        policy_revision: policy.revision.clone(),
        mode: mode.into(),
        scope: input.scope.clone(),
        offered: input.items.len(),
        required_questions: input.required_questions.clone(),
        offered_item_ids: input.items.iter().map(|i| i.item_id.clone()).collect(),
        threshold_fingerprints: items
            .iter()
            .filter_map(|item| {
                item.thresholds.as_ref().map(|thresholds| {
                    (
                        item.item_id.clone(),
                        magician_decision::config::fingerprint(thresholds),
                    )
                })
            })
            .collect(),
        items,
        reference,
        calls,
        pricing,
        wall_latency_ms: elapsed.as_millis() as u64,
        discovery_latency_ms: Some(policy.discovery_latency_ms),
        foreground_latency_ms,
        engine_latency_ms: reply.map(|r| r.latency_ms),
        accounting_complete: reply.is_some(),
        application_cache_hit: false,
        reference_attempted,
        requires_completion: mode == "gate" && input.requires_completion,
    };
    if let Ok(record) = serde_json::to_value(record) {
        decision_host::emit_model_event(RuntimeTransportEvent::DecisionShadowAgreement {
            principal: input.scope.scope.principal.clone(),
            workspace: input.scope.scope.workspace.clone(),
            record: Box::new(record),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
    observation
}

/// Follow-up evidence joins to the primary comparison even if export order differs.
#[derive(Clone)]
pub(crate) struct Observation {
    comparison_id: String,
    principal: String,
    workspace: String,
}
impl Observation {
    /// A separate reference stage keeps the production gate and required-text
    /// completion unchanged. The report joins stages by comparison ID.
    pub fn reference(&self, status: &str, attempted: Option<bool>, reference: Option<Reference>) {
        let record = serde_json::json!({
            "schema_version": 1, "comparison_id": self.comparison_id,
            "stage": "reference", "status": status,
            "reference_attempted": attempted, "reference": reference,
        });
        decision_host::emit_model_event(RuntimeTransportEvent::DecisionShadowAgreement {
            principal: self.principal.clone(),
            workspace: self.workspace.clone(),
            record: Box::new(record),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
    pub fn complete(
        &self,
        text: Option<Reference>,
        text_attempted: bool,
        validated: bool,
        elapsed: Duration,
    ) {
        let record = serde_json::json!({
            "schema_version": 1, "comparison_id": self.comparison_id,
            "stage": "completion", "text_call": text.and_then(|r| r.call),
            "text_attempted": text_attempted, "result_validated": validated,
            "foreground_ms": elapsed.as_millis() as u64,
        });
        decision_host::emit_model_event(RuntimeTransportEvent::DecisionShadowAgreement {
            principal: self.principal.clone(),
            workspace: self.workspace.clone(),
            record: Box::new(record),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
}

/// Operational observations never count as held-out inference evidence.
pub(crate) fn invocation(
    principal: &str,
    workspace: &str,
    operation: &str,
    case_id: &str,
    offered: usize,
    status: &str,
    elapsed: Duration,
) {
    decision_host::emit_model_event(RuntimeTransportEvent::DecisionShadowAgreement {
        principal: principal.into(),
        workspace: workspace.into(),
        record: Box::new(serde_json::json!({"schema_version":1,"stage":"invocation",
            "comparison_id":ulid::Ulid::new().to_string(),"operation":operation,"case_id":case_id,
            "offered":offered,"status":status,"foreground_ms":elapsed.as_millis() as u64})),
        timestamp: chrono::Utc::now().timestamp_millis(),
    });
}
