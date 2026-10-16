//! Typed macOS physical owner for Apps.
//!
//! This adapter deliberately does not expose the legacy
//! `macos-ui-automation::call(action_name, args_json)` shape. A reviewed action
//! is joined to the common interactive/effect typestates, lowered to one closed
//! server-to-desktop wire action, and signed with the runtime/desktop pairing
//! key. Raw PID/window/AX indices stay private owner evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use magician_app_contract::macos_host::{
    app_macos_host_observation_contains_secure_content, app_macos_host_observation_content_tree,
    app_macos_host_observation_fence_input, app_macos_host_parse_element_token,
    app_macos_host_protected_bundle_id, app_macos_host_valid_snapshot_id, AppMacosHostAction,
    AppMacosHostActionClass, AppMacosHostKey, AppMacosHostModifier, AppMacosHostPermitClaims,
    AppMacosHostScrollDirection, SignedAppMacosHostRequest,
    APP_MACOS_HOST_MAX_SCREENSHOT_SCALE_MILLIS, APP_MACOS_HOST_MAX_SCROLL_AMOUNT,
    APP_MACOS_HOST_MIN_SCREENSHOT_SCALE_MILLIS, APP_MACOS_HOST_PROTECTED_POLICY_V1,
    APP_MACOS_HOST_WIRE_V2,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroize as _;

use super::{
    effect_kernel::{
        AppEffectBinding, AppEffectInFlight, AppEffectKernelError, AppEffectPhysicalOwner,
        AppEffectPhysicalTarget, AppEffectProviderIoAuthorization, AppEffectSettlement,
        AppEffectStage,
    },
    interactive::{
        AppInteractiveActionPermit, AppInteractiveBackgroundPosture, AppInteractiveCancellation,
        AppInteractiveCancellationReason, AppInteractiveCurrentFence,
        AppInteractiveEffectInspection, AppInteractiveEffectPermit, AppInteractiveExecutionProfile,
        AppInteractiveGeometry, AppInteractiveGrantDescriptor, AppInteractiveObservationKind,
        AppInteractiveObservationRef, AppInteractiveOwnerIoPermit, AppInteractiveResourceCeilings,
        AppInteractiveResourceClaim, AppInteractiveReviewedAction, AppInteractiveSessionHandle,
        AppInteractiveSettlementReceipt,
    },
    models::{AppDigest, AppReference},
    package_lock::{AppLockedPrimitiveActionBinding, AppLockedPrimitiveBinding},
    tool_disclosure::AttestedAppToolTarget,
};
use crate::magician_v2::{execution::actions::ActionResult, json_traversal::canonical_json_bytes};

pub(crate) const APP_MACOS_HOST_PROFILE_V1: &str = "magician.app-macos-host.v1";
const APP_MACOS_HOST_IMPLEMENTATION_REVISION: &str = "macos-host-owner.1";
const APP_MACOS_HOST_REQUEST_TTL_SECONDS: i64 = 10;
const MAX_APP_MACOS_TARGETS: usize = 64;
const MAX_APP_MACOS_OBSERVATION_ELEMENTS: usize = 8 * 1024;
const MAX_APP_MACOS_REVIEWED_OBSERVATION_ELEMENTS: u64 = MAX_APP_MACOS_OBSERVATION_ELEMENTS as u64;
const MAX_APP_MACOS_SEMANTIC_TEXT_BYTES: usize = 256;
const MAX_APP_MACOS_TREE_LINE_BYTES: usize = 4 * 1024;
const MAX_APP_MACOS_TREE_DEPTH: u16 = 64;
const MAX_APP_MACOS_CANONICAL_INPUT_BYTES: usize = 256 * 1024;
const MAX_APP_MACOS_HOST_RESPONSE_BYTES: usize = 32 * 1024 * 1024 + 4 * 1024;
const MAX_APP_MACOS_OBSERVATION_BYTES: usize = 16 * 1024 * 1024;
const APP_MACOS_HOST_TRANSPORT_TIMEOUT_SECONDS: u64 = 70;
pub const APP_MACOS_OBSERVE_RESULT_CEILING: u64 = 16 * 1024 * 1024;
pub const APP_MACOS_ACTION_RESULT_CEILING: u64 = 32 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosOperation {
    Launch,
    Focus,
    Observe,
    CapturePixels,
    ClickElement,
    TypeText,
    PressKey,
    ScrollElement,
    DragElements,
}

impl AppMacosOperation {
    pub(crate) fn action_name(self) -> Option<&'static str> {
        match self {
            Self::Launch => Some("launch"),
            Self::Focus => Some("focus"),
            Self::Observe => Some("snapshot"),
            Self::ClickElement => Some("click"),
            Self::TypeText => Some("type"),
            Self::PressKey => Some("key"),
            Self::ScrollElement => Some("scroll"),
            Self::DragElements => Some("drag"),
            Self::CapturePixels => None,
        }
    }

    fn wire_class(self) -> AppMacosHostActionClass {
        match self {
            Self::Launch | Self::Focus => AppMacosHostActionClass::NavigateOrLaunch,
            Self::Observe => AppMacosHostActionClass::Observe,
            Self::CapturePixels => AppMacosHostActionClass::CapturePixels,
            Self::TypeText | Self::ScrollElement => AppMacosHostActionClass::Interact,
            Self::ClickElement | Self::PressKey | Self::DragElements => {
                AppMacosHostActionClass::OutwardCommit
            },
        }
    }

    fn interactive_class(self) -> super::interactive::AppInteractiveActionClass {
        match self {
            Self::Launch | Self::Focus => {
                super::interactive::AppInteractiveActionClass::NavigateOrLaunch
            },
            Self::Observe => super::interactive::AppInteractiveActionClass::Observe,
            Self::CapturePixels => super::interactive::AppInteractiveActionClass::CapturePixels,
            Self::TypeText | Self::ScrollElement => {
                super::interactive::AppInteractiveActionClass::Interact
            },
            Self::ClickElement | Self::PressKey | Self::DragElements => {
                super::interactive::AppInteractiveActionClass::OutwardCommit
            },
        }
    }

    fn required_observation_kind(self) -> Option<AppInteractiveObservationKind> {
        matches!(
            self,
            Self::ClickElement
                | Self::TypeText
                | Self::PressKey
                | Self::ScrollElement
                | Self::DragElements
        )
        .then_some(AppInteractiveObservationKind::StructuredTree)
    }

    fn requires_observation(self) -> bool {
        self.required_observation_kind().is_some()
    }

    fn invalidates_observation(self) -> bool {
        !matches!(self, Self::Observe | Self::CapturePixels)
    }
}

pub fn macos_operation(action_name: &str) -> Option<AppMacosOperation> {
    match action_name {
        "launch" => Some(AppMacosOperation::Launch),
        "focus" => Some(AppMacosOperation::Focus),
        "snapshot" => Some(AppMacosOperation::Observe),
        "click" => Some(AppMacosOperation::ClickElement),
        "type" => Some(AppMacosOperation::TypeText),
        "key" => Some(AppMacosOperation::PressKey),
        "scroll" => Some(AppMacosOperation::ScrollElement),
        "drag" => Some(AppMacosOperation::DragElements),
        _ => None,
    }
}

pub(crate) fn macos_observe_input_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "operation": { "const": "observe" }
        },
        "required": ["operation"],
        "additionalProperties": false
    })
}

pub fn macos_observe_result_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "result": { "const": "structured_observation" },
            "schema": { "const": "magician.app-macos-result.v1" },
            "operation": { "const": "observe" },
            "target_ref": { "type": "string", "maxLength": 192 },
            "tcc_policy_digest": { "type": "string", "maxLength": 71 },
            "tcc_epoch": { "type": "integer", "minimum": 1 },
            "evidence_digest": { "type": "string", "maxLength": 71 },
            "observation_ref": { "type": "string", "maxLength": 192 },
            "geometry": {
                "type": "object",
                "properties": {
                    "width": { "type": "integer", "minimum": 1 },
                    "height": { "type": "integer", "minimum": 1 },
                    "scale_millis": { "const": 1000 }
                },
                "required": ["width", "height", "scale_millis"],
                "additionalProperties": false
            },
            "nodes": {
                "type": "array",
                "maxItems": MAX_APP_MACOS_OBSERVATION_ELEMENTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "element_ref": { "type": "string", "maxLength": 192 },
                        "role": { "type": "string", "maxLength": 64 },
                        "label": { "type": "string", "maxLength": MAX_APP_MACOS_SEMANTIC_TEXT_BYTES },
                        "title": { "type": "string", "maxLength": MAX_APP_MACOS_SEMANTIC_TEXT_BYTES },
                        "value": { "type": "string", "maxLength": MAX_APP_MACOS_SEMANTIC_TEXT_BYTES },
                        "enabled": { "type": "boolean" },
                        "selected": { "type": "boolean" },
                        "depth": { "type": "integer", "minimum": 0, "maximum": MAX_APP_MACOS_TREE_DEPTH }
                    },
                    "required": ["element_ref", "role", "depth"],
                    "additionalProperties": false
                }
            }
        },
        "required": [
            "result", "schema", "operation", "target_ref",
            "tcc_policy_digest", "tcc_epoch", "evidence_digest", "observation_ref", "geometry", "nodes"
        ],
        "additionalProperties": false
    })
}

pub fn macos_action_input_schema(action_name: &str) -> Option<Value> {
    let reference = serde_json::json!({"type":"string","maxLength":192});
    match action_name {
        "snapshot" => Some(macos_observe_input_schema()),
        "launch" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,"required":["operation"],
            "properties":{"operation":{"const":"launch"}}
        })),
        "focus" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,"required":["operation"],
            "properties":{"operation":{"const":"focus"}}
        })),
        "click" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["operation","observation_ref","element_ref","click_count"],
            "properties":{
                "operation":{"const":"click_element"},
                "observation_ref":reference,
                "element_ref":{"type":"string","maxLength":192},
                "click_count":{"type":"integer","minimum":1,"maximum":2}
            }
        })),
        "type" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["operation","observation_ref","element_ref","text"],
            "properties":{
                "operation":{"const":"type_text"},
                "observation_ref":reference,
                "element_ref":{"type":"string","maxLength":192},
                "text":{"type":"string","minLength":1,"maxLength":16384}
            }
        })),
        "key" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["operation","observation_ref","key","modifiers"],
            "properties":{
                "operation":{"const":"press_key"},
                "observation_ref":reference,
                "key":{"enum":["return","tab","escape","space","backspace","delete_forward","arrow_up","arrow_down","arrow_left","arrow_right","home","end","page_up","page_down"]},
                "modifiers":{"type":"array","maxItems":4,"uniqueItems":true,"items":{"enum":["command","option","control","shift"]}}
            }
        })),
        "scroll" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["operation","observation_ref","element_ref","direction","amount"],
            "properties":{
                "operation":{"const":"scroll_element"},
                "observation_ref":reference,
                "element_ref":{"type":"string","maxLength":192},
                "direction":{"enum":["up","down","left","right"]},
                "amount":{"type":"integer","minimum":1,"maximum":APP_MACOS_HOST_MAX_SCROLL_AMOUNT}
            }
        })),
        "drag" => Some(serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["operation","observation_ref","source_element_ref","destination_element_ref"],
            "properties":{
                "operation":{"const":"drag_elements"},
                "observation_ref":reference,
                "source_element_ref":{"type":"string","maxLength":192},
                "destination_element_ref":{"type":"string","maxLength":192}
            }
        })),
        _ => None,
    }
}

pub fn macos_action_result_schema() -> Value {
    serde_json::json!({
        "type":"object","additionalProperties":false,
        "required":["result","schema","operation","target_ref","tcc_policy_digest","tcc_epoch","outcome"],
        "properties":{
            "result":{"const":"action"},
            "schema":{"const":"magician.app-macos-result.v1"},
            "operation":{"enum":["launch","focus","click_element","type_text","press_key","scroll_element","drag_elements"]},
            "target_ref":{"type":"string","maxLength":192},
            "tcc_policy_digest":{"type":"string","maxLength":71},
            "tcc_epoch":{"type":"integer","minimum":1},
            "outcome":{"const":"completed"}
        }
    })
}

#[allow(dead_code)] // Snapshot-only package compatibility.
pub(crate) fn macos_observe_resource_ceilings(
    max_duration_seconds: u64,
) -> Result<AppInteractiveResourceCeilings, AppMacosHostError> {
    AppInteractiveResourceCeilings::reviewed(
        1,
        1,
        max_duration_seconds.min(300),
        MAX_APP_MACOS_OBSERVATION_BYTES as u64,
        MAX_APP_MACOS_REVIEWED_OBSERVATION_ELEMENTS,
        0,
        0,
        APP_MACOS_OBSERVE_RESULT_CEILING,
    )
    .map_err(AppMacosHostError::Interactive)
}

#[allow(dead_code)] // Snapshot-only reviewed package compatibility.
pub(crate) fn reviewed_macos_observe_action(
    action_ref: AppReference,
    owner_implementation_digest: AppDigest,
) -> Result<AppInteractiveReviewedAction, AppMacosHostError> {
    AppInteractiveReviewedAction::reviewed(
        action_ref,
        super::interactive::AppInteractiveActionClass::Observe,
        AppDigest::blake3_canonical_json(&macos_observe_input_schema())
            .map_err(|_| AppMacosHostError::Encoding)?,
        AppDigest::blake3_canonical_json(&macos_observe_result_schema())
            .map_err(|_| AppMacosHostError::Encoding)?,
        owner_implementation_digest,
        APP_MACOS_OBSERVE_RESULT_CEILING,
        None,
        false,
    )
    .map_err(AppMacosHostError::Interactive)
}

pub(crate) fn reviewed_macos_action(
    action_ref: AppReference,
    owner_implementation_digest: AppDigest,
    operation: AppMacosOperation,
) -> Result<AppInteractiveReviewedAction, AppMacosHostError> {
    let name = match operation {
        AppMacosOperation::Launch => "launch",
        AppMacosOperation::Focus => "focus",
        AppMacosOperation::Observe => "snapshot",
        AppMacosOperation::ClickElement => "click",
        AppMacosOperation::TypeText => "type",
        AppMacosOperation::PressKey => "key",
        AppMacosOperation::ScrollElement => "scroll",
        AppMacosOperation::DragElements => "drag",
        AppMacosOperation::CapturePixels => return Err(AppMacosHostError::ActionNotReviewed),
    };
    let input = macos_action_input_schema(name).ok_or(AppMacosHostError::ActionNotReviewed)?;
    let result = if operation == AppMacosOperation::Observe {
        macos_observe_result_schema()
    } else {
        macos_action_result_schema()
    };
    AppInteractiveReviewedAction::reviewed(
        action_ref,
        operation.interactive_class(),
        AppDigest::blake3_canonical_json(&input).map_err(|_| AppMacosHostError::Encoding)?,
        AppDigest::blake3_canonical_json(&result).map_err(|_| AppMacosHostError::Encoding)?,
        owner_implementation_digest,
        if operation == AppMacosOperation::Observe {
            APP_MACOS_OBSERVE_RESULT_CEILING
        } else {
            APP_MACOS_ACTION_RESULT_CEILING
        },
        operation.required_observation_kind(),
        operation.invalidates_observation(),
    )
    .map_err(AppMacosHostError::Interactive)
}

