//! Product-safe response contract for MCP multi-round-trip requests (MRTR).
//!
//! Server request keys and SDK request objects remain private. The product receives
//! opaque, exact-continuation input ids plus stable request kinds and returns move-only
//! JSON values. Preparation validates an exact, complete, duplicate-free response set
//! and the matching SDK result shape without claiming or resuming the continuation.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    num::NonZeroU32,
};

#[allow(deprecated)]
use rmcp::model::{
    CreateMessageResult, ElicitRequestParams, ElicitResult, ElicitationAction, EnumSchema,
    InputRequest, InputRequests, InputRequiredResult, InputResponses, ListRootsResult,
    MultiSelectEnumSchema, PrimitiveSchemaDefinition, SingleSelectEnumSchema,
};
use serde_json::Value;

use crate::{
    continuation::{McpContinuationId, McpContinuationRevision, McpPendingCall},
    validation::{drop_json_value_iterative, json_string_wire_len, measure_json},
    McpClientError, McpClientLimits,
};

pub const MCP_MRTR_RESPONSE_CONTRACT_V1: &str = "magician.mcp-mrtr-response.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpMrtrInputKind {
    Sampling,
    Elicitation,
    Roots,
}

/// Opaque identity for one SDK-issued input request in one continuation revision.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpMrtrInputId {
    continuation_id: McpContinuationId,
    revision: McpContinuationRevision,
    ordinal: NonZeroU32,
}

impl fmt::Debug for McpMrtrInputId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpMrtrInputId([REDACTED])")
    }
}

/// Payload-free product slot for one response required by an MRTR round.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct McpMrtrInputSlot {
    id: McpMrtrInputId,
    kind: McpMrtrInputKind,
}

impl McpMrtrInputSlot {
    pub fn id(self) -> McpMrtrInputId {
        self.id
    }

    pub fn kind(self) -> McpMrtrInputKind {
        self.kind
    }
}

impl fmt::Debug for McpMrtrInputSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpMrtrInputSlot")
            .field("contract", &MCP_MRTR_RESPONSE_CONTRACT_V1)
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Move-only product response for one opaque MRTR input id.
pub struct McpMrtrResponse {
    input_id: McpMrtrInputId,
    value: Option<Value>,
}

impl McpMrtrResponse {
    pub fn new(input_id: McpMrtrInputId, value: Value) -> Self {
        Self {
            input_id,
            value: Some(value),
        }
    }

    fn into_parts(mut self) -> (McpMrtrInputId, Value) {
        let value = self.value.take().expect("MRTR response value is present");
        (self.input_id, value)
    }
}

impl fmt::Debug for McpMrtrResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpMrtrResponse")
            .field("contract", &MCP_MRTR_RESPONSE_CONTRACT_V1)
            .field("input_id", &self.input_id)
            .field("has_value", &self.value.is_some())
            .finish()
    }
}

impl Drop for McpMrtrResponse {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            drop_json_value_iterative(value);
        }
    }
}

/// Move-only, SDK-private response map prepared for an atomic claim and future resume.
///
/// Preparation is not authorization and does not mutate continuation state. The claim
/// boundary consumes this object only after rechecking the exact revision under lock.
pub struct McpPreparedMrtrResponses {
    pending: McpPendingCall,
    responses: Option<InputResponses>,
    accounted_bytes: usize,
}

impl McpPreparedMrtrResponses {
    pub fn pending(&self) -> McpPendingCall {
        self.pending
    }

    pub fn response_count(&self) -> usize {
        self.responses.as_ref().map_or(0, BTreeMap::len)
    }

    pub(crate) fn accounted_bytes(&self) -> usize {
        self.accounted_bytes
    }

    pub(crate) fn take_responses(&mut self) -> Option<InputResponses> {
        self.responses.take()
    }

    pub(crate) fn restore_responses(&mut self, responses: InputResponses) {
        debug_assert!(self.responses.is_none());
        self.responses = Some(responses);
    }

    pub(crate) fn into_responses(mut self) -> Option<InputResponses> {
        self.responses.take()
    }
}

impl fmt::Debug for McpPreparedMrtrResponses {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpPreparedMrtrResponses")
            .field("contract", &MCP_MRTR_RESPONSE_CONTRACT_V1)
            .field("kind", &self.pending.kind())
            .field("revision", &self.pending.revision())
            .field("response_count", &self.response_count())
            .field("accounted_bytes", &self.accounted_bytes)
            .finish()
    }
}