/// Closed canonical app input. It contains logical target/element references,
/// never PID/window/AX indices or an owner-selectable CUA verb.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum AppMacosCanonicalInput {
    Launch {},
    Focus {},
    Observe {},
    CapturePixels {},
    ClickElement {
        observation_ref: AppReference,
        element_ref: AppReference,
        click_count: u8,
    },
    TypeText {
        observation_ref: AppReference,
        element_ref: AppReference,
        text: String,
    },
    PressKey {
        observation_ref: AppReference,
        key: AppMacosHostKey,
        modifiers: Vec<AppMacosHostModifier>,
    },
    ScrollElement {
        observation_ref: AppReference,
        element_ref: AppReference,
        direction: AppMacosHostScrollDirection,
        amount: u8,
    },
    DragElements {
        observation_ref: AppReference,
        source_element_ref: AppReference,
        destination_element_ref: AppReference,
    },
}

impl AppMacosCanonicalInput {
    fn operation(&self) -> AppMacosOperation {
        match self {
            Self::Launch {} => AppMacosOperation::Launch,
            Self::Focus {} => AppMacosOperation::Focus,
            Self::Observe {} => AppMacosOperation::Observe,
            Self::CapturePixels {} => AppMacosOperation::CapturePixels,
            Self::ClickElement { .. } => AppMacosOperation::ClickElement,
            Self::TypeText { .. } => AppMacosOperation::TypeText,
            Self::PressKey { .. } => AppMacosOperation::PressKey,
            Self::ScrollElement { .. } => AppMacosOperation::ScrollElement,
            Self::DragElements { .. } => AppMacosOperation::DragElements,
        }
    }
}

pub(crate) fn canonical_macos_action_input(
    input: Value,
) -> Result<(AppMacosOperation, Vec<u8>), AppMacosHostError> {
    let parsed: AppMacosCanonicalInput =
        serde_json::from_value(input).map_err(|_| AppMacosHostError::InvalidCanonicalInput)?;
    let operation = parsed.operation();
    let canonical = canonical_json_bytes(
        &serde_json::to_value(parsed).map_err(|_| AppMacosHostError::Encoding)?,
    )
    .map_err(|_| AppMacosHostError::Encoding)?;
    if canonical.is_empty() || canonical.len() > MAX_APP_MACOS_CANONICAL_INPUT_BYTES {
        return Err(AppMacosHostError::InvalidCanonicalInput);
    }
    Ok((operation, canonical))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosReviewedTarget {
    target_ref: AppReference,
    bundle_id: String,
    application_identity_digest: AppDigest,
}

impl AppMacosReviewedTarget {
    /// Called only with the result of the native application-identity owner.
    pub(crate) fn from_owner_review(
        target_ref: AppReference,
        bundle_id: String,
        application_identity_digest: AppDigest,
    ) -> Result<Self, AppMacosHostError> {
        validate_bundle_id(&bundle_id)?;
        if app_macos_host_protected_bundle_id(&bundle_id) {
            return Err(AppMacosHostError::ProtectedTarget);
        }
        Ok(Self {
            target_ref,
            bundle_id,
            application_identity_digest,
        })
    }

    pub fn target_ref(&self) -> &AppReference {
        &self.target_ref
    }

    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    pub fn application_identity_digest(&self) -> &AppDigest {
        &self.application_identity_digest
    }
}

/// Exact finite bundle/application set reviewed at install time. Protected
/// applications cannot be represented in the current direct-owner profile.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosTargetPolicy {
    schema: &'static str,
    protected_policy_digest: AppDigest,
    targets: BTreeMap<AppReference, AppMacosReviewedTarget>,
    policy_digest: AppDigest,
}

impl AppMacosTargetPolicy {
    pub(crate) fn from_owner_review(
        protected_policy_digest: AppDigest,
        reviewed_targets: Vec<AppMacosReviewedTarget>,
    ) -> Result<Self, AppMacosHostError> {
        if protected_policy_digest
            != AppDigest::blake3(APP_MACOS_HOST_PROTECTED_POLICY_V1.as_bytes())
            || reviewed_targets.is_empty()
            || reviewed_targets.len() > MAX_APP_MACOS_TARGETS
        {
            return Err(AppMacosHostError::InvalidTargetPolicy);
        }
        let mut targets = BTreeMap::new();
        let mut bundles = BTreeSet::new();
        for target in reviewed_targets {
            if app_macos_host_protected_bundle_id(&target.bundle_id)
                || !bundles.insert(target.bundle_id.to_ascii_lowercase())
                || targets.insert(target.target_ref.clone(), target).is_some()
            {
                return Err(AppMacosHostError::InvalidTargetPolicy);
            }
        }
        let mut value = Self {
            schema: APP_MACOS_HOST_PROFILE_V1,
            protected_policy_digest,
            targets,
            policy_digest: AppDigest::blake3(b"pending-macos-target-policy"),
        };
        value.policy_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value((
                &value.schema,
                &value.protected_policy_digest,
                &value.targets,
            ))
            .map_err(|_| AppMacosHostError::Encoding)?,
        )
        .map_err(|_| AppMacosHostError::Encoding)?;
        Ok(value)
    }

    pub fn policy_digest(&self) -> &AppDigest {
        &self.policy_digest
    }

    pub fn target(&self, target_ref: &AppReference) -> Option<&AppMacosReviewedTarget> {
        self.targets.get(target_ref)
    }
}

/// Current physical resolution retained only by the macOS owner. Apps see the
/// logical target and opaque observation/element refs, never these IDs.
pub(crate) struct AppMacosResolvedTarget {
    reviewed: AppMacosReviewedTarget,
    host_identity_digest: AppDigest,
    process_id: Option<u32>,
    window_id: Option<u32>,
    physical_identity_digest: AppDigest,
}

impl AppMacosResolvedTarget {
    pub(crate) fn from_owner(
        reviewed: AppMacosReviewedTarget,
        pairing: &AppMacosHostPairing,
        process_id: Option<u32>,
        window_id: Option<u32>,
    ) -> Result<Self, AppMacosHostError> {
        if process_id.is_some() != window_id.is_some()
            || process_id.is_some_and(|value| value == 0)
            || window_id.is_some_and(|value| value == 0)
        {
            return Err(AppMacosHostError::InvalidPhysicalTarget);
        }
        let host_identity_digest = pairing.host_identity_digest.clone();
        let owner_profile_digest = macos_host_owner_profile_digest(pairing)?;
        let owner_implementation_digest =
            macos_host_owner_implementation_digest(&pairing.cua_driver_binary_digest)?;
        let physical_identity_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "profile": APP_MACOS_HOST_PROFILE_V1,
            "target_ref": reviewed.target_ref,
            "bundle_id": reviewed.bundle_id,
            "application_identity_digest": reviewed.application_identity_digest,
            "host_identity_digest": host_identity_digest,
            "owner_profile_digest": owner_profile_digest,
            "owner_implementation_digest": owner_implementation_digest,
            "process_id": process_id,
            "window_id": window_id,
        }))
        .map_err(|_| AppMacosHostError::Encoding)?;
        Ok(Self {
            reviewed,
            host_identity_digest,
            process_id,
            window_id,
            physical_identity_digest,
        })
    }
}

/// One CUA element as the physical owner observed it: the snapshot-scoped
/// `element_token` (`s<snapshot>:<index>`) CuaDriver 0.28 requires for
/// element-addressed actions. Never enters an app-visible projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppMacosObservedElement {
    element_index: u32,
    element_token: String,
}

/// Private mapping retained by the physical owner after projecting an AX
/// observation. App input carries only the opaque refs; physical element
/// tokens are resolved here immediately before the signed host request is
/// prepared.
pub(crate) struct AppMacosOwnerObservation {
    owner_target_digest: AppDigest,
    observation_ref: AppReference,
    process_id: u32,
    window_id: u32,
    /// The observed `tree_markdown`, from which each element action's permit
    /// fence (its target's ancestor identities and own line) is derived for
    /// exactly the element(s) it names. Kept whole rather than as one fence per
    /// element: a drag fences two elements as one joined input, and per-element
    /// ancestor chains would repeat every ancestor once per descendant.
    fence_tree: String,
    evidence_bytes: u64,
    screenshot_scale_millis: Option<u16>,
    element_tokens: BTreeMap<AppReference, String>,
}

impl AppMacosOwnerObservation {
    fn from_owner_projection(
        owner: &AppMacosHostOwner,
        observation_ref: AppReference,
        fence_tree: String,
        evidence_bytes: u64,
        screenshot_scale_millis: Option<u16>,
        elements: Vec<(AppReference, AppMacosObservedElement)>,
    ) -> Result<Self, AppMacosHostError> {
        let process_id = owner
            .target
            .process_id
            .ok_or(AppMacosHostError::InvalidPhysicalTarget)?;
        let window_id = owner
            .target
            .window_id
            .ok_or(AppMacosHostError::InvalidPhysicalTarget)?;
        if elements.len() > MAX_APP_MACOS_OBSERVATION_ELEMENTS {
            return Err(AppMacosHostError::InvalidPhysicalTarget);
        }
        let mut element_tokens = BTreeMap::new();
        let mut physical_indices = BTreeSet::new();
        let mut snapshot_ids = BTreeSet::new();
        for (element_ref, element) in elements {
            let Some((snapshot_id, token_index)) =
                app_macos_host_parse_element_token(&element.element_token)
            else {
                return Err(AppMacosHostError::InvalidPhysicalTarget);
            };
            snapshot_ids.insert(snapshot_id.to_owned());
            if token_index != element.element_index
                || snapshot_ids.len() > 1
                || !physical_indices.insert(element.element_index)
                || element_tokens
                    .insert(element_ref, element.element_token)
                    .is_some()
            {
                return Err(AppMacosHostError::InvalidPhysicalTarget);
            }
        }
        let scale_bounds =
            APP_MACOS_HOST_MIN_SCREENSHOT_SCALE_MILLIS..=APP_MACOS_HOST_MAX_SCREENSHOT_SCALE_MILLIS;
        if screenshot_scale_millis.is_some_and(|scale| !scale_bounds.contains(&scale)) {
            return Err(AppMacosHostError::InvalidPhysicalTarget);
        }
        Ok(Self {
            owner_target_digest: owner.target.physical_identity_digest.clone(),
            observation_ref,
            process_id,
            window_id,
            fence_tree,
            evidence_bytes,
            screenshot_scale_millis,
            element_tokens,
        })
    }

    fn validate_for(
        &self,
        owner: &AppMacosHostOwner,
        observation_ref: &AppReference,
    ) -> Result<(), AppMacosHostError> {
        if &self.owner_target_digest != owner.owner_target_digest()
            || &self.observation_ref != observation_ref
            || Some(self.process_id) != owner.target.process_id
            || Some(self.window_id) != owner.target.window_id
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        Ok(())
    }

    fn element_token(&self, element_ref: &AppReference) -> Result<String, AppMacosHostError> {
        self.element_tokens
            .get(element_ref)
            .cloned()
            .ok_or(AppMacosHostError::IdentityMismatch)
    }

    /// The desktop fence digest for `action`, computed from this observation.
    fn fence_digest(&self, action: &AppMacosHostAction) -> Result<AppDigest, AppMacosHostError> {
        app_macos_owner_fence_digest(&self.fence_tree, action)
    }
}

/// The `observation_content_digest` an observed action's permit carries: the
/// blake3 of the contract fence over the element(s) the action names (both
/// drag endpoints, in order), or of the window fence for a key press, which
/// names no element. An element without a fence (menu chrome, absent or
/// duplicated) fails closed here instead of reaching the desktop unfenced.
fn app_macos_owner_fence_digest(
    tree: &str,
    action: &AppMacosHostAction,
) -> Result<AppDigest, AppMacosHostError> {
    let index = |token: &str| {
        app_macos_host_parse_element_token(token)
            .map(|(_, index)| index)
            .ok_or(AppMacosHostError::InvalidPhysicalTarget)
    };
    let indexes = match action {
        AppMacosHostAction::ClickElement { element_token, .. }
        | AppMacosHostAction::TypeText { element_token, .. }
        | AppMacosHostAction::ScrollElement { element_token, .. } => vec![index(element_token)?],
        AppMacosHostAction::DragElements {
            source_element_token,
            destination_element_token,
            ..
        } => vec![
            index(source_element_token)?,
            index(destination_element_token)?,
        ],
        AppMacosHostAction::PressKey { .. } => Vec::new(),
        AppMacosHostAction::Launch { .. }
        | AppMacosHostAction::Focus { .. }
        | AppMacosHostAction::Observe { .. }
        | AppMacosHostAction::ObserveApplication { .. }
        | AppMacosHostAction::CapturePixels { .. } => {
            return Err(AppMacosHostError::ObservationRequired);
        },
    };
    app_macos_host_observation_fence_input(tree, &indexes)
        .map(|input| AppDigest::blake3(input.as_bytes()))
        .ok_or(AppMacosHostError::InvalidPhysicalTarget)
}

#[derive(Debug, Clone)]
struct AppMacosActionPlan {
    operation: AppMacosOperation,
    reviewed: AppInteractiveReviewedAction,
    implementation_plan_digest: AppDigest,
}

/// Exact reviewed physical owner for one resolved application/window target.
pub(crate) struct AppMacosHostOwner {
    tool_ref: AppReference,
    primitive_ref: AppReference,
    source_digest: AppDigest,
    target_policy_digest: AppDigest,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    cua_driver_binary_digest: AppDigest,
    target: AppMacosResolvedTarget,
    actions: BTreeMap<AppReference, AppMacosActionPlan>,
}

impl AppMacosHostOwner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_reviewed(
        tool_ref: AppReference,
        primitive_ref: AppReference,
        source_digest: AppDigest,
        target_policy: &AppMacosTargetPolicy,
        target: AppMacosResolvedTarget,
        grant: &AppInteractiveGrantDescriptor,
        pairing: &AppMacosHostPairing,
        transport: &AppMacosHttpHostTransport,
        reviewed_operations: Vec<(AppMacosOperation, AppReference)>,
    ) -> Result<Self, AppMacosHostError> {
        let expected_owner_profile_digest = macos_host_owner_profile_digest(pairing)?;
        let exact_tool = tool_ref.as_str() == "capability:macos-ui-automation"
            || reviewed_operations.len() == 1
                && reviewed_operations[0].0.action_name().is_some_and(|name| {
                    tool_ref.as_str() == format!("capability:macos-ui-automation__{name}")
                });
        if !exact_tool
            || target_policy.target(&target.reviewed.target_ref) != Some(&target.reviewed)
            || grant.profile() != AppInteractiveExecutionProfile::MacosHost
            || grant.background() != AppInteractiveBackgroundPosture::DirectOwner
            || grant.target_policy_digest() != target_policy.policy_digest()
            || grant.owner_profile_digest() != &expected_owner_profile_digest
            || pairing.validate_transport(transport).is_err()
            || target.host_identity_digest != pairing.host_identity_digest
            || reviewed_operations.is_empty()
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        let owner_implementation_digest =
            macos_host_owner_implementation_digest(&pairing.cua_driver_binary_digest)?;
        if grant.owner_implementation_digest() != &owner_implementation_digest {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        let owner_profile_digest = expected_owner_profile_digest;
        let mut actions = BTreeMap::new();
        for (operation, action_ref) in reviewed_operations {
            let reviewed = grant
                .actions()
                .get(&action_ref)
                .ok_or(AppMacosHostError::ActionNotReviewed)?
                .clone();
            if reviewed.class() != operation.interactive_class()
                || reviewed.required_observation_kind() != operation.required_observation_kind()
                || reviewed.invalidates_observation() != operation.invalidates_observation()
                || reviewed.owner_implementation_digest() != &owner_implementation_digest
            {
                return Err(AppMacosHostError::IdentityMismatch);
            }
            let implementation_plan_digest = macos_action_implementation_plan_digest(
                &source_digest,
                target_policy.policy_digest(),
                &owner_profile_digest,
                &owner_implementation_digest,
                &reviewed,
                operation,
            )?;
            let action_ref = reviewed.action_ref().clone();
            if actions
                .insert(
                    action_ref,
                    AppMacosActionPlan {
                        operation,
                        reviewed,
                        implementation_plan_digest,
                    },
                )
                .is_some()
            {
                return Err(AppMacosHostError::IdentityMismatch);
            }
        }
        Ok(Self {
            tool_ref,
            primitive_ref,
            source_digest,
            target_policy_digest: target_policy.policy_digest().clone(),
            owner_profile_digest,
            owner_implementation_digest,
            cua_driver_binary_digest: pairing.cua_driver_binary_digest.clone(),
            target,
            actions,
        })
    }

    pub(crate) fn rebind_exact_leaf(
        &mut self,
        tool_ref: AppReference,
        primitive_ref: AppReference,
        source_digest: AppDigest,
        grant: &AppInteractiveGrantDescriptor,
        operation: AppMacosOperation,
        action_ref: &AppReference,
    ) -> Result<(), AppMacosHostError> {
        let action_name = operation
            .action_name()
            .ok_or(AppMacosHostError::ActionNotReviewed)?;
        if !matches!(tool_ref.as_str(), "capability:macos-ui-automation")
            && tool_ref.as_str() != format!("capability:macos-ui-automation__{action_name}")
            || grant.profile() != AppInteractiveExecutionProfile::MacosHost
            || grant.target_policy_digest() != &self.target_policy_digest
            || grant.owner_profile_digest() != &self.owner_profile_digest
            || grant.owner_implementation_digest() != &self.owner_implementation_digest
            || grant.actions().len() != 1
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        let reviewed = grant
            .actions()
            .get(action_ref)
            .ok_or(AppMacosHostError::ActionNotReviewed)?
            .clone();
        if reviewed.class() != operation.interactive_class()
            || reviewed.required_observation_kind() != operation.required_observation_kind()
            || reviewed.invalidates_observation() != operation.invalidates_observation()
            || reviewed.owner_implementation_digest() != &self.owner_implementation_digest
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        let implementation_plan_digest = macos_action_implementation_plan_digest(
            &source_digest,
            &self.target_policy_digest,
            &self.owner_profile_digest,
            &self.owner_implementation_digest,
            &reviewed,
            operation,
        )?;
        self.tool_ref = tool_ref;
        self.primitive_ref = primitive_ref;
        self.source_digest = source_digest;
        self.actions.clear();
        self.actions.insert(
            action_ref.clone(),
            AppMacosActionPlan {
                operation,
                reviewed,
                implementation_plan_digest,
            },
        );
        Ok(())
    }

    pub(crate) fn owner_target_ref(&self) -> Result<AppReference, AppMacosHostError> {
        let digest = self
            .target
            .physical_identity_digest
            .as_str()
            .strip_prefix("blake3:")
            .ok_or(AppMacosHostError::Encoding)?;
        AppReference::parse(format!("runtime:app-macos-host:v1:{digest}"))
            .map_err(|_| AppMacosHostError::Encoding)
    }

    pub(crate) fn owner_target_digest(&self) -> &AppDigest {
        &self.target.physical_identity_digest
    }

    pub(crate) fn operation_for_action(
        &self,
        action_ref: &AppReference,
    ) -> Option<AppMacosOperation> {
        self.actions.get(action_ref).map(|plan| plan.operation)
    }

    pub(crate) fn prepare_action(
        &self,
        action_ref: &AppReference,
        canonical_input: &[u8],
        observation: Option<&AppMacosOwnerObservation>,
    ) -> Result<AppMacosPreparedAction, AppMacosHostError> {
        let plan = self
            .actions
            .get(action_ref)
            .ok_or(AppMacosHostError::ActionNotReviewed)?;
        if canonical_input.is_empty() || canonical_input.len() > MAX_APP_MACOS_CANONICAL_INPUT_BYTES
        {
            return Err(AppMacosHostError::InvalidCanonicalInput);
        }
        let input: AppMacosCanonicalInput = serde_json::from_slice(canonical_input)
            .map_err(|_| AppMacosHostError::InvalidCanonicalInput)?;
        let bundle_id = self.target.reviewed.bundle_id.clone();
        let physical_window = || {
            self.target
                .process_id
                .zip(self.target.window_id)
                .ok_or(AppMacosHostError::InvalidPhysicalTarget)
        };
        let require_observation = |observation_ref: &AppReference| {
            let observation = observation.ok_or(AppMacosHostError::ObservationRequired)?;
            observation.validate_for(self, observation_ref)?;
            Ok::<_, AppMacosHostError>(observation)
        };
        let wire_action = match (plan.operation, input) {
            (AppMacosOperation::Launch, AppMacosCanonicalInput::Launch {}) => {
                AppMacosHostAction::Launch { bundle_id }
            },
            (AppMacosOperation::Focus, AppMacosCanonicalInput::Focus {}) => {
                let (process_id, _) = physical_window()?;
                AppMacosHostAction::Focus {
                    bundle_id,
                    process_id,
                }
            },
            (AppMacosOperation::Observe, AppMacosCanonicalInput::Observe {}) => {
                match self.target.process_id.zip(self.target.window_id) {
                    Some((process_id, window_id)) => AppMacosHostAction::Observe {
                        bundle_id,
                        process_id,
                        window_id,
                    },
                    None => AppMacosHostAction::ObserveApplication { bundle_id },
                }
            },
            (AppMacosOperation::CapturePixels, AppMacosCanonicalInput::CapturePixels {}) => {
                return Err(AppMacosHostError::PhysicalOperationUnavailable);
            },
            (
                AppMacosOperation::ClickElement,
                AppMacosCanonicalInput::ClickElement {
                    observation_ref,
                    element_ref,
                    click_count,
                },
            ) => {
                if !(1..=2).contains(&click_count) {
                    return Err(AppMacosHostError::InvalidCanonicalInput);
                }
                let observation = require_observation(&observation_ref)?;
                AppMacosHostAction::ClickElement {
                    bundle_id,
                    process_id: observation.process_id,
                    window_id: observation.window_id,
                    observation_ref: observation_ref.to_string(),
                    element_token: observation.element_token(&element_ref)?,
                    click_count,
                }
            },
            (
                AppMacosOperation::TypeText,
                AppMacosCanonicalInput::TypeText {
                    observation_ref,
                    element_ref,
                    text,
                },
            ) => {
                if text.is_empty() || text.len() > 16 * 1024 || text.contains('\0') {
                    return Err(AppMacosHostError::InvalidCanonicalInput);
                }
                let observation = require_observation(&observation_ref)?;
                AppMacosHostAction::TypeText {
                    bundle_id,
                    process_id: observation.process_id,
                    window_id: observation.window_id,
                    observation_ref: observation_ref.to_string(),
                    element_token: observation.element_token(&element_ref)?,
                    text,
                }
            },
            (
                AppMacosOperation::PressKey,
                AppMacosCanonicalInput::PressKey {
                    observation_ref,
                    key,
                    modifiers,
                },
            ) => {
                if modifiers.len() > 4
                    || modifiers
                        .iter()
                        .map(|modifier| match modifier {
                            AppMacosHostModifier::Command => 1_u8,
                            AppMacosHostModifier::Option => 2,
                            AppMacosHostModifier::Control => 3,
                            AppMacosHostModifier::Shift => 4,
                        })
                        .collect::<BTreeSet<_>>()
                        .len()
                        != modifiers.len()
                {
                    return Err(AppMacosHostError::InvalidCanonicalInput);
                }
                let observation = require_observation(&observation_ref)?;
                AppMacosHostAction::PressKey {
                    bundle_id,
                    process_id: observation.process_id,
                    window_id: observation.window_id,
                    observation_ref: observation_ref.to_string(),
                    key,
                    modifiers,
                }
            },
            (
                AppMacosOperation::ScrollElement,
                AppMacosCanonicalInput::ScrollElement {
                    observation_ref,
                    element_ref,
                    direction,
                    amount,
                },
            ) => {
                if !(1..=APP_MACOS_HOST_MAX_SCROLL_AMOUNT).contains(&amount) {
                    return Err(AppMacosHostError::InvalidCanonicalInput);
                }
                let observation = require_observation(&observation_ref)?;
                AppMacosHostAction::ScrollElement {
                    bundle_id,
                    process_id: observation.process_id,
                    window_id: observation.window_id,
                    observation_ref: observation_ref.to_string(),
                    element_token: observation.element_token(&element_ref)?,
                    direction,
                    amount,
                }
            },
            (
                AppMacosOperation::DragElements,
                AppMacosCanonicalInput::DragElements {
                    observation_ref,
                    source_element_ref,
                    destination_element_ref,
                },
            ) => {
                let observation = require_observation(&observation_ref)?;
                // CuaDriver 0.28 drags in window-local screenshot pixels. An
                // observation without its screenshot scale cannot place them.
                let screenshot_scale_millis = observation
                    .screenshot_scale_millis
                    .ok_or(AppMacosHostError::PhysicalOperationUnavailable)?;
                AppMacosHostAction::DragElements {
                    bundle_id,
                    process_id: observation.process_id,
                    window_id: observation.window_id,
                    observation_ref: observation_ref.to_string(),
                    source_element_token: observation.element_token(&source_element_ref)?,
                    destination_element_token: observation
                        .element_token(&destination_element_ref)?,
                    screenshot_scale_millis,
                }
            },
            _ => return Err(AppMacosHostError::IdentityMismatch),
        };
        wire_action
            .validate()
            .map_err(|_| AppMacosHostError::InvalidCanonicalInput)?;
        if observation.is_some() != plan.operation.requires_observation() {
            return Err(AppMacosHostError::ObservationRequired);
        }
        // The desktop re-snapshots the window before acting and compares this
        // fence, not the whole window: macOS retitles a document on its own
        // after an edit, and that must not refuse an unrelated element.
        let observation_content_digest = observation
            .map(|observation| observation.fence_digest(&wire_action))
            .transpose()?;
        let observation_revalidation_byte_ceiling = observation.map(|observation| {
            observation
                .evidence_bytes
                .saturating_add(
                    magician_app_contract::macos_host::APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES,
                )
                .min(MAX_APP_MACOS_OBSERVATION_BYTES as u64)
        });
        Ok(AppMacosPreparedAction {
            action_ref: action_ref.clone(),
            operation: plan.operation,
            wire_action,
            input_digest: AppDigest::blake3(canonical_input),
            input_bytes: u64::try_from(canonical_input.len())
                .map_err(|_| AppMacosHostError::InvalidCanonicalInput)?,
            observation_content_digest,
            observation_revalidation_byte_ceiling,
        })
    }
}

impl AppEffectPhysicalOwner for AppMacosHostOwner {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        let plan = self
            .actions
            .get(action.action_ref())
            .ok_or(AppEffectKernelError::IdentityMismatch)?;
        if tool_ref != &self.tool_ref
            || primitive.primitive_ref() != &self.primitive_ref
            || primitive.source_content_digest() != &self.source_digest
            || !primitive
                .actions()
                .iter()
                .any(|candidate| candidate == action)
            || !action.dispatchable()
            || action.input_schema_digest() != Some(plan.reviewed.input_schema_digest())
            || action.result_schema_digest() != Some(plan.reviewed.result_schema_digest())
            || action.implementation_plan_digest() != Some(&plan.implementation_plan_digest)
            || action.transport_result_byte_ceiling() != Some(plan.reviewed.result_byte_ceiling())
        {
            return Err(AppEffectKernelError::IdentityMismatch);
        }
        let target_ref = self
            .owner_target_ref()
            .map_err(|_| AppEffectKernelError::IdentityMismatch)?;
        let target = AttestedAppToolTarget::from_trusted_local_dispatcher(
            tool_ref.clone(),
            target_ref.clone(),
        );
        Ok(AppEffectPhysicalTarget::from_owner(
            target_ref,
            self.source_digest.clone(),
            target,
        ))
    }
}

/// Runtime/desktop pairing capability. It is move-only, non-Serde, and never
/// exposed to package code or generic tool runtimes.
pub(crate) struct AppMacosHostPairing {
    key_id: String,
    signing_key: [u8; 32],
    desktop_identity_digest: AppDigest,
    desktop_identity_attestation_digest: AppDigest,
    host_identity_digest: AppDigest,
    cua_driver_binary_digest: AppDigest,
    gateway_endpoint_digest: AppDigest,
    tcc_policy_digest: AppDigest,
    tcc_epoch: u64,
}

impl Drop for AppMacosHostPairing {
    fn drop(&mut self) {
        self.signing_key.zeroize();
    }
}