impl Drop for McpPreparedMrtrResponses {
    fn drop(&mut self) {
        if let Some(responses) = self.responses.take() {
            drop_input_responses_iterative(responses);
        }
    }
}

pub(crate) fn drop_input_responses_iterative(responses: InputResponses) {
    for (_, value) in responses {
        drop_json_value_iterative(value);
    }
}

pub(crate) fn project_input_slots(
    pending: McpPendingCall,
    result: &InputRequiredResult,
    max_inputs: usize,
) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
    project_input_slots_from_requests(pending, result.input_requests.as_ref(), max_inputs)
}

pub(crate) fn project_input_slots_from_requests(
    pending: McpPendingCall,
    requests: Option<&InputRequests>,
    max_inputs: usize,
) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
    let count = requests.map_or(0, BTreeMap::len);
    if count > max_inputs || count > u32::MAX as usize {
        return Err(McpClientError::ContinuationResponseRejected);
    }
    requests
        .into_iter()
        .flat_map(BTreeMap::values)
        .enumerate()
        .map(|(index, request)| {
            let ordinal = u32::try_from(index.saturating_add(1))
                .ok()
                .and_then(NonZeroU32::new)
                .ok_or(McpClientError::ContinuationResponseRejected)?;
            Ok(McpMrtrInputSlot {
                id: McpMrtrInputId {
                    continuation_id: pending.id,
                    revision: pending.revision,
                    ordinal,
                },
                kind: input_kind(request)?,
            })
        })
        .collect()
}

pub(crate) fn prepare_input_responses(
    pending: McpPendingCall,
    result: &InputRequiredResult,
    responses: Vec<McpMrtrResponse>,
    limits: &McpClientLimits,
) -> Result<McpPreparedMrtrResponses, McpClientError> {
    prepare_input_responses_from_requests(
        pending,
        result.input_requests.as_ref(),
        responses,
        limits,
    )
}

pub(crate) fn prepare_input_responses_from_requests(
    pending: McpPendingCall,
    requests: Option<&InputRequests>,
    responses: Vec<McpMrtrResponse>,
    limits: &McpClientLimits,
) -> Result<McpPreparedMrtrResponses, McpClientError> {
    let slots = project_input_slots_from_requests(pending, requests, limits.max_mrtr_inputs)?;
    if responses.len() != slots.len() || responses.len() > limits.max_mrtr_inputs {
        return Err(McpClientError::ContinuationResponseRejected);
    }

    let mut by_ordinal = (0..slots.len()).map(|_| None).collect::<Vec<_>>();
    for response in responses {
        let input_id = response.input_id;
        if input_id.continuation_id != pending.id || input_id.revision != pending.revision {
            return Err(McpClientError::ContinuationResponseRejected);
        }
        let index = usize::try_from(input_id.ordinal.get() - 1)
            .map_err(|_| McpClientError::ContinuationResponseRejected)?;
        let Some(slot) = by_ordinal.get_mut(index) else {
            return Err(McpClientError::ContinuationResponseRejected);
        };
        if slot.replace(response).is_some() {
            return Err(McpClientError::ContinuationResponseRejected);
        }
    }

    let mut input_responses = InputResponses::new();
    let mut accounted_bytes = 2usize.saturating_add(slots.len().saturating_sub(1));
    for (index, (request_key, request)) in requests.into_iter().flat_map(BTreeMap::iter).enumerate()
    {
        let response = by_ordinal[index]
            .take()
            .ok_or(McpClientError::ContinuationResponseRejected)?;
        let (_, value) = response.into_parts();
        let (value, value_bytes) = normalize_response(request, value, limits)?;
        accounted_bytes = accounted_bytes
            .checked_add(json_string_wire_len(request_key))
            .and_then(|bytes| bytes.checked_add(1))
            .and_then(|bytes| bytes.checked_add(value_bytes))
            .ok_or(McpClientError::ContinuationResponseRejected)?;
        if accounted_bytes > limits.max_request_bytes {
            drop_json_value_iterative(value);
            return Err(McpClientError::ContinuationResponseRejected);
        }
        input_responses.insert(request_key.clone(), value);
    }

    Ok(McpPreparedMrtrResponses {
        pending,
        responses: Some(input_responses),
        accounted_bytes,
    })
}