impl AppMacosHostPairing {
    pub(crate) fn from_pairing_owner(
        key_id: String,
        signing_key: [u8; 32],
        desktop_identity_digest: AppDigest,
        desktop_identity_attestation_digest: AppDigest,
        host_identity_digest: AppDigest,
        cua_driver_binary_digest: AppDigest,
        gateway_endpoint_digest: AppDigest,
        tcc_policy_digest: AppDigest,
        tcc_epoch: u64,
    ) -> Result<Self, AppMacosHostError> {
        if key_id.is_empty() || key_id.len() > 64 || signing_key == [0_u8; 32] || tcc_epoch == 0 {
            return Err(AppMacosHostError::InvalidPairing);
        }
        Ok(Self {
            key_id,
            signing_key,
            desktop_identity_digest,
            desktop_identity_attestation_digest,
            host_identity_digest,
            cua_driver_binary_digest,
            gateway_endpoint_digest,
            tcc_policy_digest,
            tcc_epoch,
        })
    }

    fn mint_request(
        &self,
        owner: &AppMacosHostOwner,
        permit: &AppInteractiveOwnerIoPermit,
        prepared: &AppMacosPreparedAction,
        now: DateTime<Utc>,
        resource_expires_at: DateTime<Utc>,
    ) -> Result<SignedAppMacosHostRequest, AppMacosHostError> {
        let plan = owner
            .actions
            .get(&prepared.action_ref)
            .ok_or(AppMacosHostError::ActionNotReviewed)?;
        if permit.profile() != AppInteractiveExecutionProfile::MacosHost
            || permit.action().action_ref() != &prepared.action_ref
            || permit.action() != &plan.reviewed
            || permit.action().class() != prepared.operation.interactive_class()
            || permit.owner_target_ref() != &owner.owner_target_ref()?
            || permit.owner_target_digest() != owner.owner_target_digest()
            || permit.target_policy_digest() != &owner.target_policy_digest
            || permit.owner_profile_digest() != &owner.owner_profile_digest
            || permit.owner_implementation_digest() != &owner.owner_implementation_digest
            || self.host_identity_digest != owner.target.host_identity_digest
            || self.cua_driver_binary_digest != owner.cua_driver_binary_digest
            || permit.input_digest() != &prepared.input_digest
            || permit.input_bytes() != prepared.input_bytes
            || permit.observation_ref().map(AppReference::as_str)
                != prepared.wire_action.observation_ref()
            || permit.observation_digest().is_some() != prepared.operation.requires_observation()
            || permit.observation_digest().is_some()
                != prepared.observation_content_digest.is_some()
            || permit.observation_digest().is_some()
                != prepared.observation_revalidation_byte_ceiling.is_some()
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        let expires_at = permit
            .expires_at()
            .min(now + Duration::seconds(APP_MACOS_HOST_REQUEST_TTL_SECONDS))
            .min(resource_expires_at);
        if expires_at <= now {
            return Err(AppMacosHostError::StalePermit);
        }
        let claims = AppMacosHostPermitClaims {
            schema: APP_MACOS_HOST_WIRE_V2.to_owned(),
            key_id: self.key_id.clone(),
            nonce: format!("nonce:app-macos:{}", Uuid::new_v4()),
            host_identity_digest: self.host_identity_digest.to_string(),
            installation_id: permit.installation_id().to_string(),
            installation_generation: permit.installation_generation(),
            run_ref: permit.run_ref().to_string(),
            grant_revision: permit.grant_revision().get(),
            grant_digest: permit.grant_digest().to_string(),
            policy_digest: permit.policy_digest().to_string(),
            grant_descriptor_digest: permit.grant_descriptor_digest().to_string(),
            target_policy_digest: permit.target_policy_digest().to_string(),
            owner_profile_digest: permit.owner_profile_digest().to_string(),
            owner_implementation_digest: permit.owner_implementation_digest().to_string(),
            cua_driver_binary_digest: self.cua_driver_binary_digest.to_string(),
            owner_target_ref: permit.owner_target_ref().to_string(),
            owner_target_digest: permit.owner_target_digest().to_string(),
            session_binding_digest: permit.session_binding_digest().to_string(),
            resource_lease_ref: permit.resource_lease_ref().to_string(),
            effect_binding_digest: permit.effect_binding_digest().to_string(),
            interactive_permit_digest: permit.permit_digest().to_string(),
            action_ref: permit.action().action_ref().to_string(),
            action_class: prepared.operation.wire_class(),
            input_digest: permit.input_digest().to_string(),
            input_bytes: permit.input_bytes(),
            observation_digest: permit.observation_digest().map(ToString::to_string),
            observation_content_digest: prepared
                .observation_content_digest
                .as_ref()
                .map(ToString::to_string),
            observation_revalidation_byte_ceiling: prepared.observation_revalidation_byte_ceiling,
            result_byte_ceiling: permit.action().result_byte_ceiling(),
            evidence_byte_ceiling: permit.claim().evidence_bytes,
            bundle_id: owner.target.reviewed.bundle_id.clone(),
            application_identity_digest: owner
                .target
                .reviewed
                .application_identity_digest
                .to_string(),
            tcc_policy_digest: self.tcc_policy_digest.to_string(),
            tcc_epoch: self.tcc_epoch,
            issued_at_ms: now.timestamp_millis(),
            expires_at_ms: expires_at.timestamp_millis(),
        };
        SignedAppMacosHostRequest::mint(claims, prepared.wire_action.clone(), &self.signing_key)
            .map_err(|_| AppMacosHostError::Encoding)
    }

    fn validate_transport(
        &self,
        transport: &dyn AppMacosHostTransport,
    ) -> Result<(), AppMacosHostError> {
        if transport.endpoint_digest() != &self.gateway_endpoint_digest {
            return Err(AppMacosHostError::InvalidPairing);
        }
        Ok(())
    }
}

pub(crate) struct AppMacosPreparedAction {
    action_ref: AppReference,
    operation: AppMacosOperation,
    wire_action: AppMacosHostAction,
    input_digest: AppDigest,
    input_bytes: u64,
    observation_content_digest: Option<AppDigest>,
    observation_revalidation_byte_ceiling: Option<u64>,
}

mod transport_sealed {
    pub trait Sealed {}
}

#[async_trait]
pub(crate) trait AppMacosHostTransport: transport_sealed::Sealed + Send + Sync {
    fn endpoint_digest(&self) -> &AppDigest;

    /// Returns the exact bounded typed host response body. Timeout, disconnect,
    /// cancellation after poll and malformed HTTP are all `Err` and therefore
    /// settle outcome-uncertain at the common effect owner.
    async fn invoke(
        &self,
        request: &SignedAppMacosHostRequest,
        cancellation: AppInteractiveCancellation,
    ) -> Result<Vec<u8>, AppMacosHostTransportError>;
}

/// Exact local typed endpoint. It is created only by the pairing owner and
/// deliberately does not consult MAGICIAN_HOST_GATEWAY_URL, system proxies or
/// redirects. App input can never influence this URL.
pub(crate) struct AppMacosHttpHostTransport {
    action_url: Url,
    cancel_url: Url,
    endpoint_digest: AppDigest,
    client: reqwest::Client,
}

impl transport_sealed::Sealed for AppMacosHttpHostTransport {}

#[allow(dead_code)] // Endpoint identity inspection remains part of owner qualification.
impl AppMacosHttpHostTransport {
    pub(crate) fn from_pairing_owner(action_url: Url) -> Result<Self, AppMacosHostError> {
        let host = action_url
            .host_str()
            .ok_or(AppMacosHostError::InvalidPairing)?;
        if action_url.scheme() != "http"
            // The exact IP literal is the V1 connect identity. Hostnames are
            // not accepted because reqwest DNS resolution is not itself a
            // reviewed or cryptographically pinned desktop owner.
            || host != "127.0.0.1"
            || action_url.port() != Some(3017)
            || action_url.path() != "/host/apps/macos/action"
            || action_url.query().is_some()
            || action_url.fragment().is_some()
            || !action_url.username().is_empty()
            || action_url.password().is_some()
        {
            return Err(AppMacosHostError::InvalidPairing);
        }
        let endpoint_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "profile": APP_MACOS_HOST_PROFILE_V1,
            "typed_action_url": action_url.as_str(),
        }))
        .map_err(|_| AppMacosHostError::Encoding)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(
                APP_MACOS_HOST_TRANSPORT_TIMEOUT_SECONDS,
            ))
            .build()
            .map_err(|_| AppMacosHostError::InvalidPairing)?;
        let mut cancel_url = action_url.clone();
        cancel_url.set_path("/host/apps/macos/cancel");
        Ok(Self {
            action_url,
            cancel_url,
            endpoint_digest,
            client,
        })
    }

    pub(crate) fn endpoint_digest(&self) -> &AppDigest {
        &self.endpoint_digest
    }

    async fn request_exact_cancel(&self, request_bytes: &[u8]) {
        for _ in 0..3 {
            let cancel = self
                .client
                .post(self.cancel_url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(request_bytes.to_vec())
                .send();
            if tokio::time::timeout(std::time::Duration::from_millis(250), cancel)
                .await
                .is_ok_and(|result| {
                    result.is_ok_and(|response| response.status() == reqwest::StatusCode::OK)
                })
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

#[async_trait]
impl AppMacosHostTransport for AppMacosHttpHostTransport {
    fn endpoint_digest(&self) -> &AppDigest {
        &self.endpoint_digest
    }

    async fn invoke(
        &self,
        request: &SignedAppMacosHostRequest,
        cancellation: AppInteractiveCancellation,
    ) -> Result<Vec<u8>, AppMacosHostTransportError> {
        let request_bytes =
            serde_json::to_vec(request).map_err(|_| AppMacosHostTransportError::InvalidResponse)?;
        if request_bytes.is_empty() || request_bytes.len() > MAX_APP_MACOS_CANONICAL_INPUT_BYTES {
            return Err(AppMacosHostTransportError::InvalidResponse);
        }
        let response_ceiling = request
            .claims
            .result_byte_ceiling
            .checked_add(request.claims.evidence_byte_ceiling)
            .and_then(|value| {
                value.checked_add(
                    magician_app_contract::macos_host::APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES,
                )
            })
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(AppMacosHostTransportError::InvalidResponse)?;
        let send = self
            .client
            .post(self.action_url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request_bytes.clone())
            .send();
        tokio::pin!(send);
        let mut cancellation_poll = tokio::time::interval(std::time::Duration::from_millis(25));
        let response = loop {
            tokio::select! {
                biased;
                _ = cancellation_poll.tick() => {
                    if cancellation.is_cancelled() {
                        self.request_exact_cancel(&request_bytes).await;
                        return Err(AppMacosHostTransportError::CancelledAfterPoll);
                    }
                },
                response = &mut send => {
                    break response.map_err(map_app_macos_transport_error)?;
                },
            }
        };
        if response.status() != reqwest::StatusCode::OK
            || response
                .content_length()
                .is_some_and(|length| length > response_ceiling as u64)
        {
            return Err(AppMacosHostTransportError::InvalidResponse);
        }
        let mut response_bytes = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or(0)
                .min(response_ceiling),
        );
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = cancellation_poll.tick() => {
                    if cancellation.is_cancelled() {
                        self.request_exact_cancel(&request_bytes).await;
                        return Err(AppMacosHostTransportError::CancelledAfterPoll);
                    }
                    continue;
                },
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = chunk.map_err(map_app_macos_transport_error)?;
            if response_bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > response_ceiling)
            {
                return Err(AppMacosHostTransportError::InvalidResponse);
            }
            response_bytes.extend_from_slice(&chunk);
        }
        if response_bytes.is_empty() {
            return Err(AppMacosHostTransportError::InvalidResponse);
        }
        Ok(response_bytes)
    }
}

fn map_app_macos_transport_error(error: reqwest::Error) -> AppMacosHostTransportError {
    if error.is_timeout() {
        AppMacosHostTransportError::Timeout
    } else if error.is_connect() {
        AppMacosHostTransportError::Unavailable
    } else if error.is_body() {
        AppMacosHostTransportError::Disconnected
    } else {
        AppMacosHostTransportError::InvalidResponse
    }
}

struct AppMacosLiveObservation {
    interactive: AppInteractiveObservationRef,
    owner: AppMacosOwnerObservation,
}

/// Run-owned paired-host state retained through the common interactive owner
/// registry. Pairing keys, PIDs/window IDs and AX indices remain inside this
/// typed owner and are never serialized into workflow state.
pub(crate) struct AppMacosLiveSession {
    owner: Arc<AppMacosHostOwner>,
    pairing: AppMacosHostPairing,
    transport: AppMacosHttpHostTransport,
    session: AppInteractiveSessionHandle,
    grant: AppInteractiveGrantDescriptor,
    observation: Option<AppMacosLiveObservation>,
}

impl AppMacosLiveSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: AppMacosHostOwner,
        pairing: AppMacosHostPairing,
        transport: AppMacosHttpHostTransport,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        resource_lease_ref: AppReference,
        cancellation: AppInteractiveCancellation,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppMacosHostError> {
        let session = AppInteractiveSessionHandle::acquire(
            &grant,
            current,
            owner.owner_target_ref()?,
            owner.owner_target_digest().clone(),
            resource_lease_ref,
            1,
            cancellation,
            now,
            expires_at,
        )?;
        Ok(Self {
            owner: Arc::new(owner),
            pairing,
            transport,
            session,
            grant,
            observation: None,
        })
    }

    pub(crate) fn binding_digest(&self) -> &AppDigest {
        self.session.binding_digest()
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.session.cancellation()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rebind_authority(
        &mut self,
        tool_ref: AppReference,
        primitive_ref: AppReference,
        source_digest: AppDigest,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        operation: AppMacosOperation,
        action_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<(), AppMacosHostError> {
        Arc::get_mut(&mut self.owner)
            .ok_or(AppMacosHostError::PhysicalOperationUnavailable)?
            .rebind_exact_leaf(
                tool_ref,
                primitive_ref,
                source_digest,
                &grant,
                operation,
                action_ref,
            )?;
        self.session.rebind_exact_leaf(&grant, current, now)?;
        self.grant = grant;
        if let Some(previous) = self.observation.take() {
            let old = previous.interactive;
            let expires_at = old.expires_at().min(self.grant.expires_at());
            if expires_at > now {
                let interactive = self.session.issue_observation(
                    &self.grant,
                    current,
                    old.kind(),
                    old.geometry().clone(),
                    old.content_digest().clone(),
                    old.labels_digest().clone(),
                    old.evidence_bytes(),
                    old.evidence_nodes(),
                    now,
                    expires_at,
                )?;
                self.observation = Some(AppMacosLiveObservation {
                    interactive,
                    owner: previous.owner,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn materialize_observation(
        &mut self,
        current: &AppInteractiveCurrentFence,
        projection: AppMacosObservationProjection,
        now: DateTime<Utc>,
    ) -> Result<(), AppMacosHostError> {
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(30))
            .ok_or(AppMacosHostError::InvalidHostResponse)?
            .min(self.grant.expires_at());
        let interactive = self.session.issue_observation(
            &self.grant,
            current,
            AppInteractiveObservationKind::StructuredTree,
            projection.geometry().clone(),
            projection.content_digest().clone(),
            projection.labels_digest().clone(),
            projection.evidence_bytes(),
            projection.evidence_nodes(),
            now,
            expires_at,
        )?;
        let owner = projection.bind(self.owner.as_ref(), &interactive)?;
        self.observation = Some(AppMacosLiveObservation { interactive, owner });
        Ok(())
    }

    pub(crate) fn prepare_action(
        live: Arc<tokio::sync::Mutex<Self>>,
        current: &AppInteractiveCurrentFence,
        action_ref: &AppReference,
        canonical_input: Vec<u8>,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPreparedWorkflowAction, AppMacosHostError> {
        let mut guard = live
            .try_lock()
            .map_err(|_| AppMacosHostError::PhysicalOperationUnavailable)?;
        let operation = guard
            .owner
            .operation_for_action(action_ref)
            .ok_or(AppMacosHostError::ActionNotReviewed)?;
        let observation = if operation.requires_observation() {
            Some(
                guard
                    .observation
                    .take()
                    .ok_or(AppMacosHostError::ObservationRequired)?,
            )
        } else {
            None
        };
        let prepared = guard.owner.prepare_action(
            action_ref,
            &canonical_input,
            observation.as_ref().map(|value| &value.owner),
        )?;
        let claim = if operation == AppMacosOperation::Observe {
            AppInteractiveResourceClaim {
                evidence_bytes: MAX_APP_MACOS_OBSERVATION_BYTES as u64,
                evidence_nodes: MAX_APP_MACOS_REVIEWED_OBSERVATION_ELEMENTS,
                pixels: 0,
                artifact_bytes: 0,
                output_bytes: APP_MACOS_OBSERVE_RESULT_CEILING,
            }
        } else {
            AppInteractiveResourceClaim {
                evidence_bytes: 0,
                evidence_nodes: 0,
                pixels: 0,
                artifact_bytes: 0,
                output_bytes: APP_MACOS_ACTION_RESULT_CEILING,
            }
        };
        let grant = guard.grant.clone();
        let permit = guard.session.authorize_action(
            &grant,
            current,
            action_ref,
            &canonical_input,
            claim,
            observation.map(|value| value.interactive),
            now,
        )?;
        let owner = guard.owner.clone();
        drop(guard);
        Ok(AppMacosPreparedWorkflowAction {
            owner,
            pairing: None,
            transport: None,
            prepared,
            canonical_input,
            interactive_permit: permit,
            session: None,
            live: Some(live),
            current: Some(current.clone()),
        })
    }
}

pub(crate) struct AppMacosPreparedWorkflowAction {
    owner: Arc<AppMacosHostOwner>,
    pairing: Option<AppMacosHostPairing>,
    transport: Option<AppMacosHttpHostTransport>,
    prepared: AppMacosPreparedAction,
    canonical_input: Vec<u8>,
    interactive_permit: AppInteractiveActionPermit,
    session: Option<AppInteractiveSessionHandle>,
    live: Option<Arc<tokio::sync::Mutex<AppMacosLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

#[allow(dead_code)] // Snapshot-only preparation remains a compatibility constructor.
impl AppMacosPreparedWorkflowAction {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_single_observe(
        owner: AppMacosHostOwner,
        pairing: AppMacosHostPairing,
        transport: AppMacosHttpHostTransport,
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        action_ref: &AppReference,
        canonical_input: Vec<u8>,
        resource_lease_ref: AppReference,
        cancellation: AppInteractiveCancellation,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppMacosHostError> {
        if owner.operation_for_action(action_ref) != Some(AppMacosOperation::Observe)
            || canonical_input.len() > MAX_APP_MACOS_CANONICAL_INPUT_BYTES
        {
            return Err(AppMacosHostError::ActionNotReviewed);
        }
        let prepared = owner.prepare_action(action_ref, &canonical_input, None)?;
        let mut session = AppInteractiveSessionHandle::acquire(
            grant,
            current,
            owner.owner_target_ref()?,
            owner.owner_target_digest().clone(),
            resource_lease_ref,
            1,
            cancellation,
            now,
            expires_at,
        )
        .map_err(AppMacosHostError::Interactive)?;
        let interactive_permit = session
            .authorize_action(
                grant,
                current,
                action_ref,
                &canonical_input,
                AppInteractiveResourceClaim {
                    evidence_bytes: MAX_APP_MACOS_OBSERVATION_BYTES as u64,
                    evidence_nodes: MAX_APP_MACOS_REVIEWED_OBSERVATION_ELEMENTS,
                    pixels: 0,
                    artifact_bytes: 0,
                    output_bytes: APP_MACOS_OBSERVE_RESULT_CEILING,
                },
                None,
                now,
            )
            .map_err(AppMacosHostError::Interactive)?;
        Ok(Self {
            owner: Arc::new(owner),
            pairing: Some(pairing),
            transport: Some(transport),
            prepared,
            canonical_input,
            interactive_permit,
            session: Some(session),
            live: None,
            current: None,
        })
    }

    pub(crate) fn bind_effect(
        self,
        binding: &AppEffectBinding,
        now: DateTime<Utc>,
    ) -> Result<AppMacosEffectAction, AppMacosHostError> {
        let interactive_effect = self
            .interactive_permit
            .bind_effect(binding, &self.canonical_input, now)
            .map_err(AppMacosHostError::Interactive)?;
        Ok(AppMacosEffectAction {
            owner: self.owner,
            pairing: self.pairing,
            transport: self.transport,
            prepared: self.prepared,
            interactive_effect,
            session: self.session,
            live: self.live,
            current: self.current,
        })
    }
}

impl AppEffectPhysicalOwner for AppMacosPreparedWorkflowAction {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        self.owner.attest_effect_target(tool_ref, primitive, action)
    }
}

pub(crate) struct AppMacosEffectAction {
    owner: Arc<AppMacosHostOwner>,
    pairing: Option<AppMacosHostPairing>,
    transport: Option<AppMacosHttpHostTransport>,
    prepared: AppMacosPreparedAction,
    interactive_effect: AppInteractiveEffectPermit,
    session: Option<AppInteractiveSessionHandle>,
    live: Option<Arc<tokio::sync::Mutex<AppMacosLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

impl AppMacosEffectAction {
    pub(crate) fn interactive_inspection(&self) -> AppInteractiveEffectInspection {
        self.interactive_effect.inspection()
    }
}

pub(crate) struct AppMacosIoSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl AppMacosIoSlot {
    pub(crate) fn is_live_for_start(&self) -> bool {
        true
    }
}

fn macos_io_slots() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
}

pub(crate) async fn reserve_macos_io_slot() -> Result<AppMacosIoSlot, AppMacosHostError> {
    let permit = std::sync::Arc::clone(macos_io_slots())
        .acquire_owned()
        .await
        .map_err(|_| AppMacosHostError::PhysicalOperationUnavailable)?;
    Ok(AppMacosIoSlot { _permit: permit })
}

pub(crate) struct AppMacosObservedEffect<R> {
    result: ActionResult,
    canonical_result: Vec<u8>,
    observation: Option<AppMacosObservationProjection>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    effect: AppEffectInFlight<R>,
}

impl<R> AppMacosObservedEffect<R> {
    pub(crate) fn action_result(&self) -> &ActionResult {
        &self.result
    }

    pub(crate) fn canonical_result_bytes(&self) -> &[u8] {
        &self.canonical_result
    }

    pub(crate) fn interactive_receipt(&self) -> &AppInteractiveSettlementReceipt {
        &self.interactive_receipt
    }

    pub(crate) fn outcome_uncertain(
        self,
        stage: AppEffectStage,
        error: AppMacosHostError,
    ) -> AppMacosUncertainEffect<R> {
        AppMacosUncertainEffect {
            error,
            interactive_receipt: Some(self.interactive_receipt),
            settlement: self.effect.outcome_uncertain(stage),
        }
    }

    pub(crate) fn commit(self) -> Result<AppMacosCommittedEffect<R>, AppMacosUncertainEffect<R>> {
        let settlement = match self.effect.commit_result(&self.canonical_result) {
            Ok(settlement) => settlement,
            Err(settlement) => {
                return Err(AppMacosUncertainEffect {
                    error: AppMacosHostError::InvalidHostResponse,
                    interactive_receipt: Some(self.interactive_receipt),
                    settlement,
                })
            },
        };
        Ok(AppMacosCommittedEffect {
            result: self.result,
            observation: self.observation,
            interactive_receipt: self.interactive_receipt,
            settlement,
        })
    }
}

pub(crate) struct AppMacosCommittedEffect<R> {
    result: ActionResult,
    observation: Option<AppMacosObservationProjection>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppMacosCommittedEffect<R> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ActionResult,
        Option<AppMacosObservationProjection>,
        AppInteractiveSettlementReceipt,
        AppEffectSettlement<R>,
    ) {
        (
            self.result,
            self.observation,
            self.interactive_receipt,
            self.settlement,
        )
    }
}

pub(crate) struct AppMacosUncertainEffect<R> {
    error: AppMacosHostError,
    interactive_receipt: Option<AppInteractiveSettlementReceipt>,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppMacosUncertainEffect<R> {
    pub(crate) fn interactive_receipt(&self) -> Option<&AppInteractiveSettlementReceipt> {
        self.interactive_receipt.as_ref()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        AppMacosHostError,
        Option<AppInteractiveSettlementReceipt>,
        AppEffectSettlement<R>,
    ) {
        (self.error, self.interactive_receipt, self.settlement)
    }
}

pub(crate) enum AppMacosEffectOutcome<R> {
    Observed(AppMacosObservedEffect<R>),
    Uncertain(AppMacosUncertainEffect<R>),
}

pub(crate) async fn execute_started_macos<R>(
    _slot: AppMacosIoSlot,
    action: AppMacosEffectAction,
    mut effect: AppEffectInFlight<R>,
    physical_timeout: std::time::Duration,
) -> AppMacosEffectOutcome<R> {
    let resource_deadline = tokio::time::Instant::now() + physical_timeout;
    let deadline_now = Utc::now();
    let resource_expires_at = Duration::from_std(physical_timeout)
        .ok()
        .and_then(|duration| deadline_now.checked_add_signed(duration))
        .unwrap_or(deadline_now);
    let AppMacosEffectAction {
        owner,
        pairing,
        transport,
        prepared,
        interactive_effect,
        session,
        live,
        current,
    } = action;
    let Some(authorization) = effect.take_provider_io_authorization() else {
        let receipt = interactive_effect.outcome_uncertain().ok();
        return AppMacosEffectOutcome::Uncertain(AppMacosUncertainEffect {
            error: AppMacosHostError::IdentityMismatch,
            interactive_receipt: receipt,
            settlement: effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        });
    };
    let attempt = match (live, pairing, transport, session, current) {
        (Some(live), None, None, None, Some(current)) => {
            let mut guard = match tokio::time::timeout_at(resource_deadline, live.lock()).await {
                Ok(guard) => guard,
                Err(_) => {
                    interactive_effect
                        .cancellation()
                        .cancel(AppInteractiveCancellationReason::Deadline);
                    let receipt = interactive_effect.outcome_uncertain().ok();
                    return AppMacosEffectOutcome::Uncertain(AppMacosUncertainEffect {
                        error: AppMacosHostError::CancelledAfterDispatchStart,
                        interactive_receipt: receipt,
                        settlement: effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
                    });
                },
            };
            let attempt = prepared
                .execute(
                    &owner,
                    &guard.pairing,
                    interactive_effect,
                    authorization,
                    &guard.transport,
                    Utc::now(),
                    resource_deadline,
                    resource_expires_at,
                )
                .await;
            match attempt {
                Ok(AppMacosOwnerAttempt::Completed(mut completed)) => {
                    if let Some(projection) = completed.observation.take() {
                        if let Err(cause) =
                            guard.materialize_observation(&current, projection, Utc::now())
                        {
                            return AppMacosEffectOutcome::Uncertain(AppMacosUncertainEffect {
                                error: cause,
                                interactive_receipt: Some(completed.receipt),
                                settlement: effect
                                    .outcome_uncertain(AppEffectStage::ResultMaterialization),
                            });
                        }
                    }
                    Ok(AppMacosOwnerAttempt::Completed(completed))
                },
                other => other,
            }
        },
        (None, Some(pairing), Some(transport), Some(_session), None) => {
            prepared
                .execute(
                    &owner,
                    &pairing,
                    interactive_effect,
                    authorization,
                    &transport,
                    Utc::now(),
                    resource_deadline,
                    resource_expires_at,
                )
                .await
        },
        _ => Err(AppMacosHostError::IdentityMismatch),
    };
    match attempt {
        Ok(AppMacosOwnerAttempt::Completed(completed)) => {
            AppMacosEffectOutcome::Observed(AppMacosObservedEffect {
                result: completed.result,
                canonical_result: completed.result_bytes,
                observation: completed.observation,
                interactive_receipt: completed.receipt,
                effect,
            })
        },
        Ok(AppMacosOwnerAttempt::OutcomeUncertain { receipt, cause }) => {
            AppMacosEffectOutcome::Uncertain(AppMacosUncertainEffect {
                error: cause,
                interactive_receipt: Some(receipt),
                settlement: effect.outcome_uncertain(AppEffectStage::ProviderIo),
            })
        },
        Err(error) => AppMacosEffectOutcome::Uncertain(AppMacosUncertainEffect {
            error,
            interactive_receipt: None,
            settlement: effect.outcome_uncertain(AppEffectStage::ProviderIo),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMacosHostTransportError {
    Unavailable,
    Timeout,
    Disconnected,
    CancelledAfterPoll,
    InvalidResponse,
}

pub(crate) enum AppMacosOwnerAttempt {
    Completed(AppMacosCompletedAction),
    OutcomeUncertain {
        receipt: AppInteractiveSettlementReceipt,
        cause: AppMacosHostError,
    },
}

/// Move-only physical element projection. The app-visible result contains
/// only the matching opaque element refs; the raw CUA element tokens (and the
/// screenshot scale a drag needs) are retained here until the common
/// observation ref is issued and then bound back to the exact owner target.
pub(crate) struct AppMacosObservationProjection {
    public_ref: AppReference,
    owner_target_digest: AppDigest,
    content_digest: AppDigest,
    evidence_bytes: u64,
    labels_digest: AppDigest,
    geometry: AppInteractiveGeometry,
    screenshot_scale_millis: Option<u16>,
    process_id: u32,
    window_id: u32,
    elements: Vec<(AppReference, AppMacosObservedElement)>,
    fence_tree: String,
}

/// Public, bounded semantics for one AX node. Physical indices, process/window
/// identities and native selectors never enter this projection. Editable-field
/// values are deliberately omitted even when CUA happens to include them.
#[derive(Debug, Serialize)]
struct AppMacosSanitizedNode {
    element_ref: AppReference,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected: Option<bool>,
    depth: u16,
}

#[derive(Serialize)]
struct AppMacosSemanticDigestNode<'a> {
    role: &'a str,
    label: Option<&'a str>,
    title: Option<&'a str>,
    value: Option<&'a str>,
    enabled: Option<bool>,
    selected: Option<bool>,
    depth: u16,
}

struct AppMacosProjectedElements {
    mappings: Vec<(AppReference, AppMacosObservedElement)>,
    nodes: Vec<AppMacosSanitizedNode>,
    labels_digest: AppDigest,
}

impl AppMacosObservationProjection {
    pub(crate) fn geometry(&self) -> &AppInteractiveGeometry {
        &self.geometry
    }

    pub(crate) fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub(crate) fn labels_digest(&self) -> &AppDigest {
        &self.labels_digest
    }

    pub(crate) fn evidence_bytes(&self) -> u64 {
        self.evidence_bytes
    }

    pub(crate) fn evidence_nodes(&self) -> u64 {
        u64::try_from(self.elements.len()).unwrap_or(u64::MAX)
    }

    pub(crate) fn bind(
        self,
        owner: &AppMacosHostOwner,
        observation: &AppInteractiveObservationRef,
    ) -> Result<AppMacosOwnerObservation, AppMacosHostError> {
        if &self.owner_target_digest != owner.owner_target_digest()
            || observation.kind() != AppInteractiveObservationKind::StructuredTree
            || observation.geometry() != &self.geometry
            || observation.content_digest() != &self.content_digest
            || observation.labels_digest() != &self.labels_digest
            || observation.evidence_bytes() != self.evidence_bytes
            || observation.evidence_nodes()
                != u64::try_from(self.elements.len()).unwrap_or(u64::MAX)
            || owner.target.process_id != Some(self.process_id)
            || owner.target.window_id != Some(self.window_id)
        {
            return Err(AppMacosHostError::IdentityMismatch);
        }
        AppMacosOwnerObservation::from_owner_projection(
            owner,
            self.public_ref,
            self.fence_tree,
            self.evidence_bytes,
            self.screenshot_scale_millis,
            self.elements,
        )
    }
}

pub(crate) struct AppMacosCompletedAction {
    pub(crate) result: ActionResult,
    pub(crate) result_bytes: Vec<u8>,
    pub(crate) receipt: AppInteractiveSettlementReceipt,
    pub(crate) observation: Option<AppMacosObservationProjection>,
}

impl AppMacosPreparedAction {
    pub(crate) async fn execute(
        self,
        owner: &AppMacosHostOwner,
        pairing: &AppMacosHostPairing,
        effect_permit: AppInteractiveEffectPermit,
        authorization: AppEffectProviderIoAuthorization,
        transport: &dyn AppMacosHostTransport,
        now: DateTime<Utc>,
        resource_deadline: tokio::time::Instant,
        resource_expires_at: DateTime<Utc>,
    ) -> Result<AppMacosOwnerAttempt, AppMacosHostError> {
        let permit = match effect_permit.start_owner_io(authorization, now) {
            Ok(permit) => permit,
            Err(failure) => {
                let (error, permit) = failure.into_parts();
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                    .map_err(AppMacosHostError::Interactive)?;
                return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
                    receipt,
                    cause: AppMacosHostError::Interactive(error),
                });
            },
        };
        if let Err(cause) = pairing.validate_transport(transport) {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                .map_err(AppMacosHostError::Interactive)?;
            return Ok(AppMacosOwnerAttempt::OutcomeUncertain { receipt, cause });
        }
        let request = match pairing.mint_request(owner, &permit, &self, now, resource_expires_at) {
            Ok(request) => request,
            Err(cause) => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                    .map_err(AppMacosHostError::Interactive)?;
                return Ok(AppMacosOwnerAttempt::OutcomeUncertain { receipt, cause });
            },
        };
        if permit.cancellation_reason().is_some() {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                .map_err(AppMacosHostError::Interactive)?;
            return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
                receipt,
                cause: AppMacosHostError::CancelledAfterDispatchStart,
            });
        }
        let cancellation = permit.cancellation();
        let invocation = transport.invoke(&request, cancellation.clone());
        tokio::pin!(invocation);
        let response_bytes = match tokio::time::timeout_at(resource_deadline, &mut invocation).await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                    .map_err(AppMacosHostError::Interactive)?;
                return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
                    receipt,
                    cause: AppMacosHostError::Transport(error),
                });
            },
            Err(_) => {
                cancellation.cancel(AppInteractiveCancellationReason::Deadline);
                // Keep polling the owner briefly after sticky cancellation so
                // the local transport can send its exact cancel request and
                // reap the in-flight HTTP exchange. This cleanup does not
                // change the common outcome-uncertain settlement.
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(1), &mut invocation).await;
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                    .map_err(AppMacosHostError::Interactive)?;
                return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
                    receipt,
                    cause: AppMacosHostError::CancelledAfterDispatchStart,
                });
            },
        };
        if permit.cancellation_reason().is_some() {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                .map_err(AppMacosHostError::Interactive)?;
            return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
                receipt,
                cause: AppMacosHostError::CancelledAfterDispatchStart,
            });
        }
        let response = match AppMacosHostResponse::parse(
            &response_bytes,
            &request,
            permit
                .action()
                .result_byte_ceiling()
                .checked_add(permit.claim().evidence_bytes)
                .and_then(|value| {
                    value.checked_add(
                        magician_app_contract::macos_host::APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES,
                    )
                })
                .ok_or(AppMacosHostError::InvalidHostResponse)?,
            self.operation,
        ) {
            Ok(response) => response,
            Err(cause) => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                    .map_err(AppMacosHostError::Interactive)?;
                return Ok(AppMacosOwnerAttempt::OutcomeUncertain { receipt, cause });
            },
        };
        complete_owner_response(owner, permit, response, resource_deadline)
    }
}