fn input_kind(request: &InputRequest) -> Result<McpMrtrInputKind, McpClientError> {
    match request {
        InputRequest::CreateMessage(_) => Ok(McpMrtrInputKind::Sampling),
        InputRequest::Elicitation(_) => Ok(McpMrtrInputKind::Elicitation),
        InputRequest::ListRoots(_) => Ok(McpMrtrInputKind::Roots),
        _ => Err(McpClientError::ContinuationResponseRejected),
    }
}

#[allow(deprecated)]
fn normalize_response(
    request: &InputRequest,
    value: Value,
    limits: &McpClientLimits,
) -> Result<(Value, usize), McpClientError> {
    let measurement = match measure_json(
        &value,
        limits.max_request_bytes,
        limits.max_json_depth,
        limits.max_json_nodes,
        "MRTR input response",
    ) {
        Ok(measurement) => measurement,
        Err(_) => {
            drop_json_value_iterative(value);
            return Err(McpClientError::ContinuationResponseRejected);
        },
    };
    if contains_reserved_meta(&value) {
        drop_json_value_iterative(value);
        return Err(McpClientError::ContinuationResponseRejected);
    }
    let encoded =
        serde_json::to_vec(&value).map_err(|_| McpClientError::ContinuationResponseRejected)?;
    let canonical = match request {
        InputRequest::CreateMessage(_) => {
            let response: CreateMessageResult = serde_json::from_slice(&encoded)
                .map_err(|_| McpClientError::ContinuationResponseRejected)?;
            if response.validate().is_err() || response.message.meta.is_some() {
                return Err(McpClientError::ContinuationResponseRejected);
            }
            serde_json::to_value(response)
        },
        InputRequest::Elicitation(request) => {
            let response: ElicitResult = serde_json::from_slice(&encoded)
                .map_err(|_| McpClientError::ContinuationResponseRejected)?;
            if response.meta.is_some() || !elicitation_response_matches(request, &response) {
                return Err(McpClientError::ContinuationResponseRejected);
            }
            serde_json::to_value(response)
        },
        InputRequest::ListRoots(_) => {
            let response: ListRootsResult = serde_json::from_slice(&encoded)
                .map_err(|_| McpClientError::ContinuationResponseRejected)?;
            if response.meta.is_some()
                || response
                    .roots
                    .iter()
                    .any(|root| root.meta.is_some() || url::Url::parse(&root.uri).is_err())
            {
                return Err(McpClientError::ContinuationResponseRejected);
            }
            serde_json::to_value(response)
        },
        _ => return Err(McpClientError::ContinuationResponseRejected),
    }
    .map_err(|_| McpClientError::ContinuationResponseRejected)?;
    if canonical != value {
        drop_json_value_iterative(canonical);
        drop_json_value_iterative(value);
        return Err(McpClientError::ContinuationResponseRejected);
    }
    drop_json_value_iterative(value);
    Ok((canonical, measurement.bytes))
}