#[derive(Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum AppMacosCanonicalResult<'a> {
    Action {
        schema: &'static str,
        operation: AppMacosOperation,
        target_ref: &'a AppReference,
        tcc_policy_digest: &'a AppDigest,
        tcc_epoch: u64,
        outcome: &'static str,
    },
    StructuredObservation {
        schema: &'static str,
        operation: AppMacosOperation,
        target_ref: &'a AppReference,
        tcc_policy_digest: &'a AppDigest,
        tcc_epoch: u64,
        evidence_digest: &'a AppDigest,
        observation_ref: &'a AppReference,
        geometry: &'a AppInteractiveGeometry,
        nodes: &'a [AppMacosSanitizedNode],
    },
}

fn complete_owner_response(
    owner: &AppMacosHostOwner,
    permit: AppInteractiveOwnerIoPermit,
    response: AppMacosHostResponse,
    resource_deadline: tokio::time::Instant,
) -> Result<AppMacosOwnerAttempt, AppMacosHostError> {
    if permit.cancellation_reason().is_some() {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
            .map_err(AppMacosHostError::Interactive)?;
        return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
            receipt,
            cause: AppMacosHostError::CancelledAfterDispatchStart,
        });
    }
    let completion = match prepare_owner_completion(owner, &permit, response) {
        Ok(completion) => completion,
        Err(cause) => {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
                .map_err(AppMacosHostError::Interactive)?;
            return Ok(AppMacosOwnerAttempt::OutcomeUncertain { receipt, cause });
        },
    };
    if permit.cancellation_reason().is_some() || tokio::time::Instant::now() >= resource_deadline {
        permit
            .cancellation()
            .cancel(AppInteractiveCancellationReason::Deadline);
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit)
            .map_err(AppMacosHostError::Interactive)?;
        return Ok(AppMacosOwnerAttempt::OutcomeUncertain {
            receipt,
            cause: AppMacosHostError::CancelledAfterDispatchStart,
        });
    }
    let receipt = AppInteractiveSettlementReceipt::completed(
        permit,
        &completion.result_bytes,
        completion.evidence_digest,
        completion.evidence_bytes,
    )
    .map_err(AppMacosHostError::Interactive)?;
    Ok(AppMacosOwnerAttempt::Completed(AppMacosCompletedAction {
        result: completion.result,
        result_bytes: completion.result_bytes,
        receipt,
        observation: completion.observation,
    }))
}

struct AppMacosPreparedCompletion {
    result: ActionResult,
    result_bytes: Vec<u8>,
    evidence_digest: Option<AppDigest>,
    evidence_bytes: u64,
    observation: Option<AppMacosObservationProjection>,
}

fn prepare_owner_completion(
    owner: &AppMacosHostOwner,
    permit: &AppInteractiveOwnerIoPermit,
    response: AppMacosHostResponse,
) -> Result<AppMacosPreparedCompletion, AppMacosHostError> {
    let response_tcc_policy_digest = AppDigest::parse(response.tcc_policy_digest.clone())
        .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
    let operation = owner
        .actions
        .get(permit.action().action_ref())
        .ok_or(AppMacosHostError::ActionNotReviewed)?
        .operation;
    let (result, evidence_digest, evidence_bytes, observation) = match response.observation {
        Some(raw_observation) => {
            if operation != AppMacosOperation::Observe {
                return Err(AppMacosHostError::InvalidHostResponse);
            }
            let process_id = response
                .process_id
                .ok_or(AppMacosHostError::InvalidHostResponse)?;
            let window_id = response
                .window_id
                .ok_or(AppMacosHostError::InvalidHostResponse)?;
            if owner
                .target
                .process_id
                .is_some_and(|expected| expected != process_id)
                || owner
                    .target
                    .window_id
                    .is_some_and(|expected| expected != window_id)
            {
                return Err(AppMacosHostError::IdentityMismatch);
            }
            let evidence_digest = AppDigest::blake3(raw_observation.as_bytes());
            let fence_tree = project_observation_tree(&raw_observation)?;
            // The observation's content identity (menu bar excluded). Each
            // action's desktop fence is narrower and derived from `fence_tree`.
            let content_digest =
                AppDigest::blake3(app_macos_host_observation_content_tree(&fence_tree).as_bytes());
            let evidence_bytes = u64::try_from(raw_observation.len())
                .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
            let projected = project_observation_elements(&raw_observation)?;
            if u64::try_from(projected.mappings.len()).unwrap_or(u64::MAX)
                > permit.claim().evidence_nodes
            {
                return Err(AppMacosHostError::InvalidHostResponse);
            }
            let geometry = project_observation_geometry(&raw_observation)?;
            let screenshot_scale_millis = project_observation_screenshot_scale(&raw_observation)?;
            let public_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                "schema":"magician.app-macos-observation-public-ref.v1",
                "owner_target_digest":owner.owner_target_digest(),
                "content_digest":&content_digest,
                "labels_digest":&projected.labels_digest,
                "geometry":&geometry,
                "tcc_policy_digest":&response_tcc_policy_digest,
                "tcc_epoch":response.tcc_epoch,
            }))
            .map_err(|_| AppMacosHostError::Encoding)?;
            let public_ref = AppReference::parse(format!(
                "macos-observation:{}",
                public_digest.as_str().trim_start_matches("blake3:")
            ))
            .map_err(|_| AppMacosHostError::Encoding)?;
            let result_value =
                serde_json::to_value(&AppMacosCanonicalResult::StructuredObservation {
                    schema: "magician.app-macos-result.v1",
                    operation,
                    target_ref: &owner.target.reviewed.target_ref,
                    tcc_policy_digest: &response_tcc_policy_digest,
                    tcc_epoch: response.tcc_epoch,
                    evidence_digest: &evidence_digest,
                    observation_ref: &public_ref,
                    geometry: &geometry,
                    nodes: &projected.nodes,
                })
                .map_err(|_| AppMacosHostError::Encoding)?;
            let observation = Some(AppMacosObservationProjection {
                public_ref,
                owner_target_digest: owner.owner_target_digest().clone(),
                content_digest,
                evidence_bytes,
                labels_digest: projected.labels_digest,
                geometry,
                screenshot_scale_millis,
                process_id,
                window_id,
                elements: projected.mappings,
                fence_tree,
            });
            (
                ActionResult::Browser { data: result_value },
                Some(evidence_digest),
                evidence_bytes,
                observation,
            )
        },
        None => {
            let result_value = serde_json::to_value(&AppMacosCanonicalResult::Action {
                schema: "magician.app-macos-result.v1",
                operation,
                target_ref: &owner.target.reviewed.target_ref,
                tcc_policy_digest: &response_tcc_policy_digest,
                tcc_epoch: response.tcc_epoch,
                outcome: "completed",
            })
            .map_err(|_| AppMacosHostError::Encoding)?;
            (ActionResult::Browser { data: result_value }, None, 0, None)
        },
    };
    let result_bytes = canonical_json_bytes(
        &serde_json::to_value(&result).map_err(|_| AppMacosHostError::Encoding)?,
    )
    .map_err(|_| AppMacosHostError::Encoding)?;
    AppInteractiveSettlementReceipt::preflight_completed(
        permit,
        &result_bytes,
        evidence_digest.as_ref(),
        evidence_bytes,
    )
    .map_err(AppMacosHostError::Interactive)?;
    Ok(AppMacosPreparedCompletion {
        result,
        result_bytes,
        evidence_digest,
        evidence_bytes,
        observation,
    })
}

/// Project CuaDriver 0.28's structured `elements[]` (never the Markdown
/// rendering, whose `[N]` markers are presentation only). Every row must carry
/// a unique `element_index` and an `element_token` minted by the reply's own
/// top-level `snapshot_id` for that same index; anything else fails closed.
fn project_observation_elements(
    raw_observation: &str,
) -> Result<AppMacosProjectedElements, AppMacosHostError> {
    let value: Value = serde_json::from_str(raw_observation)
        .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
    let snapshot_id = value
        .get("snapshot_id")
        .and_then(Value::as_str)
        .filter(|snapshot_id| app_macos_host_valid_snapshot_id(snapshot_id))
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    let elements = value
        .get("elements")
        .and_then(Value::as_array)
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    if elements.len() > MAX_APP_MACOS_OBSERVATION_ELEMENTS {
        return Err(AppMacosHostError::InvalidHostResponse);
    }
    let mut indices = BTreeSet::new();
    let mut mappings = Vec::with_capacity(elements.len());
    let mut nodes = Vec::with_capacity(elements.len());
    for element in elements {
        let element = element
            .as_object()
            .ok_or(AppMacosHostError::InvalidHostResponse)?;
        let index = element
            .get("element_index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok())
            .ok_or(AppMacosHostError::InvalidHostResponse)?;
        if !indices.insert(index) {
            return Err(AppMacosHostError::InvalidHostResponse);
        }
        let element_token = element
            .get("element_token")
            .and_then(Value::as_str)
            .ok_or(AppMacosHostError::InvalidHostResponse)?;
        match app_macos_host_parse_element_token(element_token) {
            Some((token_snapshot, token_index))
                if token_snapshot == snapshot_id && token_index == index => {},
            _ => return Err(AppMacosHostError::InvalidHostResponse),
        }
        let role = project_observation_role(element.get("role"))?;
        let depth = element
            .get("depth")
            .and_then(Value::as_u64)
            .and_then(|depth| u16::try_from(depth).ok())
            .filter(|depth| *depth <= MAX_APP_MACOS_TREE_DEPTH)
            .ok_or(AppMacosHostError::InvalidHostResponse)?;
        // CUA may alias an editable control's current content into its label
        // or title rather than the explicit `value` attribute. Keep all free
        // text out of editable roles; the opaque ref, role and state remain
        // sufficient for the read-only V1 tree shape.
        let editable = app_macos_role_is_editable(&role);
        let label = project_observation_text(element, "label")?;
        let title = project_observation_text(element, "title")?;
        let value = project_observation_text(element, "value")?;
        let label = (!editable).then_some(label).flatten();
        let title = (!editable).then_some(title).flatten();
        let value = (!editable && app_macos_role_value_is_safe(&role))
            .then_some(value)
            .flatten();
        let enabled = element.get("enabled").and_then(Value::as_bool);
        let selected = element.get("selected").and_then(Value::as_bool);
        let element_ref = {
            // A public deterministic derivation from the evidence digest and
            // small integer index would let an app brute-force the physical
            // index. Generate an unguessable logical identity instead and keep
            // this association solely in the move-only owner projection.
            AppReference::parse(format!("macos-element:{}", Uuid::new_v4()))
                .map_err(|_| AppMacosHostError::Encoding)?
        };
        mappings.push((
            element_ref.clone(),
            AppMacosObservedElement {
                element_index: index,
                element_token: element_token.to_owned(),
            },
        ));
        nodes.push(AppMacosSanitizedNode {
            element_ref,
            role,
            label,
            title,
            value,
            enabled,
            selected,
            depth,
        });
    }
    if nodes.is_empty() {
        return Err(AppMacosHostError::InvalidHostResponse);
    }
    let digest_nodes = nodes
        .iter()
        .map(|node| AppMacosSemanticDigestNode {
            role: &node.role,
            label: node.label.as_deref(),
            title: node.title.as_deref(),
            value: node.value.as_deref(),
            enabled: node.enabled,
            selected: node.selected,
            depth: node.depth,
        })
        .collect::<Vec<_>>();
    let labels_digest = AppDigest::blake3_canonical_json(
        &serde_json::to_value(&digest_nodes).map_err(|_| AppMacosHostError::Encoding)?,
    )
    .map_err(|_| AppMacosHostError::Encoding)?;
    Ok(AppMacosProjectedElements {
        mappings,
        nodes,
        labels_digest,
    })
}

fn project_observation_role(role: Option<&Value>) -> Result<String, AppMacosHostError> {
    let role = role
        .and_then(Value::as_str)
        .filter(|role| {
            role.len() > 2
                && role.len() <= 64
                && role.starts_with("AX")
                && role.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    if role.to_ascii_lowercase().contains("secure") {
        return Err(AppMacosHostError::InvalidHostResponse);
    }
    Ok(role.to_owned())
}

/// Bounded free text of one structured field. A field larger than a whole
/// rendered tree line is malformed evidence and fails the observation; a
/// shorter one is sanitized and dropped when it is not safe semantic text.
fn project_observation_text(
    element: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<String>, AppMacosHostError> {
    let text = match element.get(name) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(_) | Value::Object(_)) => return Ok(None),
    };
    if text.len() > MAX_APP_MACOS_TREE_LINE_BYTES {
        return Err(AppMacosHostError::InvalidHostResponse);
    }
    Ok(sanitize_observation_semantic_text(&text))
}

fn sanitize_observation_semantic_text(value: &str) -> Option<String> {
    let value = value
        .trim_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, '-' | '[' | ']' | '(' | ')' | '"' | '\'')
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty()
        || value.len() > MAX_APP_MACOS_SEMANTIC_TEXT_BYTES
        || value.chars().any(char::is_control)
        || value.to_ascii_lowercase().contains("element_index")
    {
        None
    } else {
        Some(value)
    }
}

fn app_macos_role_value_is_safe(role: &str) -> bool {
    matches!(
        role,
        "AXButton"
            | "AXCheckBox"
            | "AXLink"
            | "AXMenuItem"
            | "AXRadioButton"
            | "AXStaticText"
            | "AXTab"
    )
}

fn app_macos_role_is_editable(role: &str) -> bool {
    matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox"
    )
}

fn project_observation_tree(raw_observation: &str) -> Result<String, AppMacosHostError> {
    let value: Value = serde_json::from_str(raw_observation)
        .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
    value
        .get("tree_markdown")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(AppMacosHostError::InvalidHostResponse)
}

fn project_observation_geometry(
    raw_observation: &str,
) -> Result<AppInteractiveGeometry, AppMacosHostError> {
    let value: Value = serde_json::from_str(raw_observation)
        .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
    let width = value
        .get("screenshot_width")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    let height = value
        .get("screenshot_height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    let geometry = AppInteractiveGeometry {
        width,
        height,
        scale_millis: 1_000,
    };
    geometry
        .validate()
        .map_err(AppMacosHostError::Interactive)?;
    Ok(geometry)
}

/// The observation screenshot's backing scale (CuaDriver `screenshot_scale`,
/// screenshot pixels per window point). Private owner evidence used only to
/// place a drag; absent when the reply carried no screenshot.
fn project_observation_screenshot_scale(
    raw_observation: &str,
) -> Result<Option<u16>, AppMacosHostError> {
    let value: Value = serde_json::from_str(raw_observation)
        .map_err(|_| AppMacosHostError::InvalidHostResponse)?;
    let Some(scale) = value.get("screenshot_scale") else {
        return Ok(None);
    };
    let millis = scale
        .as_f64()
        .filter(|scale| scale.is_finite())
        .map(|scale| (scale * 1_000.0).round())
        .filter(|millis| {
            *millis >= f64::from(APP_MACOS_HOST_MIN_SCREENSHOT_SCALE_MILLIS)
                && *millis <= f64::from(APP_MACOS_HOST_MAX_SCREENSHOT_SCALE_MILLIS)
        })
        .ok_or(AppMacosHostError::InvalidHostResponse)?;
    Ok(Some(millis as u16))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMacosHostResponse {
    schema: String,
    correlation_ref: String,
    effect_binding_digest: String,
    tcc_policy_digest: String,
    tcc_epoch: u64,
    outcome: String,
    #[serde(default)]
    observation: Option<String>,
    #[serde(default)]
    process_id: Option<u32>,
    #[serde(default)]
    window_id: Option<u32>,
}

impl AppMacosHostResponse {
    fn parse(
        bytes: &[u8],
        request: &SignedAppMacosHostRequest,
        result_byte_ceiling: u64,
        operation: AppMacosOperation,
    ) -> Result<Self, AppMacosHostError> {
        if bytes.is_empty()
            || bytes.len() > MAX_APP_MACOS_HOST_RESPONSE_BYTES
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > result_byte_ceiling
        {
            return Err(AppMacosHostError::InvalidHostResponse);
        }
        let response: Self =
            serde_json::from_slice(bytes).map_err(|_| AppMacosHostError::InvalidHostResponse)?;
        let expected_correlation = format!("app-macos-host:{}", request.claims.nonce);
        if response.schema != "magician.app-macos-host-result.v1"
            || response.correlation_ref != expected_correlation
            || response.effect_binding_digest != request.claims.effect_binding_digest
            || response.tcc_policy_digest != request.claims.tcc_policy_digest
            || response.tcc_epoch != request.claims.tcc_epoch
            || response.outcome != "completed"
            || response.observation.is_some()
                != matches!(
                    operation,
                    AppMacosOperation::Observe | AppMacosOperation::CapturePixels
                )
            || response.process_id.is_some() != response.observation.is_some()
            || response.window_id.is_some() != response.observation.is_some()
            || response.process_id.is_some_and(|value| value == 0)
            || response.window_id.is_some_and(|value| value == 0)
            || response.observation.as_ref().is_some_and(|value| {
                value.is_empty()
                    || value.len() > MAX_APP_MACOS_OBSERVATION_BYTES
                    || u64::try_from(value.len()).unwrap_or(u64::MAX)
                        > request.claims.evidence_byte_ceiling
                    || app_macos_host_observation_contains_secure_content(value)
            })
        {
            return Err(AppMacosHostError::InvalidHostResponse);
        }
        Ok(response)
    }
}

pub(crate) fn macos_action_implementation_plan_digest(
    source_digest: &AppDigest,
    _target_policy_digest: &AppDigest,
    _owner_profile_digest: &AppDigest,
    _owner_implementation_digest: &AppDigest,
    action: &AppInteractiveReviewedAction,
    operation: AppMacosOperation,
) -> Result<AppDigest, AppMacosHostError> {
    macos_locked_action_implementation_plan_digest(
        source_digest,
        operation,
        action.class(),
        action.input_schema_digest(),
        action.result_schema_digest(),
        action.result_byte_ceiling(),
        action.required_observation_kind(),
        action.invalidates_observation(),
    )
}

#[allow(clippy::too_many_arguments)]
fn macos_locked_action_implementation_plan_digest(
    source_digest: &AppDigest,
    operation: AppMacosOperation,
    class: super::interactive::AppInteractiveActionClass,
    input_schema_digest: &AppDigest,
    result_schema_digest: &AppDigest,
    result_byte_ceiling: u64,
    required_observation_kind: Option<AppInteractiveObservationKind>,
    invalidates_observation: bool,
) -> Result<AppDigest, AppMacosHostError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": APP_MACOS_HOST_PROFILE_V1,
        "implementation_revision": APP_MACOS_HOST_IMPLEMENTATION_REVISION,
        "owner_source_digest": macos_host_owner_source_digest()
            .ok_or(AppMacosHostError::Encoding)?,
        "source_digest": source_digest,
        "operation": operation,
        "class": class,
        "input_schema_digest": input_schema_digest,
        "result_schema_digest": result_schema_digest,
        "result_byte_ceiling": result_byte_ceiling,
        "required_observation_kind": required_observation_kind,
        "invalidates_observation": invalidates_observation,
    }))
    .map_err(|_| AppMacosHostError::Encoding)
}

#[allow(dead_code)] // Stable authoring/qualification identity helper.
pub(crate) fn macos_observe_implementation_plan_digest(
    source_digest: &AppDigest,
) -> Result<AppDigest, AppMacosHostError> {
    macos_operation_implementation_plan_digest(source_digest, AppMacosOperation::Observe)
}

pub fn macos_operation_implementation_plan_digest(
    source_digest: &AppDigest,
    operation: AppMacosOperation,
) -> Result<AppDigest, AppMacosHostError> {
    let action_name = match operation {
        AppMacosOperation::Launch => "launch",
        AppMacosOperation::Focus => "focus",
        AppMacosOperation::Observe => "snapshot",
        AppMacosOperation::ClickElement => "click",
        AppMacosOperation::TypeText => "type",
        AppMacosOperation::PressKey => "key",
        AppMacosOperation::ScrollElement => "scroll",
        AppMacosOperation::DragElements => "drag",
        AppMacosOperation::CapturePixels => return Err(AppMacosHostError::ActionNotReviewed),
    };
    let input =
        macos_action_input_schema(action_name).ok_or(AppMacosHostError::ActionNotReviewed)?;
    let result = if operation == AppMacosOperation::Observe {
        macos_observe_result_schema()
    } else {
        macos_action_result_schema()
    };
    macos_locked_action_implementation_plan_digest(
        source_digest,
        operation,
        operation.interactive_class(),
        &AppDigest::blake3_canonical_json(&input).map_err(|_| AppMacosHostError::Encoding)?,
        &AppDigest::blake3_canonical_json(&result).map_err(|_| AppMacosHostError::Encoding)?,
        if operation == AppMacosOperation::Observe {
            APP_MACOS_OBSERVE_RESULT_CEILING
        } else {
            APP_MACOS_ACTION_RESULT_CEILING
        },
        operation.required_observation_kind(),
        operation.invalidates_observation(),
    )
}

fn macos_host_owner_source_digest() -> Option<&'static AppDigest> {
    static DIGEST: OnceLock<Option<AppDigest>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            macos_host_owner_source_digest_from_parts(&[
                (
                    "cargo-package-version",
                    env!("CARGO_PKG_VERSION").as_bytes(),
                ),
                (
                    "runtime/apps/macos_host.rs",
                    include_bytes!("macos_host.rs"),
                ),
                (
                    "runtime/apps/interactive.rs",
                    include_bytes!("interactive.rs"),
                ),
                (
                    "runtime/apps/macos_pairing.rs",
                    include_bytes!("macos_pairing.rs"),
                ),
                (
                    "runtime/apps/macos_pairing_service.rs",
                    include_bytes!("macos_pairing_service.rs"),
                ),
                (
                    "contract/macos_host.rs",
                    include_bytes!("../../../../magician-app-contract/src/macos_host.rs"),
                ),
                (
                    "desktop/app_macos_host.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/app_macos_host.rs"),
                ),
                (
                    "desktop/app_macos_identity.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/app_macos_identity.rs"),
                ),
                (
                    "desktop/app_macos_pairing.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/app_macos_pairing.rs"),
                ),
                (
                    "desktop/host_gateway.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/host_gateway.rs"),
                ),
                (
                    "desktop/main.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/main.rs"),
                ),
                (
                    "desktop/ui/MacosAppPairing.svelte",
                    include_bytes!("../../../../desktop/src/lib/MacosAppPairing.svelte"),
                ),
                (
                    "desktop/ui/macosPairingUiModel.js",
                    include_bytes!("../../../../desktop/src/lib/macosPairingUiModel.js"),
                ),
                (
                    "desktop/ui/Settings.svelte",
                    include_bytes!("../../../../desktop/src/lib/Settings.svelte"),
                ),
            ])
        })
        .as_ref()
}