fn contains_reserved_meta(root: &Value) -> bool {
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        match value {
            Value::Array(values) => stack.extend(values),
            Value::Object(values) => {
                if values.contains_key("_meta") {
                    return true;
                }
                stack.extend(values.values());
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    false
}

fn elicitation_response_matches(
    request: &rmcp::model::ElicitRequest,
    response: &ElicitResult,
) -> bool {
    match (&request.params, &response.action) {
        (
            ElicitRequestParams::FormElicitationParams {
                requested_schema, ..
            },
            ElicitationAction::Accept,
        ) => response
            .content
            .as_ref()
            .is_some_and(|content| elicitation_content_matches(requested_schema, content)),
        (ElicitRequestParams::UrlElicitationParams { .. }, ElicitationAction::Accept) => {
            response.content.is_none()
        },
        (_, ElicitationAction::Decline | ElicitationAction::Cancel) => response.content.is_none(),
        _ => false,
    }
}

fn elicitation_content_matches(schema: &rmcp::model::ElicitationSchema, value: &Value) -> bool {
    let Some(content) = value.as_object() else {
        return false;
    };
    if content
        .keys()
        .any(|key| !schema.properties.contains_key(key))
    {
        return false;
    }
    let mut required = HashSet::new();
    for key in schema.required.iter().flatten() {
        if !schema.properties.contains_key(key) || !required.insert(key.as_str()) {
            return false;
        }
        if !content.contains_key(key) {
            return false;
        }
    }
    content.iter().all(|(key, value)| {
        schema
            .properties
            .get(key)
            .is_some_and(|property| primitive_value_matches(property, value))
    })
}

fn primitive_value_matches(schema: &PrimitiveSchemaDefinition, value: &Value) -> bool {
    match schema {
        PrimitiveSchemaDefinition::String(schema) => value.as_str().is_some_and(|value| {
            let length = value.chars().count() as u64;
            schema
                .min_length
                .is_none_or(|minimum| length >= u64::from(minimum))
                && schema
                    .max_length
                    .is_none_or(|maximum| length <= u64::from(maximum))
        }),
        PrimitiveSchemaDefinition::Number(schema) => value.as_f64().is_some_and(|value| {
            schema.minimum.is_none_or(|minimum| value >= minimum)
                && schema.maximum.is_none_or(|maximum| value <= maximum)
        }),
        PrimitiveSchemaDefinition::Integer(schema) => value.as_i64().is_some_and(|value| {
            schema.minimum.is_none_or(|minimum| value >= minimum)
                && schema.maximum.is_none_or(|maximum| value <= maximum)
        }),
        PrimitiveSchemaDefinition::Boolean(_) => value.is_boolean(),
        PrimitiveSchemaDefinition::Enum(schema) => enum_value_matches(schema, value),
        _ => false,
    }
}

fn enum_value_matches(schema: &EnumSchema, value: &Value) -> bool {
    match schema {
        EnumSchema::Legacy(schema) => value
            .as_str()
            .is_some_and(|value| schema.enum_.iter().any(|candidate| candidate == value)),
        EnumSchema::Single(schema) => single_select_matches(schema, value),
        EnumSchema::Multi(schema) => multi_select_matches(schema, value),
        _ => false,
    }
}

fn single_select_matches(schema: &SingleSelectEnumSchema, value: &Value) -> bool {
    let Some(value) = value.as_str() else {
        return false;
    };
    match schema {
        SingleSelectEnumSchema::Untitled(schema) => {
            schema.enum_.iter().any(|candidate| candidate == value)
        },
        SingleSelectEnumSchema::Titled(schema) => schema
            .one_of
            .iter()
            .any(|candidate| candidate.const_ == value),
        _ => false,
    }
}

fn multi_select_matches(schema: &MultiSelectEnumSchema, value: &Value) -> bool {
    let Some(values) = value.as_array() else {
        return false;
    };
    let (minimum, maximum, candidates): (Option<u64>, Option<u64>, Vec<&str>) = match schema {
        MultiSelectEnumSchema::Untitled(schema) => (
            schema.min_items,
            schema.max_items,
            schema.items.enum_.iter().map(String::as_str).collect(),
        ),
        MultiSelectEnumSchema::Titled(schema) => (
            schema.min_items,
            schema.max_items,
            schema
                .items
                .any_of
                .iter()
                .map(|candidate| candidate.const_.as_str())
                .collect(),
        ),
        _ => return false,
    };
    let allowed = candidates.iter().copied().collect::<HashSet<_>>();
    if allowed.len() != candidates.len() {
        return false;
    }
    let count = values.len() as u64;
    if minimum.is_some_and(|minimum| count < minimum)
        || maximum.is_some_and(|maximum| count > maximum)
    {
        return false;
    }
    let mut selected = HashSet::with_capacity(values.len());
    values.iter().all(|value| {
        value
            .as_str()
            .is_some_and(|value| allowed.contains(value) && selected.insert(value))
    })
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroU64, time::Instant};

    use rmcp::model::{
        CreateMessageRequest, CreateMessageRequestParams, ElicitRequest, ElicitationSchema,
        EnumSchema, ListRootsRequest, Role, Root, SamplingMessage,
    };
    use serde_json::json;
    use static_assertions::assert_not_impl_any;

    use super::*;
    use crate::continuation::McpContinuationKind;

    assert_not_impl_any!(McpMrtrResponse: Clone, serde::Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(
        McpPreparedMrtrResponses: Clone,
        serde::Serialize,
        serde::de::DeserializeOwned
    );

    fn pending(client_instance_id: u64, sequence: u64, revision: u64) -> McpPendingCall {
        McpPendingCall {
            id: McpContinuationId {
                client_instance_id,
                sequence: NonZeroU64::new(sequence).unwrap(),
            },
            revision: McpContinuationRevision(NonZeroU64::new(revision).unwrap()),
            kind: McpContinuationKind::AdditionalInput,
            expires_at: Instant::now() + std::time::Duration::from_secs(60),
        }
    }

    fn elicitation_schema() -> ElicitationSchema {
        let single = EnumSchema::builder(vec!["alpha".to_owned(), "beta".to_owned()]).build();
        let multi = EnumSchema::builder(vec!["one".to_owned(), "two".to_owned()])
            .multiselect()
            .min_items(1)
            .unwrap()
            .max_items(2)
            .unwrap()
            .build();
        ElicitationSchema::builder()
            .required_string_with("name", |schema| schema.length(2, 8))
            .required_integer("age", 1, 120)
            .required_enum_schema("choice", single)
            .required_enum_schema("tags", multi)
            .optional_bool("active", false)
            .build()
            .unwrap()
    }

    fn sampling_request() -> InputRequest {
        InputRequest::CreateMessage(CreateMessageRequest::new(CreateMessageRequestParams::new(
            vec![SamplingMessage::user_text("answer")],
            32,
        )))
    }

    fn elicitation_request() -> InputRequest {
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Provide bounded fixture values".to_owned(),
                requested_schema: elicitation_schema(),
            },
        ))
    }

    fn roots_request() -> InputRequest {
        InputRequest::ListRoots(ListRootsRequest::default())
    }

    fn input_required(
        requests: impl IntoIterator<Item = (&'static str, InputRequest)>,
    ) -> InputRequiredResult {
        InputRequiredResult::new(
            Some(
                requests
                    .into_iter()
                    .map(|(key, request)| (key.to_owned(), request))
                    .collect::<BTreeMap<_, _>>(),
            ),
            Some("canary-secret-request-state".to_owned()),
        )
    }

    fn complete_input_required() -> InputRequiredResult {
        input_required([
            ("canary-elicitation-key", elicitation_request()),
            ("canary-roots-key", roots_request()),
            ("canary-sampling-key", sampling_request()),
        ])
    }

    fn valid_response(kind: McpMrtrInputKind) -> Value {
        match kind {
            McpMrtrInputKind::Sampling => serde_json::to_value(CreateMessageResult::new(
                SamplingMessage::assistant_text("canary-sampling-answer"),
                "canary-model".to_owned(),
            ))
            .unwrap(),
            McpMrtrInputKind::Elicitation => serde_json::to_value(
                ElicitResult::new(ElicitationAction::Accept).with_content(json!({
                    "name": "Ada",
                    "age": 37,
                    "choice": "alpha",
                    "tags": ["one", "two"],
                    "active": true
                })),
            )
            .unwrap(),
            McpMrtrInputKind::Roots => {
                serde_json::to_value(ListRootsResult::new(vec![Root::new("file:///canary/root")]))
                    .unwrap()
            },
        }
    }

    fn single_request(
        request: InputRequest,
    ) -> (McpPendingCall, InputRequiredResult, McpMrtrInputSlot) {
        let pending = pending(7, 11, 3);
        let result = input_required([("canary-private-key", request)]);
        let slot = project_input_slots(pending, &result, 64).unwrap()[0];
        (pending, result, slot)
    }

    #[test]
    fn slots_and_prepared_debug_are_payload_free_and_response_order_is_irrelevant() {
        let pending = pending(7, 11, 3);
        let result = complete_input_required();
        let slots = project_input_slots(pending, &result, 64).unwrap();
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0].kind(), McpMrtrInputKind::Elicitation);
        assert_eq!(slots[1].kind(), McpMrtrInputKind::Roots);
        assert_eq!(slots[2].kind(), McpMrtrInputKind::Sampling);

        let responses = slots
            .iter()
            .rev()
            .map(|slot| McpMrtrResponse::new(slot.id(), valid_response(slot.kind())))
            .collect();
        let prepared =
            prepare_input_responses(pending, &result, responses, &McpClientLimits::default())
                .unwrap();
        assert_eq!(prepared.pending(), pending);
        assert_eq!(prepared.response_count(), 3);
        assert_eq!(
            prepared
                .responses
                .as_ref()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "canary-elicitation-key",
                "canary-roots-key",
                "canary-sampling-key"
            ]
        );

        let slot_debug = format!("{slots:?}");
        let prepared_debug = format!("{prepared:?}");
        for secret in [
            "canary-elicitation-key",
            "canary-roots-key",
            "canary-sampling-key",
            "canary-secret-request-state",
            "canary-sampling-answer",
            "canary-model",
        ] {
            assert!(!slot_debug.contains(secret));
            assert!(!prepared_debug.contains(secret));
        }
    }

    #[test]
    fn state_only_round_prepares_no_synthetic_response() {
        let pending = pending(8, 1, 1);
        let result = InputRequiredResult::from_request_state("canary-state-only");
        assert!(project_input_slots(pending, &result, 64)
            .unwrap()
            .is_empty());
        let prepared =
            prepare_input_responses(pending, &result, Vec::new(), &McpClientLimits::default())
                .unwrap();
        assert_eq!(prepared.response_count(), 0);
    }

    #[test]
    fn response_set_must_be_complete_unique_and_bound_to_exact_revision() {
        let pending = pending(9, 2, 4);
        let result = complete_input_required();
        let slots = project_input_slots(pending, &result, 64).unwrap();

        assert!(matches!(
            prepare_input_responses(
                pending,
                &result,
                vec![McpMrtrResponse::new(
                    slots[0].id(),
                    valid_response(slots[0].kind())
                )],
                &McpClientLimits::default()
            ),
            Err(McpClientError::ContinuationResponseRejected)
        ));

        let duplicate = slots[0].id();
        let duplicate_responses = vec![
            McpMrtrResponse::new(duplicate, valid_response(slots[0].kind())),
            McpMrtrResponse::new(duplicate, valid_response(slots[0].kind())),
            McpMrtrResponse::new(slots[2].id(), valid_response(slots[2].kind())),
        ];
        assert!(matches!(
            prepare_input_responses(
                pending,
                &result,
                duplicate_responses,
                &McpClientLimits::default()
            ),
            Err(McpClientError::ContinuationResponseRejected)
        ));

        let wrong_revision = McpMrtrInputId {
            continuation_id: slots[0].id().continuation_id,
            revision: McpContinuationRevision(NonZeroU64::new(5).unwrap()),
            ordinal: slots[0].id().ordinal,
        };
        let cross_bound = vec![
            McpMrtrResponse::new(wrong_revision, valid_response(slots[0].kind())),
            McpMrtrResponse::new(slots[1].id(), valid_response(slots[1].kind())),
            McpMrtrResponse::new(slots[2].id(), valid_response(slots[2].kind())),
        ];
        assert!(matches!(
            prepare_input_responses(pending, &result, cross_bound, &McpClientLimits::default()),
            Err(McpClientError::ContinuationResponseRejected)
        ));
    }

    #[test]
    fn sampling_rejects_wrong_roles_extra_fields_and_metadata() {
        let (pending, result, slot) = single_request(sampling_request());
        let wrong_role = serde_json::to_value(CreateMessageResult::new(
            SamplingMessage::new(
                Role::User,
                rmcp::model::SamplingMessageContentBlock::text("no"),
            ),
            "model".to_owned(),
        ))
        .unwrap();
        for invalid in [
            wrong_role,
            json!({
                "model": "model",
                "role": "assistant",
                "content": {"type": "text", "text": "ok"},
                "unexpected": true
            }),
            json!({
                "model": "model",
                "role": "assistant",
                "content": {"type": "text", "text": "ok", "_meta": {"secret": true}}
            }),
        ] {
            assert!(matches!(
                prepare_input_responses(
                    pending,
                    &result,
                    vec![McpMrtrResponse::new(slot.id(), invalid)],
                    &McpClientLimits::default()
                ),
                Err(McpClientError::ContinuationResponseRejected)
            ));
        }
    }

    #[test]
    fn roots_require_valid_metadata_free_uris() {
        let (pending, result, slot) = single_request(roots_request());
        for invalid in [
            json!({"roots": [{"uri": "not a URI"}]}),
            json!({"roots": [{"uri": "file:///safe", "_meta": {"secret": true}}]}),
            json!({"roots": [], "unexpected": true}),
        ] {
            assert!(matches!(
                prepare_input_responses(
                    pending,
                    &result,
                    vec![McpMrtrResponse::new(slot.id(), invalid)],
                    &McpClientLimits::default()
                ),
                Err(McpClientError::ContinuationResponseRejected)
            ));
        }
    }

    #[test]
    fn elicitation_accept_content_must_match_the_exact_primitive_schema() {
        let (pending, result, slot) = single_request(elicitation_request());
        let invalid_content = [
            json!({"name": "Ada", "choice": "alpha", "tags": ["one"]}),
            json!({"name": "A", "age": 37, "choice": "alpha", "tags": ["one"]}),
            json!({"name": "Ada", "age": 121, "choice": "alpha", "tags": ["one"]}),
            json!({"name": "Ada", "age": 37, "choice": "gamma", "tags": ["one"]}),
            json!({"name": "Ada", "age": 37, "choice": "alpha", "tags": []}),
            json!({"name": "Ada", "age": 37, "choice": "alpha", "tags": ["one", "one"]}),
            json!({"name": "Ada", "age": 37, "choice": "alpha", "tags": ["one"], "extra": true}),
        ];
        for content in invalid_content {
            let invalid = serde_json::to_value(
                ElicitResult::new(ElicitationAction::Accept).with_content(content),
            )
            .unwrap();
            assert!(matches!(
                prepare_input_responses(
                    pending,
                    &result,
                    vec![McpMrtrResponse::new(slot.id(), invalid)],
                    &McpClientLimits::default()
                ),
                Err(McpClientError::ContinuationResponseRejected)
            ));
        }

        for action in [ElicitationAction::Decline, ElicitationAction::Cancel] {
            let valid = serde_json::to_value(ElicitResult::new(action)).unwrap();
            assert!(prepare_input_responses(
                pending,
                &result,
                vec![McpMrtrResponse::new(slot.id(), valid)],
                &McpClientLimits::default()
            )
            .is_ok());
        }
    }

    #[test]
    fn duplicate_multiselect_schema_choices_fail_closed() {
        let duplicate_multi = EnumSchema::builder(vec!["same".to_owned(), "same".to_owned()])
            .multiselect()
            .build();
        let request = InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Choose".to_owned(),
                requested_schema: ElicitationSchema::builder()
                    .required_enum_schema("tags", duplicate_multi)
                    .build()
                    .unwrap(),
            },
        ));
        let (pending, result, slot) = single_request(request);
        let response = serde_json::to_value(
            ElicitResult::new(ElicitationAction::Accept).with_content(json!({"tags": ["same"]})),
        )
        .unwrap();
        assert!(matches!(
            prepare_input_responses(
                pending,
                &result,
                vec![McpMrtrResponse::new(slot.id(), response)],
                &McpClientLimits::default()
            ),
            Err(McpClientError::ContinuationResponseRejected)
        ));
    }

    #[test]
    fn response_kind_cannot_be_substituted() {
        let (pending, result, slot) = single_request(sampling_request());
        let elicitation =
            serde_json::to_value(ElicitResult::new(ElicitationAction::Decline)).unwrap();
        assert!(matches!(
            prepare_input_responses(
                pending,
                &result,
                vec![McpMrtrResponse::new(slot.id(), elicitation)],
                &McpClientLimits::default()
            ),
            Err(McpClientError::ContinuationResponseRejected)
        ));
    }

    #[test]
    fn input_count_is_bounded_before_slot_projection() {
        let pending = pending(10, 1, 1);
        let result = input_required([("one", roots_request()), ("two", roots_request())]);
        assert!(matches!(
            project_input_slots(pending, &result, 1),
            Err(McpClientError::ContinuationResponseRejected)
        ));
    }

    #[test]
    fn deeply_nested_response_rejects_and_drops_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let (pending, result, slot) = single_request(sampling_request());
                let mut value = Value::Null;
                for _ in 0..20_000 {
                    value = Value::Array(vec![value]);
                }
                assert!(matches!(
                    prepare_input_responses(
                        pending,
                        &result,
                        vec![McpMrtrResponse::new(slot.id(), value)],
                        &McpClientLimits::default()
                    ),
                    Err(McpClientError::ContinuationResponseRejected)
                ));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