fn macos_host_owner_source_digest_from_parts(parts: &[(&str, &[u8])]) -> Option<AppDigest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.app-macos-host-owner-source.v2\0");
    for (path, bytes) in parts {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex())).ok()
}

pub(crate) fn macos_host_owner_implementation_digest(
    cua_driver_binary_digest: &AppDigest,
) -> Result<AppDigest, AppMacosHostError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": APP_MACOS_HOST_PROFILE_V1,
        "owner_source_digest": macos_host_owner_source_digest()
            .ok_or(AppMacosHostError::Encoding)?,
        "cua_driver_binary_digest": cua_driver_binary_digest,
    }))
    .map_err(|_| AppMacosHostError::Encoding)
}

pub(crate) fn macos_host_owner_profile_digest(
    pairing: &AppMacosHostPairing,
) -> Result<AppDigest, AppMacosHostError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "profile": APP_MACOS_HOST_PROFILE_V1,
        "desktop_identity_digest": &pairing.desktop_identity_digest,
        "desktop_identity_attestation_digest": &pairing.desktop_identity_attestation_digest,
        "host_identity_digest": &pairing.host_identity_digest,
        "gateway_endpoint_digest": &pairing.gateway_endpoint_digest,
        "tcc_policy_digest": &pairing.tcc_policy_digest,
        "tcc_epoch": pairing.tcc_epoch,
    }))
    .map_err(|_| AppMacosHostError::Encoding)
}

fn validate_bundle_id(value: &str) -> Result<(), AppMacosHostError> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('.')
        || value.ends_with('.')
        || !value.contains('.')
        || value.split('.').any(str::is_empty)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(AppMacosHostError::InvalidTargetPolicy);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AppMacosHostError {
    #[error("macOS target policy is empty, duplicated or malformed")]
    InvalidTargetPolicy,
    #[error("macOS target is protected and unavailable to Apps")]
    ProtectedTarget,
    #[error("macOS physical target is invalid")]
    InvalidPhysicalTarget,
    #[error("macOS canonical action input is malformed or does not match the reviewed schema")]
    InvalidCanonicalInput,
    #[error("macOS physical operation is unavailable in the current paired owner")]
    PhysicalOperationUnavailable,
    #[error("macOS action requires an exact owner-held observation")]
    ObservationRequired,
    #[error("macOS physical owner identity does not match reviewed/current evidence")]
    IdentityMismatch,
    #[error("macOS action was not reviewed")]
    ActionNotReviewed,
    #[error("macOS runtime/desktop pairing is invalid")]
    InvalidPairing,
    #[error("macOS interactive permit expired before host dispatch")]
    StalePermit,
    #[error("macOS action was cancelled after durable dispatch-start")]
    CancelledAfterDispatchStart,
    #[error("macOS host transport failed after dispatch-start: {0:?}")]
    Transport(AppMacosHostTransportError),
    #[error("macOS host response is malformed, mismatched, oversized or unsafe")]
    InvalidHostResponse,
    #[error("failed to encode macOS owner identity")]
    Encoding,
    #[error(transparent)]
    Interactive(#[from] super::interactive::AppInteractiveError),
}

#[cfg(test)]
mod tests {
    use static_assertions::assert_not_impl_any;

    use super::*;

    assert_not_impl_any!(AppMacosHostPairing: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosHttpHostTransport: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosResolvedTarget: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosOwnerObservation: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosObservationProjection: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosPreparedAction: Clone, Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(AppMacosCompletedAction: Clone, Serialize, serde::de::DeserializeOwned);

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("reference")
    }

    fn digest(value: u8) -> AppDigest {
        AppDigest::parse(format!("blake3:{}", format!("{value:02x}").repeat(32))).expect("digest")
    }

    #[test]
    fn protected_apps_and_ambient_action_shapes_are_unrepresentable() {
        assert!(matches!(
            AppMacosReviewedTarget::from_owner_review(
                reference("macos-target:settings"),
                "com.apple.systempreferences".to_owned(),
                digest(1),
            )
            .unwrap_err(),
            AppMacosHostError::ProtectedTarget
        ));
        let action = AppMacosHostAction::PressKey {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 1,
            window_id: 2,
            observation_ref: "interactive-observation:1".to_owned(),
            key: magician_app_contract::macos_host::AppMacosHostKey::Return,
            modifiers: Vec::new(),
        };
        let value = serde_json::to_value(action).expect("serialize");
        let object = value.as_object().expect("object");
        for forbidden in [
            "action_name",
            "args_json",
            "script",
            "selector",
            "path",
            "argv",
        ] {
            assert!(!object.contains_key(forbidden));
        }

        assert!(
            serde_json::from_value::<AppMacosCanonicalInput>(serde_json::json!({
                "operation": "click_element",
                "observation_ref": "interactive-observation:logical",
                "element_ref": "macos-element:send",
                "element_index": 3,
                "click_count": 1,
            }))
            .is_err()
        );
    }

    /// A CuaDriver 0.28 `get_window_state` reply in its real shape (see the
    /// structured `elements[]`, top-level `snapshot_id` and `[N]` Markdown).
    fn cua_0_28_window_state(elements: Value, tree_markdown: &str) -> String {
        serde_json::json!({
            "app_name": "Editor",
            "element_count": elements.as_array().map_or(0, Vec::len),
            "elements": elements,
            "elements_complete": true,
            "pid": 42,
            "screenshot_height": 720,
            "screenshot_mime_type": "image/png",
            "screenshot_scale": 2.0,
            "screenshot_width": 1280,
            "snapshot_id": "s00000006",
            "tree_markdown": tree_markdown,
            "window_bounds": {"height": 360.0, "width": 640.0, "x": 546.0, "y": 132.0},
            "window_id": 9,
            "window_title": "Draft"
        })
        .to_string()
    }

    #[test]
    fn observation_projection_is_usable_bounded_and_keeps_physical_ids_private() {
        let raw = cua_0_28_window_state(
            serde_json::json!([
                {
                    "actions": ["AXPress"], "depth": 1, "element_index": 3,
                    "element_token": "s00000006:3", "enabled": true,
                    "frame": {"h": 26.0, "w": 80.0, "x": 560.0, "y": 150.0},
                    "label": "Send message", "parent_index": 0, "role": "AXButton"
                },
                {
                    "depth": 2, "element_index": 9, "element_token": "s00000006:9",
                    "enabled": true, "frame": {"h": 22.0, "w": 300.0, "x": 560.0, "y": 200.0},
                    "label": "Draft", "value": "private draft", "parent_index": 3,
                    "role": "AXTextField"
                }
            ]),
            concat!(
                "  - [3] AXButton (Send message) [actions=[press]]\n",
                "    - [9] AXTextField = \"private draft\" (Draft)"
            ),
        );
        let evidence_digest = AppDigest::blake3(raw.as_bytes());
        let projected = project_observation_elements(&raw).expect("projection");
        assert_eq!(projected.mappings.len(), 2);
        assert_eq!(projected.mappings[0].1.element_index, 3);
        assert_eq!(projected.mappings[0].1.element_token, "s00000006:3");
        assert_eq!(projected.mappings[1].1.element_index, 9);
        assert_eq!(projected.nodes[0].role, "AXButton");
        assert_eq!(projected.nodes[0].label.as_deref(), Some("Send message"));
        assert_eq!(projected.nodes[0].depth, 1);
        assert_eq!(projected.nodes[1].depth, 2);
        assert_eq!(projected.nodes[1].label, None);
        assert_eq!(projected.nodes[1].title, None);
        assert_eq!(projected.nodes[1].value, None);
        assert_eq!(
            project_observation_screenshot_scale(&raw).expect("scale"),
            Some(2_000)
        );

        let public = serde_json::to_value(AppMacosCanonicalResult::StructuredObservation {
            schema: "magician.app-macos-result.v1",
            operation: AppMacosOperation::Observe,
            target_ref: &reference("macos-target:editor"),
            tcc_policy_digest: &digest(7),
            tcc_epoch: 1,
            evidence_digest: &evidence_digest,
            observation_ref: &reference("macos-observation:fixture"),
            geometry: &project_observation_geometry(&raw).expect("geometry"),
            nodes: &projected.nodes,
        })
        .expect("public result");
        let encoded = public.to_string();
        assert!(encoded.contains("AXButton"));
        assert!(encoded.contains("Send message"));
        for forbidden in [
            "element_index",
            "element_token",
            "s00000006",
            "process_id",
            "window_id",
            "Draft",
            "private draft",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn observation_projection_rejects_oversized_semantics_and_secure_roles() {
        let element = |role: &str, label: String| {
            serde_json::json!([{
                "depth": 0, "element_index": 3, "element_token": "s00000006:3",
                "label": label, "role": role
            }])
        };
        let oversized = cua_0_28_window_state(
            element("AXButton", "x".repeat(MAX_APP_MACOS_TREE_LINE_BYTES + 1)),
            "- [3] AXButton",
        );
        assert!(project_observation_elements(&oversized).is_err());

        let secure = cua_0_28_window_state(
            element("AXSecureTextField", "Password".to_owned()),
            "- [3] AXSecureTextField (Password)",
        );
        assert!(project_observation_elements(&secure).is_err());
    }

    #[test]
    fn observation_projection_rejects_tokens_not_minted_by_the_reply_snapshot() {
        for (index, token) in [(3, "s00000005:3"), (3, "s00000006:4"), (3, "3")] {
            let raw = cua_0_28_window_state(
                serde_json::json!([{
                    "depth": 0, "element_index": index, "element_token": token,
                    "label": "Send", "role": "AXButton"
                }]),
                "- [3] AXButton (Send)",
            );
            assert!(project_observation_elements(&raw).is_err(), "{token}");
        }

        // Markdown `[N]` markers alone are presentation, not addressable rows.
        let markdown_only = cua_0_28_window_state(
            serde_json::json!([]),
            "- [3] AXButton (Send) [actions=[press]]",
        );
        assert!(project_observation_elements(&markdown_only).is_err());
    }

    #[test]
    fn permits_fence_the_named_elements_and_menu_targets_fail_closed() {
        let tree = "- [0] AXWindow \"Untitled 3\"\n  - [1] AXScrollArea\n    \
                    - [2] AXTextArea \"seed\"\n  - [3] AXButton \"Edited\"\n\
                    - [4] AXMenuBar\n  - [5] AXMenuBarItem \"Edit\"";
        let observation_ref = format!("interactive-observation:{}", "01".repeat(32));
        let click = |token: &str| AppMacosHostAction::ClickElement {
            bundle_id: "com.apple.TextEdit".to_owned(),
            process_id: 7,
            window_id: 9,
            observation_ref: observation_ref.clone(),
            element_token: token.to_owned(),
            click_count: 1,
        };
        let digest = |input: String| AppDigest::blake3(input.as_bytes());
        let fence = |index| {
            magician_app_contract::macos_host::app_macos_host_element_fence(tree, index)
                .expect("fence")
        };
        assert_eq!(
            app_macos_owner_fence_digest(tree, &click("s0000000a:2")).expect("click"),
            digest(fence(2)),
        );
        // A retitle leaves the permit's fence digest valid.
        let retitled = tree
            .replace("\"Untitled 3\"", "\"Live Seed 3\"")
            .replace("\"Edited\"", "\"Suggested\"");
        assert_eq!(
            app_macos_owner_fence_digest(&retitled, &click("s0000000a:2")).expect("retitled"),
            digest(fence(2)),
        );
        let drag = AppMacosHostAction::DragElements {
            bundle_id: "com.apple.TextEdit".to_owned(),
            process_id: 7,
            window_id: 9,
            observation_ref: observation_ref.clone(),
            source_element_token: "s0000000a:2".to_owned(),
            destination_element_token: "s0000000a:3".to_owned(),
            screenshot_scale_millis: 2_000,
        };
        assert_eq!(
            app_macos_owner_fence_digest(tree, &drag).expect("drag"),
            digest(format!("{}\n\u{0}\n{}", fence(2), fence(3))),
        );
        let key = AppMacosHostAction::PressKey {
            bundle_id: "com.apple.TextEdit".to_owned(),
            process_id: 7,
            window_id: 9,
            observation_ref: observation_ref.clone(),
            key: AppMacosHostKey::Return,
            modifiers: Vec::new(),
        };
        assert_eq!(
            app_macos_owner_fence_digest(tree, &key).expect("key"),
            digest("[0] AXWindow\nAXScrollArea\nAXButton".to_owned()),
        );
        // Menu chrome and absent elements have no fence: refused, never sent.
        for token in ["s0000000a:5", "s0000000a:4", "s0000000a:99"] {
            assert!(matches!(
                app_macos_owner_fence_digest(tree, &click(token)),
                Err(AppMacosHostError::InvalidPhysicalTarget)
            ));
        }
    }

    #[test]
    fn typed_transport_refuses_ambient_or_redirectable_endpoints() {
        let local = AppMacosHttpHostTransport::from_pairing_owner(
            Url::parse("http://127.0.0.1:3017/host/apps/macos/action").expect("local URL"),
        )
        .expect("paired transport");
        assert!(local.endpoint_digest().as_str().starts_with("blake3:"));

        for denied in [
            "https://host.docker.internal:3017/host/apps/macos/action",
            "http://host.docker.internal:3017/host/apps/macos/action",
            "http://localhost:3017/host/apps/macos/action",
            "http://example.com:3017/host/apps/macos/action",
            "http://127.0.0.1:3017/host/ax/click",
            "http://user@127.0.0.1:3017/host/apps/macos/action",
            "http://127.0.0.1:3017/host/apps/macos/action?next=evil",
        ] {
            assert!(AppMacosHttpHostTransport::from_pairing_owner(
                Url::parse(denied).expect("URL")
            )
            .is_err());
        }
    }

    #[test]
    fn owner_source_digest_changes_for_identity_or_authority_bytes() {
        let baseline = macos_host_owner_source_digest_from_parts(&[
            ("desktop/app_macos_identity.rs", b"identity-v1"),
            ("runtime/apps/macos_pairing.rs", b"pairing-v1"),
        ])
        .expect("baseline digest");
        let identity_changed = macos_host_owner_source_digest_from_parts(&[
            ("desktop/app_macos_identity.rs", b"identity-v2"),
            ("runtime/apps/macos_pairing.rs", b"pairing-v1"),
        ])
        .expect("changed digest");
        let path_changed = macos_host_owner_source_digest_from_parts(&[
            ("desktop/renamed_identity.rs", b"identity-v1"),
            ("runtime/apps/macos_pairing.rs", b"pairing-v1"),
        ])
        .expect("renamed digest");
        assert_ne!(baseline, identity_changed);
        assert_ne!(baseline, path_changed);
    }
}
