//! App-safe Android interactive owner.
//!
//! The legacy Android packs accept a raw device id, coordinates, text and
//! package names. Apps must never inherit that ambient transport vocabulary.
//! This module resolves one owner-reviewed opaque pairing target, binds the
//! exact authenticated socket generation, and lowers only closed typed actions
//! through the shared interactive/effect typestate. The first vertical is a
//! bounded structured snapshot; mutation, screenshot and app-lifecycle verbs
//! remain non-activatable until their point-of-use device fences are reviewed.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{Arc, OnceLock},
    time::Duration,
};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

use super::{
    android_owner::{global_android_owner_store, AppAndroidOwnerSnapshotFence},
    android_owner_bootstrap::revalidate_owner_status,
    effect_kernel::{
        AppEffectBinding, AppEffectInFlight, AppEffectKernelError, AppEffectPhysicalOwner,
        AppEffectPhysicalTarget, AppEffectSettlement, AppEffectStage,
    },
    interactive::{
        AppInteractiveActionClass, AppInteractiveActionPermit, AppInteractiveBackgroundPosture,
        AppInteractiveCancellation, AppInteractiveCancellationReason, AppInteractiveCurrentFence,
        AppInteractiveEffectInspection, AppInteractiveEffectPermit, AppInteractiveError,
        AppInteractiveExecutionProfile, AppInteractiveGeometry, AppInteractiveGrantDescriptor,
        AppInteractiveObservationKind, AppInteractiveObservationRef,
        AppInteractiveResourceCeilings, AppInteractiveResourceClaim, AppInteractiveReviewedAction,
        AppInteractiveSessionHandle, AppInteractiveSettlementReceipt,
    },
    models::{AppDigest, AppReference},
    package_lock::{AppLockedPrimitiveActionBinding, AppLockedPrimitiveBinding},
    tool_disclosure::AttestedAppToolTarget,
};
use crate::magician_v2::{
    device_bridge::{
        DeviceBridgeError, DeviceBridgeHub, DeviceKey, DeviceOwnerConnection,
        DEFAULT_DEVICE_ACTION_TIMEOUT,
    },
    device_governance::{DeviceActionAudit, DeviceActionRecord, DeviceActionVerdict},
    device_pairing::{DeviceAutomationReview, DevicePairingStore, PairingError},
    execution::actions::ActionResult,
    json_traversal::canonical_json_bytes,
};

pub(crate) const APP_ANDROID_DEVICE_PROFILE_V1: &str = "magician.app-android-device-owner.v1";
pub(crate) const APP_ANDROID_DEVICE_IMPLEMENTATION_REVISION: &str =
    "magician.app-android-device-implementation.2026-08-22.7";
pub const APP_ANDROID_SNAPSHOT_RESULT_CEILING: u64 = 1024 * 1024;
const APP_ANDROID_SNAPSHOT_EVIDENCE_CEILING: u64 = 512 * 1024;
const APP_ANDROID_SNAPSHOT_NODE_CEILING: u64 = 4_096;
const APP_ANDROID_OBSERVATION_TTL_SECONDS: i64 = 30;
const APP_ANDROID_OWNER_WIRE_SCHEMA: &str = "magician.android-app-owner.v1";
const APP_ANDROID_OWNER_PROTOCOL: &str = "2026-07-28";
const APP_ANDROID_OWNER_SERVER_NAME: &str = "magdroid";
const APP_ANDROID_OWNER_SERVER_VERSION: &str = "1.1.0";
const APP_ANDROID_WIRE_ACTION_SNAPSHOT: &str = "android_get_ui_tree";
const APP_ANDROID_WIRE_ACTION_SCREENSHOT: &str = "android_screenshot";
const APP_ANDROID_WIRE_ACTION_TAP: &str = "android_tap";
const APP_ANDROID_WIRE_ACTION_TYPE: &str = "android_input_text";
const APP_ANDROID_WIRE_ACTION_KEY: &str = "android_press_key";
const APP_ANDROID_WIRE_ACTION_SCROLL: &str = "android_swipe";
const APP_ANDROID_WIRE_ACTION_LAUNCH: &str = "android_launch_app";
const APP_ANDROID_WIRE_ACTION_CLOSE: &str = "android_close_app";
pub const APP_ANDROID_ACTION_RESULT_CEILING: u64 = 32 * 1024;
pub const APP_ANDROID_SCREENSHOT_RESULT_CEILING: u64 = 8 * 1024 * 1024;
const APP_ANDROID_SCREENSHOT_EVIDENCE_CEILING: u64 = 4 * 1024 * 1024;
const MAX_APP_ANDROID_PACKAGES: usize = 64;
const MAX_APP_ANDROID_PACKAGE_BYTES: usize = 255;
const MAX_APP_ANDROID_SEMANTIC_FIELD_CHARS: usize = 512;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidTargetPolicy {
    device_target_ref: AppReference,
    review_generation: u64,
    review_digest: AppDigest,
    allowed_packages: BTreeSet<String>,
    digest: AppDigest,
}

impl AppAndroidTargetPolicy {
    pub(crate) fn from_owner_review(
        review: &DeviceAutomationReview,
    ) -> Result<Self, AppAndroidDeviceError> {
        if review.actions.as_slice()
            != crate::magician_v2::device_pairing::DEVICE_AUTOMATION_ACTION_ROSTER.as_slice()
        {
            return Err(AppAndroidDeviceError::InvalidTargetPolicy);
        }
        Self::reviewed(
            AppReference::parse(review.target_ref.clone())
                .map_err(|_| AppAndroidDeviceError::InvalidTargetPolicy)?,
            review.generation,
            AppDigest::parse(review.review_digest.clone())
                .map_err(|_| AppAndroidDeviceError::InvalidTargetPolicy)?,
            review.allowed_packages.iter().cloned().collect(),
        )
    }

    pub(crate) fn reviewed(
        device_target_ref: AppReference,
        review_generation: u64,
        review_digest: AppDigest,
        allowed_packages: BTreeSet<String>,
    ) -> Result<Self, AppAndroidDeviceError> {
        if !device_target_ref.as_str().starts_with("android-device:")
            || review_generation == 0
            || allowed_packages.is_empty()
            || allowed_packages.len() > MAX_APP_ANDROID_PACKAGES
            || allowed_packages
                .iter()
                .any(|package| !valid_android_package(package))
        {
            return Err(AppAndroidDeviceError::InvalidTargetPolicy);
        }
        let digest = AppDigest::blake3_canonical_json(&json!({
            "schema": APP_ANDROID_DEVICE_PROFILE_V1,
            "device_target_ref": &device_target_ref,
            "review_generation": review_generation,
            "review_digest": &review_digest,
            "allowed_packages": &allowed_packages,
            "protected_app_enforcement": "device_point_of_use",
            "raw_device_ids": false,
        }))
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
        Ok(Self {
            device_target_ref,
            review_generation,
            review_digest,
            allowed_packages,
            digest,
        })
    }

    pub fn device_target_ref(&self) -> &AppReference {
        &self.device_target_ref
    }

    pub fn allowed_packages(&self) -> &BTreeSet<String> {
        &self.allowed_packages
    }

    pub fn review_generation(&self) -> u64 {
        self.review_generation
    }

    pub fn review_digest(&self) -> &AppDigest {
        &self.review_digest
    }

    pub fn digest(&self) -> &AppDigest {
        &self.digest
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppAndroidAction {
    Snapshot,
    Screenshot {
        package: String,
    },
    Launch {
        package: String,
    },
    Close {
        package: String,
    },
    Tap {
        observation_ref: AppReference,
        element_ref: AppReference,
    },
    Type {
        observation_ref: AppReference,
        text: String,
    },
    Key {
        observation_ref: AppReference,
        key: AppAndroidKey,
    },
    Scroll {
        observation_ref: AppReference,
        direction: AppAndroidScrollDirection,
        duration_millis: u16,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidKey {
    Back,
    Home,
    Enter,
    Delete,
    Tab,
    Escape,
    Space,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl AppAndroidKey {
    fn wire_label(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Home => "home",
            Self::Enter => "enter",
            Self::Delete => "delete",
            Self::Tab => "tab",
            Self::Escape => "escape",
            Self::Space => "space",
        }
    }
}

impl AppAndroidAction {
    fn resource_claim(&self) -> AppInteractiveResourceClaim {
        match self {
            Self::Snapshot => AppInteractiveResourceClaim {
                evidence_bytes: APP_ANDROID_SNAPSHOT_EVIDENCE_CEILING,
                evidence_nodes: APP_ANDROID_SNAPSHOT_NODE_CEILING,
                pixels: 0,
                artifact_bytes: 0,
                output_bytes: APP_ANDROID_SNAPSHOT_RESULT_CEILING,
            },
            Self::Screenshot { .. } => AppInteractiveResourceClaim {
                evidence_bytes: APP_ANDROID_SCREENSHOT_EVIDENCE_CEILING,
                evidence_nodes: 0,
                pixels: 1280 * 720,
                artifact_bytes: 0,
                output_bytes: APP_ANDROID_SCREENSHOT_RESULT_CEILING,
            },
            Self::Launch { .. }
            | Self::Close { .. }
            | Self::Tap { .. }
            | Self::Type { .. }
            | Self::Key { .. }
            | Self::Scroll { .. } => AppInteractiveResourceClaim {
                evidence_bytes: 16 * 1024,
                evidence_nodes: 1,
                pixels: 0,
                artifact_bytes: 0,
                output_bytes: APP_ANDROID_ACTION_RESULT_CEILING,
            },
        }
    }

    pub(crate) fn action_name(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Screenshot { .. } => "screenshot",
            Self::Launch { .. } => "launch",
            Self::Close { .. } => "close",
            Self::Tap { .. } => "tap",
            Self::Type { .. } => "type",
            Self::Key { .. } => "key",
            Self::Scroll { .. } => "scroll",
        }
    }

    fn requires_observation(&self) -> bool {
        matches!(
            self,
            Self::Tap { .. } | Self::Type { .. } | Self::Key { .. } | Self::Scroll { .. }
        )
    }

    fn observation_ref(&self) -> Option<&AppReference> {
        match self {
            Self::Tap {
                observation_ref, ..
            }
            | Self::Type {
                observation_ref, ..
            }
            | Self::Key {
                observation_ref, ..
            }
            | Self::Scroll {
                observation_ref, ..
            } => Some(observation_ref),
            Self::Snapshot | Self::Screenshot { .. } | Self::Launch { .. } | Self::Close { .. } => {
                None
            },
        }
    }
}

fn validate_android_input(
    action: &AppAndroidAction,
    policy: &AppAndroidTargetPolicy,
) -> Result<(), AppAndroidDeviceError> {
    let package = match action {
        AppAndroidAction::Screenshot { package }
        | AppAndroidAction::Launch { package }
        | AppAndroidAction::Close { package } => Some(package),
        _ => None,
    };
    if package.is_some_and(|package| !policy.allowed_packages().contains(package)) {
        return Err(AppAndroidDeviceError::InvalidInput);
    }
    match action {
        AppAndroidAction::Type { text, .. }
            if text.is_empty() || text.len() > 16 * 1024 || text.contains('\0') =>
        {
            Err(AppAndroidDeviceError::InvalidInput)
        },
        AppAndroidAction::Scroll {
            duration_millis, ..
        } if !(100..=900).contains(duration_millis) => Err(AppAndroidDeviceError::InvalidInput),
        _ => Ok(()),
    }
}

pub fn android_action_input_schema(action_name: &str) -> Option<Value> {
    let observation = json!({"type":"string","maxLength":192});
    match action_name {
        "snapshot" => Some(android_snapshot_input_schema()),
        "screenshot" => Some(json!({
            "type":"object","additionalProperties":false,"required":["action","package"],
            "properties":{
                "action":{"const":"screenshot"},
                "package":{"type":"string","minLength":3,"maxLength":255}
            }
        })),
        "launch" | "close" => Some(json!({
            "type":"object","additionalProperties":false,"required":["action","package"],
            "properties":{
                "action":{"const":action_name},
                "package":{"type":"string","minLength":3,"maxLength":255}
            }
        })),
        "tap" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","element_ref"],
            "properties":{
                "action":{"const":"tap"},"observation_ref":observation,
                "element_ref":{"type":"string","maxLength":192}
            }
        })),
        "type" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","text"],
            "properties":{
                "action":{"const":"type"},"observation_ref":observation,
                "text":{"type":"string","minLength":1,"maxLength":16384}
            }
        })),
        "key" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","key"],
            "properties":{
                "action":{"const":"key"},"observation_ref":observation,
                "key":{"enum":["back","home","enter","delete","tab","escape","space"]}
            }
        })),
        "scroll" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","direction","duration_millis"],
            "properties":{
                "action":{"const":"scroll"},"observation_ref":observation,
                "direction":{"enum":["up","down","left","right"]},
                "duration_millis":{"type":"integer","minimum":100,"maximum":900}
            }
        })),
        _ => None,
    }
}

pub fn android_action_result_schema(action_name: &str) -> Option<Value> {
    if action_name == "snapshot" {
        return Some(android_snapshot_result_schema());
    }
    if action_name == "screenshot" {
        return Some(json!({
            "type":"object","additionalProperties":false,
            "required":["kind","success","foreground_package","content_digest","width","height","mime_type","image_base64"],
            "properties":{
                "kind":{"const":"app_android_screenshot"},"success":{"const":true},
                "foreground_package":{"type":"string","maxLength":255},
                "content_digest":{"type":"string","maxLength":71},
                "width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},
                "mime_type":{"const":"image/jpeg"},
                "image_base64":{"type":"string","maxLength":5592408}
            }
        }));
    }
    matches!(action_name, "launch" | "close" | "tap" | "type" | "key" | "scroll")
        .then(|| json!({
            "type":"object","additionalProperties":false,
            "required":["kind","success","action","target_package","foreground_package","receipt_digest"],
            "properties":{
                "kind":{"const":"app_android_action"},"success":{"const":true},
                "action":{"const":action_name},
                "target_package":{"type":"string","maxLength":255},
                "foreground_package":{"type":["string","null"],"maxLength":255},
                "receipt_digest":{"type":"string","maxLength":71}
            }
        }))
}

pub(crate) fn android_snapshot_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action"],
        "properties": {"action": {"const": "snapshot"}}
    })
}

pub(crate) fn android_snapshot_result_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "kind", "success", "device_target_ref", "foreground_package", "geometry",
            "content_digest", "observation_ref", "nodes", "total_nodes", "truncated"
        ],
        "properties": {
            "kind": {"const": "app_android_snapshot"},
            "success": {"const": true},
            "device_target_ref": {"type": "string", "maxLength": 192},
            "foreground_package": {"type": "string", "maxLength": 255},
            "geometry": {
                "type": "object", "additionalProperties": false,
                "required": ["width", "height", "scale_millis"],
                "properties": {
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1},
                    "scale_millis": {"const": 1000}
                }
            },
            "content_digest": {"type": "string"},
            "observation_ref": {"type": "string", "maxLength": 192},
            "nodes": {
                "type": "array",
                "maxItems": 4096,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "element_ref", "text", "description", "clickable",
                        "focusable", "scrollable", "checkable"
                    ],
                    "properties": {
                        "element_ref": {"type": "string", "maxLength": 192},
                        "text": {"type": ["string", "null"], "maxLength": 512},
                        "description": {"type": ["string", "null"], "maxLength": 512},
                        "clickable": {"type": "boolean"},
                        "focusable": {"type": "boolean"},
                        "scrollable": {"type": "boolean"},
                        "checkable": {"type": "boolean"},
                        "bounds": {
                            "type": ["object", "null"],
                            "additionalProperties": false,
                            "required": ["left", "top", "right", "bottom"],
                            "properties": {
                                "left": {"type": "integer", "minimum": 0},
                                "top": {"type": "integer", "minimum": 0},
                                "right": {"type": "integer", "minimum": 1},
                                "bottom": {"type": "integer", "minimum": 1}
                            }
                        }
                    }
                }
            },
            "total_nodes": {"type": "integer", "minimum": 0, "maximum": 4096},
            "truncated": {"type": "boolean"}
        }
    })
}

#[allow(dead_code)] // Compatibility constructor for snapshot-only reviewed packages.
pub(crate) fn android_snapshot_resource_ceilings(
    max_duration_seconds: u64,
) -> Result<AppInteractiveResourceCeilings, AppAndroidDeviceError> {
    android_action_resource_ceilings("snapshot", max_duration_seconds)
}

#[allow(dead_code)] // Publicly reviewed action metadata helper.
pub(crate) fn android_action_resource_ceilings(
    action_name: &str,
    max_duration_seconds: u64,
) -> Result<AppInteractiveResourceCeilings, AppAndroidDeviceError> {
    let (_, result, evidence, nodes) =
        android_action_wire_contract(action_name).ok_or(AppAndroidDeviceError::InvalidInput)?;
    let pixels = if action_name == "screenshot" {
        4096_u64 * 4096
    } else {
        0
    };
    AppInteractiveResourceCeilings::reviewed(
        1,
        1,
        max_duration_seconds.min(300),
        evidence,
        nodes,
        pixels,
        0,
        result,
    )
    .map_err(Into::into)
}

pub(crate) fn app_android_owner_profile_digest() -> Result<AppDigest, AppAndroidDeviceError> {
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_ANDROID_DEVICE_PROFILE_V1,
        "protocol_version": APP_ANDROID_OWNER_PROTOCOL,
        "server_name": APP_ANDROID_OWNER_SERVER_NAME,
        "server_version": APP_ANDROID_OWNER_SERVER_VERSION,
        "background": AppInteractiveBackgroundPosture::DirectOwner,
        "connection_generation_bound": true,
        "socket_generation_proof": "pinned_android_keystore_p256",
        "enrollment_attestation": "android_key_attestation_verified_boot_v1",
        "artifact_authority": "server_decoded_play_integrity_standard_token_per_socket_v1",
        "point_of_use_protected_app_gate": true,
        "raw_device_ids": false,
        "raw_selectors": false,
    }))
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))
}

pub(crate) fn app_android_runtime_implementation_digest() -> AppDigest {
    static DIGEST: OnceLock<AppDigest> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            for (path, bytes) in [
                (
                    "revision",
                    APP_ANDROID_DEVICE_IMPLEMENTATION_REVISION.as_bytes(),
                ),
                ("build/Cargo.toml", include_bytes!("../../../../Cargo.toml")),
                ("build/Cargo.lock", include_bytes!("../../../../Cargo.lock")),
                (
                    "build/magician-api.Cargo.toml",
                    include_bytes!("../../../../magician-api/Cargo.toml"),
                ),
                (
                    "build/magician.Cargo.toml",
                    include_bytes!("../../../Cargo.toml"),
                ),
                (
                    "runtime/apps/android_device.rs",
                    include_bytes!("android_device.rs"),
                ),
                (
                    "runtime/apps/android_owner.rs",
                    include_bytes!("android_owner.rs"),
                ),
                (
                    "runtime/apps/android_owner_bootstrap.rs",
                    include_bytes!("android_owner_bootstrap.rs"),
                ),
                (
                    "runtime/apps/interactive.rs",
                    include_bytes!("interactive.rs"),
                ),
                (
                    "runtime/device_bridge.rs",
                    include_bytes!("../device_bridge.rs"),
                ),
                (
                    "runtime/device_pairing.rs",
                    include_bytes!("../device_pairing.rs"),
                ),
                (
                    "runtime/device_governance.rs",
                    include_bytes!("../device_governance.rs"),
                ),
                ("runtime/config.rs", include_bytes!("../../config.rs")),
                (
                    "api/android_apps_attestation.rs",
                    include_bytes!("../../../../magician-api/src/android_apps_attestation.rs"),
                ),
                (
                    "api/android_play_integrity.rs",
                    include_bytes!("../../../../magician-api/src/android_play_integrity.rs"),
                ),
                (
                    "api/android_apps_owner_api.rs",
                    include_bytes!("../../../../magician-api/src/android_apps_owner_api.rs"),
                ),
                (
                    "contract/android_owner.rs",
                    include_bytes!("../../../../magician-app-contract/src/android_owner.rs"),
                ),
                (
                    "api/device_bridge_handler.rs",
                    include_bytes!("../../../../magician-api/src/device_bridge_handler.rs"),
                ),
                (
                    "api/device_pairing_api.rs",
                    include_bytes!("../../../../magician-api/src/device_pairing_api.rs"),
                ),
                (
                    "runtime/cloudflare_access.rs",
                    include_bytes!("../cloudflare_access.rs"),
                ),
                (
                    "api/route_wiring.rs",
                    include_bytes!("../../../../magician-bin/src/main.rs"),
                ),
                (
                    "desktop/app_android_authority.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/app_android_authority.rs"),
                ),
                (
                    "desktop/app_macos_identity.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/app_macos_identity.rs"),
                ),
                (
                    "desktop/host_gateway.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/host_gateway.rs"),
                ),
                (
                    "desktop/native_command_wiring.rs",
                    include_bytes!("../../../../desktop/src-tauri/src/main.rs"),
                ),
                (
                    "desktop/Settings.svelte",
                    include_bytes!("../../../../desktop/src/lib/Settings.svelte"),
                ),
                (
                    "desktop/AndroidAppsAuthority.svelte",
                    include_bytes!("../../../../desktop/src/lib/AndroidAppsAuthority.svelte"),
                ),
                (
                    "owner-ui/devicePairing.ts",
                    include_bytes!("../../../../ui/unified-ui/src/lib/devices/devicePairing.ts"),
                ),
                (
                    "owner-ui/DevicePairingPanel.svelte",
                    include_bytes!(
                        "../../../../ui/unified-ui/src/lib/devices/DevicePairingPanel.svelte"
                    ),
                ),
                (
                    "android/app-build.gradle.kts",
                    include_bytes!("../../../../magdroid/android/app/build.gradle.kts"),
                ),
                (
                    "android/root-build.gradle.kts",
                    include_bytes!("../../../../magdroid/android/build.gradle.kts"),
                ),
                (
                    "android/settings.gradle.kts",
                    include_bytes!("../../../../magdroid/android/settings.gradle.kts"),
                ),
                (
                    "android/gradle.properties",
                    include_bytes!("../../../../magdroid/android/gradle.properties"),
                ),
                (
                    "android/gradle-wrapper.properties",
                    include_bytes!(
                        "../../../../magdroid/android/gradle/wrapper/gradle-wrapper.properties"
                    ),
                ),
                (
                    "android/app/AndroidManifest.xml",
                    include_bytes!("../../../../magdroid/android/app/src/main/AndroidManifest.xml"),
                ),
                (
                    "android/bridge-build.gradle.kts",
                    include_bytes!("../../../../magdroid/android/bridge/build.gradle.kts"),
                ),
                (
                    "android/bridge/AndroidManifest.xml",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/AndroidManifest.xml"
                    ),
                ),
                (
                    "android/accessibility_service_config.xml",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/res/xml/\
                         accessibility_service_config.xml"
                    ),
                ),
                (
                    "android/MainActivity.kt",
                    include_bytes!(
                        "../../../../magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/\
                         MainActivity.kt"
                    ),
                ),
                (
                    "android/BridgeScreen.kt",
                    include_bytes!(
                        "../../../../magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/\
                         ui/BridgeScreen.kt"
                    ),
                ),
                (
                    "android/AndroidAutomationIdentity.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/access/AndroidAutomationIdentity.kt"
                    ),
                ),
                (
                    "android/AndroidPlayIntegrity.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/access/AndroidPlayIntegrity.kt"
                    ),
                ),
                (
                    "android/DeviceEnrollment.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/access/DeviceEnrollment.kt"
                    ),
                ),
                (
                    "android/MagicianAccess.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/access/MagicianAccess.kt"
                    ),
                ),
                (
                    "android/MagicianBridgeClient.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/bridge/MagicianBridgeClient.kt"
                    ),
                ),
                (
                    "android/MagdroidMcpServer.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/mcp/MagdroidMcpServer.kt"
                    ),
                ),
                (
                    "android/McpToolHandler.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/mcp/McpToolHandler.kt"
                    ),
                ),
                (
                    "android/McpToolRegistry.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/mcp/McpToolRegistry.kt"
                    ),
                ),
                (
                    "android/McpProtocol.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/mcp/McpProtocol.kt"
                    ),
                ),
                (
                    "android/ProtectionGate.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/protection/ProtectionGate.kt"
                    ),
                ),
                (
                    "android/ProtectedApps.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/protection/ProtectedApps.kt"
                    ),
                ),
                (
                    "android/MagdroidAccessibilityService.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/service/MagdroidAccessibilityService.kt"
                    ),
                ),
                (
                    "android/UiTreeWalker.kt",
                    include_bytes!(
                        "../../../../magdroid/android/bridge/src/main/kotlin/ai/magicbeans/\
                         magdroid/uitree/UiTreeWalker.kt"
                    ),
                ),
            ] {
                hash_implementation_component(&mut hasher, path, bytes);
            }
            AppDigest::blake3(hasher.finalize().as_bytes())
        })
        .clone()
}

fn hash_implementation_component(hasher: &mut blake3::Hasher, path: &str, bytes: &[u8]) {
    hasher.update(b"magician.app-android-device.implementation-component.v1\0");
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path.as_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[allow(dead_code)] // Stable authoring/qualification identity helper.
pub(crate) fn app_android_implementation_plan_digest(
    source_digest: &AppDigest,
) -> Result<AppDigest, AppAndroidDeviceError> {
    app_android_action_implementation_plan_digest(source_digest, "snapshot")
}

pub fn app_android_action_implementation_plan_digest(
    source_digest: &AppDigest,
    action_name: &str,
) -> Result<AppDigest, AppAndroidDeviceError> {
    let (wire_action, max_result_bytes, max_evidence_bytes, max_evidence_nodes) =
        android_action_wire_contract(action_name).ok_or(AppAndroidDeviceError::InvalidInput)?;
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_ANDROID_DEVICE_PROFILE_V1,
        "pack_source_digest": source_digest,
        "runtime_implementation_digest": app_android_runtime_implementation_digest(),
        "physical_owner": "paired_magdroid_exact_connection",
        "action": action_name,
        "wire_action": wire_action,
        "max_result_bytes": max_result_bytes,
        "max_evidence_bytes": max_evidence_bytes,
        "max_evidence_nodes": max_evidence_nodes,
        "private_claim_schema": APP_ANDROID_OWNER_WIRE_SCHEMA,
        "raw_device_ids": false,
        "raw_argv": false,
        "shell": false,
    }))
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))
}

fn android_action_wire_contract(action_name: &str) -> Option<(&'static str, u64, u64, u64)> {
    match action_name {
        "snapshot" => Some((
            APP_ANDROID_WIRE_ACTION_SNAPSHOT,
            APP_ANDROID_SNAPSHOT_RESULT_CEILING,
            APP_ANDROID_SNAPSHOT_EVIDENCE_CEILING,
            APP_ANDROID_SNAPSHOT_NODE_CEILING,
        )),
        "screenshot" => Some((
            APP_ANDROID_WIRE_ACTION_SCREENSHOT,
            APP_ANDROID_SCREENSHOT_RESULT_CEILING,
            APP_ANDROID_SCREENSHOT_EVIDENCE_CEILING,
            1,
        )),
        "launch" => Some((
            APP_ANDROID_WIRE_ACTION_LAUNCH,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        "close" => Some((
            APP_ANDROID_WIRE_ACTION_CLOSE,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        "tap" => Some((
            APP_ANDROID_WIRE_ACTION_TAP,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        "type" => Some((
            APP_ANDROID_WIRE_ACTION_TYPE,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        "key" => Some((
            APP_ANDROID_WIRE_ACTION_KEY,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        "scroll" => Some((
            APP_ANDROID_WIRE_ACTION_SCROLL,
            APP_ANDROID_ACTION_RESULT_CEILING,
            16 * 1024,
            1,
        )),
        _ => None,
    }
}

#[allow(dead_code)] // Legacy snapshot package compatibility.
pub(crate) fn reviewed_android_snapshot_action(
    action_ref: AppReference,
    implementation_digest: AppDigest,
) -> Result<AppInteractiveReviewedAction, AppAndroidDeviceError> {
    reviewed_android_action(action_ref, implementation_digest, "snapshot")
}

pub(crate) fn reviewed_android_action(
    action_ref: AppReference,
    implementation_digest: AppDigest,
    action_name: &str,
) -> Result<AppInteractiveReviewedAction, AppAndroidDeviceError> {
    let (class, observation, invalidates, ceiling) = match action_name {
        "snapshot" => (
            AppInteractiveActionClass::Observe,
            None,
            false,
            APP_ANDROID_SNAPSHOT_RESULT_CEILING,
        ),
        "screenshot" => (
            AppInteractiveActionClass::CapturePixels,
            None,
            false,
            APP_ANDROID_SCREENSHOT_RESULT_CEILING,
        ),
        "launch" | "close" => (
            AppInteractiveActionClass::NavigateOrLaunch,
            None,
            true,
            APP_ANDROID_ACTION_RESULT_CEILING,
        ),
        "tap" | "key" => (
            AppInteractiveActionClass::OutwardCommit,
            Some(AppInteractiveObservationKind::StructuredTree),
            true,
            APP_ANDROID_ACTION_RESULT_CEILING,
        ),
        "type" | "scroll" => (
            AppInteractiveActionClass::Interact,
            Some(AppInteractiveObservationKind::StructuredTree),
            true,
            APP_ANDROID_ACTION_RESULT_CEILING,
        ),
        _ => return Err(AppAndroidDeviceError::InvalidInput),
    };
    let input =
        android_action_input_schema(action_name).ok_or(AppAndroidDeviceError::InvalidInput)?;
    let result =
        android_action_result_schema(action_name).ok_or(AppAndroidDeviceError::InvalidInput)?;
    AppInteractiveReviewedAction::reviewed(
        action_ref,
        class,
        AppDigest::blake3_canonical_json(&input)
            .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
        AppDigest::blake3_canonical_json(&result)
            .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
        implementation_digest,
        ceiling,
        observation,
        invalidates,
    )
    .map_err(Into::into)
}

async fn current_snapshot_owner_fence(
    device_key: &DeviceKey,
    policy: &AppAndroidTargetPolicy,
    connection: &DeviceOwnerConnection,
) -> Result<AppAndroidOwnerSnapshotFence, AppAndroidDeviceError> {
    let store =
        global_android_owner_store().ok_or(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch)?;
    revalidate_owner_status(&store)
        .await
        .map_err(|_| AppAndroidDeviceError::PhysicalOwnerIdentityMismatch)?;
    store
        .require_snapshot_receipt(
            &device_key.principal,
            &device_key.workspace,
            policy.device_target_ref().as_str(),
            policy.review_generation(),
            policy.allowed_packages(),
            connection.automation_identity_digest(),
        )
        .await
        .map_err(|_| AppAndroidDeviceError::PhysicalOwnerIdentityMismatch)
}

/// Run-owned exact paired handset plus shared interactive session. Raw scope,
/// device id and socket generation never leave this move-only owner.
pub(crate) struct AppAndroidDeviceOwner {
    hub: Arc<DeviceBridgeHub>,
    connection: Arc<DeviceOwnerConnection>,
    audit: Arc<DeviceActionAudit>,
    device_key: DeviceKey,
    interactive: AppInteractiveSessionHandle,
    grant: AppInteractiveGrantDescriptor,
    policy: AppAndroidTargetPolicy,
    capability_ref: AppReference,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    owner_fence: AppAndroidOwnerSnapshotFence,
}

#[allow(dead_code)] // Stop/snapshot convenience methods remain part of the sealed owner API.
impl AppAndroidDeviceOwner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn acquire_reviewed(
        pairing: &DevicePairingStore,
        hub: Arc<DeviceBridgeHub>,
        audit: Arc<DeviceActionAudit>,
        principal: &str,
        workspace: &str,
        capability_ref: AppReference,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        policy: AppAndroidTargetPolicy,
        resource_lease_ref: AppReference,
        session_ordinal: u16,
        cancellation: AppInteractiveCancellation,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAndroidDeviceError> {
        let profile_digest = app_android_owner_profile_digest()?;
        let runtime_digest = app_android_runtime_implementation_digest();
        let reviewed_runtime = grant
            .actions()
            .values()
            .next()
            .map(|action| action.owner_implementation_digest())
            .ok_or(AppAndroidDeviceError::ReviewMismatch)?;
        if grant.profile() != AppInteractiveExecutionProfile::AndroidDevice
            || grant.background() != AppInteractiveBackgroundPosture::DirectOwner
            || grant.target_policy_digest() != policy.digest()
            || grant.owner_profile_digest() != &profile_digest
            || grant.owner_implementation_digest() != reviewed_runtime
            || grant.installation_generation() == 0
        {
            return Err(AppAndroidDeviceError::ReviewMismatch);
        }
        // The locked implementation-plan digest also includes the pack source;
        // retain an independent runtime digest in the physical target so a
        // source-only identity cannot stand in for the executing owner.
        if reviewed_runtime == &runtime_digest {
            return Err(AppAndroidDeviceError::ReviewMismatch);
        }
        let device_key = pairing
            .resolve_automation_review(
                principal,
                workspace,
                policy.device_target_ref().as_str(),
                policy.review_generation(),
                digest_hex(policy.review_digest())?,
            )
            .await?;
        let connection = hub
            .bind_owner_connection(&device_key, DEFAULT_DEVICE_ACTION_TIMEOUT)
            .await?;
        if connection.protocol_version() != APP_ANDROID_OWNER_PROTOCOL
            || connection.server_name() != APP_ANDROID_OWNER_SERVER_NAME
            || connection.server_version() != APP_ANDROID_OWNER_SERVER_VERSION
            || connection.automation_target_ref() != policy.device_target_ref().as_str()
            || connection.automation_review_generation() != policy.review_generation()
        {
            return Err(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch);
        }
        let owner_fence = current_snapshot_owner_fence(&device_key, &policy, &connection).await?;
        let scope_digest = AppDigest::blake3_canonical_json(&json!({
            "principal": principal,
            "workspace": workspace,
        }))
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
        let owner_target_digest = AppDigest::blake3_canonical_json(&json!({
            "schema": APP_ANDROID_DEVICE_PROFILE_V1,
            "runtime_implementation_digest": runtime_digest,
            "owner_profile_digest": profile_digest,
            "target_policy_digest": policy.digest(),
            "device_target_ref": policy.device_target_ref(),
            "automation_identity_digest": connection.automation_identity_digest(),
            "review_generation": policy.review_generation(),
            "review_digest": policy.review_digest(),
            "desktop_owner_receipt": &owner_fence,
            "protocol_version": connection.protocol_version(),
            "server_name": connection.server_name(),
            "server_version": connection.server_version(),
            "scope_digest": scope_digest,
            "resource_lease_ref": &resource_lease_ref,
            "session_ordinal": session_ordinal,
        }))
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
        let owner_target_ref = AppReference::parse(format!(
            "android-session:{}",
            digest_hex(&owner_target_digest)?
        ))
        .map_err(|_| AppAndroidDeviceError::Encoding("invalid Android owner target".to_owned()))?;
        let interactive = AppInteractiveSessionHandle::acquire(
            &grant,
            current,
            owner_target_ref.clone(),
            owner_target_digest.clone(),
            resource_lease_ref,
            session_ordinal,
            cancellation,
            now,
            expires_at,
        )?;
        Ok(Self {
            hub,
            connection: Arc::new(connection),
            audit,
            device_key,
            interactive,
            grant,
            policy,
            capability_ref,
            owner_target_ref,
            owner_target_digest,
            owner_fence,
        })
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.interactive.cancellation()
    }

    pub(crate) fn stop(&self, reason: AppInteractiveCancellationReason) {
        self.interactive.cancellation().cancel(reason);
    }

    pub(crate) fn rebind_authority(
        &mut self,
        capability_ref: AppReference,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        source_digest: &AppDigest,
        action_name: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AppAndroidDeviceError> {
        let expected = app_android_action_implementation_plan_digest(source_digest, action_name)?;
        if grant.profile() != AppInteractiveExecutionProfile::AndroidDevice
            || grant.background() != AppInteractiveBackgroundPosture::DirectOwner
            || grant.target_policy_digest() != self.policy.digest()
            || grant.owner_profile_digest() != &app_android_owner_profile_digest()?
            || grant.owner_implementation_digest() != &expected
        {
            return Err(AppAndroidDeviceError::ReviewMismatch);
        }
        self.interactive.rebind_exact_leaf(&grant, current, now)?;
        self.capability_ref = capability_ref;
        self.grant = grant;
        Ok(())
    }

    pub(crate) async fn prepare_single_snapshot(
        mut self,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppAndroidPreparedAction, AppAndroidDeviceError> {
        self.prepare_action(
            current,
            primitive,
            locked_action,
            source_digest,
            AppAndroidAction::Snapshot,
            None,
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_action(
        &mut self,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        action: AppAndroidAction,
        observation: Option<AppAndroidObservation>,
        now: DateTime<Utc>,
    ) -> Result<AppAndroidPreparedAction, AppAndroidDeviceError> {
        validate_android_input(&action, &self.policy)?;
        let reviewed = self
            .grant
            .actions()
            .get(locked_action.action_ref())
            .ok_or(AppAndroidDeviceError::ActionNotReviewed)?;
        let expected = reviewed_android_action(
            locked_action.action_ref().clone(),
            reviewed.owner_implementation_digest().clone(),
            action.action_name(),
        )?;
        if reviewed != &expected {
            return Err(AppAndroidDeviceError::ReviewMismatch);
        }
        validate_locked_android_action(primitive, locked_action, reviewed, &source_digest)?;
        let canonical_input = canonical_json_bytes(
            &serde_json::to_value(&action)
                .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
        )
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
        let (interactive_observation, observation_claim) = match observation {
            Some(observation) => {
                if !matches!(
                    action,
                    AppAndroidAction::Tap { .. }
                        | AppAndroidAction::Type { .. }
                        | AppAndroidAction::Key { .. }
                        | AppAndroidAction::Scroll { .. }
                ) || observation.owner_target_digest != self.owner_target_digest
                    || action.observation_ref() != Some(&observation.public_ref)
                {
                    return Err(AppAndroidDeviceError::ObservationMismatch);
                }
                let claim = AndroidWireObservationClaim {
                    foreground_package: observation.foreground_package.clone(),
                    snapshot_sha256: observation.snapshot_sha256.clone(),
                    width: observation.interactive.geometry().width,
                    height: observation.interactive.geometry().height,
                    element_bounds: observation.element_bounds,
                };
                (Some(observation.interactive), Some(claim))
            },
            None if action.requires_observation() => {
                return Err(AppAndroidDeviceError::ObservationMismatch)
            },
            None => (None, None),
        };
        let interactive_permit = self.interactive.authorize_action(
            &self.grant,
            current,
            locked_action.action_ref(),
            &canonical_input,
            action.resource_claim(),
            interactive_observation,
            now,
        )?;
        // The handset echoes this content-free correlation. Deriving it from
        // the common permit, instead of minting an unrelated adapter nonce,
        // binds the private wire claim to the exact reviewed action/session,
        // input, resource claim and expiry consumed below.
        let permit_nonce = digest_hex(interactive_permit.permit_digest())?.to_owned();
        let (wire_action, wire_arguments, target_package) = android_wire_arguments(
            &action,
            &permit_nonce,
            self.policy.allowed_packages(),
            observation_claim,
        )?;
        Ok(AppAndroidPreparedAction {
            hub: Arc::clone(&self.hub),
            connection: self.connection.clone(),
            audit: Arc::clone(&self.audit),
            device_key: self.device_key.clone(),
            policy: self.policy.clone(),
            capability_ref: self.capability_ref.clone(),
            source_digest,
            primitive_ref: primitive.primitive_ref().clone(),
            reviewed_action: reviewed.clone(),
            action,
            canonical_input,
            permit_nonce,
            wire_action,
            wire_arguments,
            target_package,
            owner_target_ref: self.owner_target_ref.clone(),
            owner_target_digest: self.owner_target_digest.clone(),
            owner_fence: self.owner_fence.clone(),
            interactive_permit,
            live_session: None,
            current: None,
        })
    }

    pub(crate) fn materialize_observation(
        &mut self,
        current: &AppInteractiveCurrentFence,
        evidence: AppAndroidObservationEvidence,
        now: DateTime<Utc>,
    ) -> Result<AppAndroidObservation, AppAndroidDeviceError> {
        if evidence.owner_target_digest != self.owner_target_digest
            || !self
                .policy
                .allowed_packages()
                .contains(&evidence.foreground_package)
        {
            return Err(AppAndroidDeviceError::ObservationMismatch);
        }
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(
                APP_ANDROID_OBSERVATION_TTL_SECONDS,
            ))
            .ok_or(AppAndroidDeviceError::ObservationMismatch)?
            .min(self.grant.expires_at());
        let interactive = self.interactive.issue_observation(
            &self.grant,
            current,
            AppInteractiveObservationKind::StructuredTree,
            evidence.geometry,
            evidence.content_digest,
            evidence.labels_digest,
            evidence.evidence_bytes,
            evidence.evidence_nodes,
            now,
            expires_at,
        )?;
        Ok(AppAndroidObservation {
            public_ref: evidence.public_ref,
            interactive,
            owner_target_digest: self.owner_target_digest.clone(),
            foreground_package: evidence.foreground_package,
            snapshot_sha256: evidence.snapshot_sha256,
            element_bounds: evidence.element_bounds,
        })
    }
}

pub(crate) struct AppAndroidLiveSession {
    owner: AppAndroidDeviceOwner,
    observation: Option<AppAndroidObservation>,
}

impl AppAndroidLiveSession {
    pub(crate) fn new(owner: AppAndroidDeviceOwner) -> Self {
        Self {
            owner,
            observation: None,
        }
    }

    pub(crate) fn binding_digest(&self) -> &AppDigest {
        self.owner.interactive.binding_digest()
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.owner.interactive.cancellation()
    }

    pub(crate) fn rebind_authority(
        &mut self,
        capability_ref: AppReference,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        source_digest: &AppDigest,
        action_name: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AppAndroidDeviceError> {
        self.owner.rebind_authority(
            capability_ref,
            grant,
            current,
            source_digest,
            action_name,
            now,
        )?;
        if let Some(previous) = self.observation.take() {
            let public_ref = previous.public_ref;
            let old = previous.interactive;
            let expires_at = old.expires_at().min(self.owner.grant.expires_at());
            if expires_at > now {
                let interactive = self.owner.interactive.issue_observation(
                    &self.owner.grant,
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
                self.observation = Some(AppAndroidObservation {
                    public_ref,
                    interactive,
                    owner_target_digest: previous.owner_target_digest,
                    foreground_package: previous.foreground_package,
                    snapshot_sha256: previous.snapshot_sha256,
                    element_bounds: previous.element_bounds,
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_action(
        live: Arc<tokio::sync::Mutex<Self>>,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        action: AppAndroidAction,
        now: DateTime<Utc>,
    ) -> Result<AppAndroidPreparedAction, AppAndroidDeviceError> {
        let mut guard = live.lock().await;
        let observation = if action.requires_observation() {
            guard.observation.take()
        } else {
            None
        };
        let mut prepared = guard
            .owner
            .prepare_action(
                current,
                primitive,
                locked_action,
                source_digest,
                action,
                observation,
                now,
            )
            .await?;
        prepared.live_session = Some(Arc::clone(&live));
        prepared.current = Some(current.clone());
        Ok(prepared)
    }
}

struct AndroidWireObservationClaim {
    foreground_package: String,
    snapshot_sha256: String,
    width: u32,
    height: u32,
    element_bounds: BTreeMap<AppReference, AppAndroidBounds>,
}

fn android_wire_arguments(
    action: &AppAndroidAction,
    permit_nonce: &str,
    allowed_packages: &BTreeSet<String>,
    observation: Option<AndroidWireObservationClaim>,
) -> Result<(&'static str, Value, Option<String>), AppAndroidDeviceError> {
    let (wire_action, max_result_bytes, _, max_evidence_nodes) =
        android_action_wire_contract(action.action_name())
            .ok_or(AppAndroidDeviceError::InvalidInput)?;
    let max_result_bytes =
        u32::try_from(max_result_bytes).map_err(|_| AppAndroidDeviceError::InvalidTargetPolicy)?;
    let max_evidence_nodes = u32::try_from(max_evidence_nodes)
        .map_err(|_| AppAndroidDeviceError::InvalidTargetPolicy)?;
    let target_package = match action {
        AppAndroidAction::Snapshot => None,
        AppAndroidAction::Screenshot { package }
        | AppAndroidAction::Launch { package }
        | AppAndroidAction::Close { package } => Some(package.clone()),
        AppAndroidAction::Tap { .. }
        | AppAndroidAction::Type { .. }
        | AppAndroidAction::Key { .. }
        | AppAndroidAction::Scroll { .. } => observation
            .as_ref()
            .map(|value| value.foreground_package.clone()),
    };
    let observation_json = observation.as_ref().map(|value| {
        json!({
            "foreground_package": value.foreground_package,
            "snapshot_sha256": value.snapshot_sha256,
            "width": value.width,
            "height": value.height,
        })
    });
    let claim = json!({
        "schema": APP_ANDROID_OWNER_WIRE_SCHEMA,
        "permit_nonce": permit_nonce,
        "action": action.action_name(),
        "allowed_packages": allowed_packages,
        "target_package": target_package,
        "observation": observation_json,
        "max_result_bytes": max_result_bytes,
        "max_evidence_nodes": max_evidence_nodes,
    });
    let mut arguments = match action {
        AppAndroidAction::Snapshot => json!({"filter":"interactive","max_depth":50}),
        AppAndroidAction::Screenshot { .. } => json!({"quality":"full"}),
        AppAndroidAction::Launch { package } => {
            json!({"package_name":package,"clear_task":false})
        },
        AppAndroidAction::Close { package } => json!({"package_name":package,"force":false}),
        AppAndroidAction::Tap { element_ref, .. } => {
            let bounds = observation
                .as_ref()
                .and_then(|value| value.element_bounds.get(element_ref))
                .ok_or(AppAndroidDeviceError::ObservationMismatch)?;
            json!({
                "x": bounds.left + (bounds.right - bounds.left) / 2,
                "y": bounds.top + (bounds.bottom - bounds.top) / 2,
            })
        },
        AppAndroidAction::Type { text, .. } => json!({"text":text,"append":false}),
        AppAndroidAction::Key { key, .. } => json!({"key":key.wire_label()}),
        AppAndroidAction::Scroll {
            direction,
            duration_millis,
            ..
        } => {
            let observed = observation
                .as_ref()
                .ok_or(AppAndroidDeviceError::ObservationMismatch)?;
            let x1 = observed.width / 2;
            let y1 = observed.height / 2;
            let dx = (observed.width / 3).max(1);
            let dy = (observed.height / 3).max(1);
            let (x2, y2) = match direction {
                AppAndroidScrollDirection::Up => {
                    (x1, y1.saturating_add(dy).min(observed.height - 1))
                },
                AppAndroidScrollDirection::Down => (x1, y1.saturating_sub(dy)),
                AppAndroidScrollDirection::Left => {
                    (x1.saturating_add(dx).min(observed.width - 1), y1)
                },
                AppAndroidScrollDirection::Right => (x1.saturating_sub(dx), y1),
            };
            json!({
                "start_x":x1,"start_y":y1,"end_x":x2,"end_y":y2,
                "duration_ms":duration_millis,
            })
        },
    };
    arguments
        .as_object_mut()
        .ok_or(AppAndroidDeviceError::InvalidInput)?
        .insert("_magician_apps".to_owned(), claim);
    Ok((wire_action, arguments, target_package))
}

fn validate_locked_android_action(
    primitive: &AppLockedPrimitiveBinding,
    locked: &AppLockedPrimitiveActionBinding,
    reviewed: &AppInteractiveReviewedAction,
    source_digest: &AppDigest,
) -> Result<(), AppAndroidDeviceError> {
    if primitive.source_content_digest() != source_digest
        || !primitive
            .actions()
            .iter()
            .any(|candidate| candidate == locked)
        || !locked.dispatchable()
        || locked.action_ref() != reviewed.action_ref()
        || locked.input_schema_digest() != Some(reviewed.input_schema_digest())
        || locked.result_schema_digest() != Some(reviewed.result_schema_digest())
        || locked.implementation_plan_digest() != Some(reviewed.owner_implementation_digest())
        || locked.transport_result_byte_ceiling() != Some(reviewed.result_byte_ceiling())
    {
        return Err(AppAndroidDeviceError::ReviewMismatch);
    }
    Ok(())
}

pub(crate) struct AppAndroidPreparedAction {
    hub: Arc<DeviceBridgeHub>,
    connection: Arc<DeviceOwnerConnection>,
    audit: Arc<DeviceActionAudit>,
    device_key: DeviceKey,
    policy: AppAndroidTargetPolicy,
    capability_ref: AppReference,
    source_digest: AppDigest,
    primitive_ref: AppReference,
    reviewed_action: AppInteractiveReviewedAction,
    action: AppAndroidAction,
    canonical_input: Vec<u8>,
    permit_nonce: String,
    wire_action: &'static str,
    wire_arguments: Value,
    target_package: Option<String>,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    owner_fence: AppAndroidOwnerSnapshotFence,
    interactive_permit: AppInteractiveActionPermit,
    live_session: Option<Arc<tokio::sync::Mutex<AppAndroidLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

#[allow(dead_code)] // Canonical bytes are exposed for owner-side qualification.
impl AppAndroidPreparedAction {
    pub(crate) fn canonical_input(&self) -> &[u8] {
        &self.canonical_input
    }

    pub(crate) async fn reserve_io_slot(&self) -> Result<AppAndroidIoSlot, AppAndroidDeviceError> {
        let cancellation = self.interactive_permit.cancellation();
        let expires_at = self.interactive_permit.expires_at();
        let permit = await_android_owner(&cancellation, expires_at, None, async {
            self.connection
                .reserve_owner_io()
                .await
                .map_err(AppAndroidDeviceError::Bridge)
        })
        .await?;
        if !self.hub.owner_connection_is_current(&self.connection)
            || current_snapshot_owner_fence(&self.device_key, &self.policy, &self.connection)
                .await?
                != self.owner_fence
        {
            return Err(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch);
        }
        Ok(AppAndroidIoSlot {
            _permit: permit,
            hub: Arc::clone(&self.hub),
            device_key: self.device_key.clone(),
            connection_id: self.connection.connection_id(),
        })
    }

    pub(crate) fn bind_effect(
        self,
        binding: &AppEffectBinding,
        now: DateTime<Utc>,
    ) -> Result<AppAndroidEffectAction, AppAndroidDeviceError> {
        let interactive_effect =
            self.interactive_permit
                .bind_effect(binding, &self.canonical_input, now)?;
        Ok(AppAndroidEffectAction {
            hub: self.hub,
            connection: self.connection,
            audit: self.audit,
            device_key: self.device_key,
            policy: self.policy,
            reviewed_action: self.reviewed_action,
            action: self.action,
            canonical_input: self.canonical_input,
            permit_nonce: self.permit_nonce,
            wire_action: self.wire_action,
            wire_arguments: self.wire_arguments,
            target_package: self.target_package,
            owner_target_digest: self.owner_target_digest,
            owner_fence: self.owner_fence,
            interactive_effect: Some(interactive_effect),
            live_session: self.live_session,
            current: self.current,
        })
    }
}

impl AppAndroidEffectAction {
    pub(crate) fn interactive_inspection(&self) -> AppInteractiveEffectInspection {
        self.interactive_effect
            .as_ref()
            .expect("bound Android effect has an unconsumed interactive permit")
            .inspection()
    }

    /// Post-provider policy fence. The durable roster is carried across the
    /// common dispatch-start boundary so a revoke or review rotation that
    /// lands while the handset is producing the snapshot cannot release bytes
    /// under stale authority.
    async fn revalidate_pairing(
        &self,
        pairing: &DevicePairingStore,
    ) -> Result<(), AppAndroidDeviceError> {
        let current = pairing
            .resolve_automation_review(
                &self.device_key.principal,
                &self.device_key.workspace,
                self.policy.device_target_ref().as_str(),
                self.policy.review_generation(),
                digest_hex(self.policy.review_digest())?,
            )
            .await?;
        let owner_fence =
            current_snapshot_owner_fence(&self.device_key, &self.policy, &self.connection).await?;
        if current != self.device_key
            || self.connection.key() != &self.device_key
            || self.connection.automation_target_ref() != self.policy.device_target_ref().as_str()
            || self.connection.automation_review_generation() != self.policy.review_generation()
            || !self.hub.owner_connection_is_current(&self.connection)
            || owner_fence != self.owner_fence
        {
            return Err(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch);
        }
        Ok(())
    }
}

impl AppEffectPhysicalOwner for AppAndroidPreparedAction {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        if tool_ref != &self.capability_ref
            || primitive.primitive_ref() != &self.primitive_ref
            || primitive.source_content_digest() != &self.source_digest
            || !primitive
                .actions()
                .iter()
                .any(|candidate| candidate == action)
            || action.action_ref() != self.reviewed_action.action_ref()
            || action.input_schema_digest() != Some(self.reviewed_action.input_schema_digest())
            || action.result_schema_digest() != Some(self.reviewed_action.result_schema_digest())
            || action.implementation_plan_digest()
                != Some(self.reviewed_action.owner_implementation_digest())
            || action.transport_result_byte_ceiling()
                != Some(self.reviewed_action.result_byte_ceiling())
            || !action.dispatchable()
        {
            return Err(AppEffectKernelError::IdentityMismatch);
        }
        let target = AttestedAppToolTarget::from_trusted_local_dispatcher(
            tool_ref.clone(),
            self.owner_target_ref.clone(),
        );
        Ok(AppEffectPhysicalTarget::from_owner(
            self.owner_target_ref.clone(),
            self.source_digest.clone(),
            target,
        ))
    }
}

pub(crate) struct AppAndroidIoSlot {
    _permit: OwnedSemaphorePermit,
    hub: Arc<DeviceBridgeHub>,
    device_key: DeviceKey,
    connection_id: Uuid,
}

pub(crate) struct AppAndroidEffectAction {
    hub: Arc<DeviceBridgeHub>,
    connection: Arc<DeviceOwnerConnection>,
    audit: Arc<DeviceActionAudit>,
    device_key: DeviceKey,
    policy: AppAndroidTargetPolicy,
    reviewed_action: AppInteractiveReviewedAction,
    action: AppAndroidAction,
    canonical_input: Vec<u8>,
    permit_nonce: String,
    wire_action: &'static str,
    wire_arguments: Value,
    target_package: Option<String>,
    owner_target_digest: AppDigest,
    owner_fence: AppAndroidOwnerSnapshotFence,
    interactive_effect: Option<AppInteractiveEffectPermit>,
    live_session: Option<Arc<tokio::sync::Mutex<AppAndroidLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAndroidSnapshotResult {
    kind: &'static str,
    success: bool,
    device_target_ref: AppReference,
    foreground_package: String,
    geometry: AppInteractiveGeometry,
    content_digest: AppDigest,
    observation_ref: AppReference,
    nodes: Vec<AppAndroidSnapshotNode>,
    total_nodes: u64,
    truncated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppAndroidScreenshotResult {
    kind: &'static str,
    success: bool,
    foreground_package: String,
    content_digest: AppDigest,
    width: u32,
    height: u32,
    mime_type: &'static str,
    image_base64: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppAndroidActionResult {
    kind: &'static str,
    success: bool,
    action: &'static str,
    target_package: String,
    foreground_package: Option<String>,
    receipt_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppAndroidSnapshotNode {
    element_ref: AppReference,
    text: Option<String>,
    description: Option<String>,
    clickable: bool,
    focusable: bool,
    scrollable: bool,
    checkable: bool,
    bounds: Option<AppAndroidBounds>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppAndroidBounds {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

pub(crate) struct AppAndroidObservation {
    public_ref: AppReference,
    interactive: AppInteractiveObservationRef,
    owner_target_digest: AppDigest,
    foreground_package: String,
    snapshot_sha256: String,
    element_bounds: BTreeMap<AppReference, AppAndroidBounds>,
}

#[derive(Clone)]
pub(crate) struct AppAndroidObservationEvidence {
    public_ref: AppReference,
    owner_target_digest: AppDigest,
    foreground_package: String,
    snapshot_sha256: String,
    geometry: AppInteractiveGeometry,
    content_digest: AppDigest,
    labels_digest: AppDigest,
    evidence_bytes: u64,
    evidence_nodes: u64,
    element_bounds: BTreeMap<AppReference, AppAndroidBounds>,
}

pub(crate) struct AppAndroidObservedEffect<R> {
    result: ActionResult,
    canonical_result: Vec<u8>,
    observation: Option<AppAndroidObservationEvidence>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    effect: AppEffectInFlight<R>,
}

impl<R> AppAndroidObservedEffect<R> {
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
        error: AppAndroidDeviceError,
    ) -> AppAndroidUncertainEffect<R> {
        AppAndroidUncertainEffect {
            error,
            interactive_receipt: Some(self.interactive_receipt),
            settlement: self.effect.outcome_uncertain(stage),
        }
    }

    pub(crate) fn commit(
        self,
    ) -> Result<AppAndroidCommittedEffect<R>, AppAndroidUncertainEffect<R>> {
        let settlement = match self.effect.commit_result(&self.canonical_result) {
            Ok(settlement) => settlement,
            Err(settlement) => {
                return Err(AppAndroidUncertainEffect {
                    error: AppAndroidDeviceError::ResultTooLarge,
                    interactive_receipt: Some(self.interactive_receipt),
                    settlement,
                })
            },
        };
        Ok(AppAndroidCommittedEffect {
            result: self.result,
            observation: self.observation,
            interactive_receipt: self.interactive_receipt,
            settlement,
        })
    }
}

pub(crate) struct AppAndroidCommittedEffect<R> {
    result: ActionResult,
    observation: Option<AppAndroidObservationEvidence>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppAndroidCommittedEffect<R> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ActionResult,
        Option<AppAndroidObservationEvidence>,
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

pub(crate) struct AppAndroidUncertainEffect<R> {
    error: AppAndroidDeviceError,
    interactive_receipt: Option<AppInteractiveSettlementReceipt>,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppAndroidUncertainEffect<R> {
    pub(crate) fn interactive_receipt(&self) -> Option<&AppInteractiveSettlementReceipt> {
        self.interactive_receipt.as_ref()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        AppAndroidDeviceError,
        Option<AppInteractiveSettlementReceipt>,
        AppEffectSettlement<R>,
    ) {
        (self.error, self.interactive_receipt, self.settlement)
    }
}

pub(crate) enum AppAndroidEffectOutcome<R> {
    Observed(AppAndroidObservedEffect<R>),
    Uncertain(AppAndroidUncertainEffect<R>),
}

impl AppAndroidIoSlot {
    pub(crate) fn is_live_for_start(&self) -> bool {
        self.hub
            .owner_connection_identity_is_current(&self.device_key, self.connection_id)
    }
}

impl AppAndroidPreparedAction {
    /// Final pre-start pairing fence. Reopening the durable roster avoids a
    /// stale in-memory pairing owner. The exact bound socket UUID is checked
    /// here before durable effect start and again by `dispatch_bound` after
    /// provider authorization is consumed.
    pub(crate) async fn revalidate_pairing(
        &self,
        pairing: &DevicePairingStore,
    ) -> Result<(), AppAndroidDeviceError> {
        let current = pairing
            .resolve_automation_review(
                &self.device_key.principal,
                &self.device_key.workspace,
                self.policy.device_target_ref().as_str(),
                self.policy.review_generation(),
                digest_hex(self.policy.review_digest())?,
            )
            .await?;
        let owner_fence =
            current_snapshot_owner_fence(&self.device_key, &self.policy, &self.connection).await?;
        if current != self.device_key
            || self.connection.key() != &self.device_key
            || self.connection.automation_target_ref() != self.policy.device_target_ref().as_str()
            || self.connection.automation_review_generation() != self.policy.review_generation()
            || !self.hub.owner_connection_is_current(&self.connection)
            || owner_fence != self.owner_fence
        {
            return Err(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch);
        }
        Ok(())
    }
}

/// Consume the common provider token and execute the exact paired handset
/// generation. Every failure after this point is uncertain, including a
/// read-only snapshot: a lost response cannot be represented as proven-unspent
/// common authority.
pub(crate) async fn execute_started_android<R>(
    _io_slot: AppAndroidIoSlot,
    mut action: AppAndroidEffectAction,
    pairing: DevicePairingStore,
    mut effect: AppEffectInFlight<R>,
    physical_timeout: Duration,
) -> AppAndroidEffectOutcome<R> {
    let resource_deadline = tokio::time::Instant::now() + physical_timeout;
    let Some(authorization) = effect.take_provider_io_authorization() else {
        let receipt = action
            .interactive_effect
            .take()
            .and_then(|permit| permit.outcome_uncertain().ok());
        return android_uncertain(
            AppAndroidDeviceError::EffectBindingMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    };
    let Some(interactive_effect) = action.interactive_effect.take() else {
        return android_uncertain(
            AppAndroidDeviceError::EffectBindingMismatch,
            None,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    };
    let owner_permit = match interactive_effect.start_owner_io(authorization, Utc::now()) {
        Ok(permit) => permit,
        Err(failure) => {
            let (_, permit) = failure.into_parts();
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit).ok();
            return android_uncertain(
                AppAndroidDeviceError::EffectBindingMismatch,
                receipt,
                effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
            );
        },
    };
    if owner_permit.profile() != AppInteractiveExecutionProfile::AndroidDevice
        || owner_permit.action() != &action.reviewed_action
        || owner_permit.target_policy_digest() != action.policy.digest()
        || owner_permit.owner_target_digest() != &action.owner_target_digest
        || owner_permit.input_digest() != &AppDigest::blake3(&action.canonical_input)
        || usize::try_from(owner_permit.input_bytes()).ok() != Some(action.canonical_input.len())
        || digest_hex(owner_permit.permit_digest()).ok() != Some(action.permit_nonce.as_str())
        || action.connection.key() != &action.device_key
    {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::EffectBindingMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    }
    if owner_permit.cancellation_reason().is_some() || Utc::now() >= owner_permit.expires_at() {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    }

    let deadline_now = Utc::now();
    let resource_expires_at = chrono::Duration::from_std(physical_timeout)
        .ok()
        .and_then(|duration| deadline_now.checked_add_signed(duration))
        .unwrap_or(deadline_now);
    let cancellation = owner_permit.cancellation();
    let expires_at = owner_permit.expires_at().min(resource_expires_at);
    if await_android_owner(
        &cancellation,
        expires_at,
        Some(resource_deadline),
        action.revalidate_pairing(&pairing),
    )
    .await
    .is_err()
        || !action.hub.owner_connection_is_current(&action.connection)
    {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::PhysicalOwnerIdentityMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    }
    let dispatched =
        await_android_owner(&cancellation, expires_at, Some(resource_deadline), async {
            let (_, max_result_bytes, _, _) =
                android_action_wire_contract(action.action.action_name())
                    .ok_or(AppAndroidDeviceError::InvalidInput)?;
            action
                .hub
                .dispatch_bound(
                    &action.connection,
                    action.wire_action,
                    action.wire_arguments.clone(),
                    DEFAULT_DEVICE_ACTION_TIMEOUT,
                    usize::try_from(max_result_bytes)
                        .map_err(|_| AppAndroidDeviceError::ResultTooLarge)?,
                )
                .await
                .map_err(AppAndroidDeviceError::Bridge)
        })
        .await;
    let raw = match dispatched {
        Ok(raw) => raw,
        Err(error) => {
            let detail = android_bridge_audit_code(&error);
            let (foreground, verdict) = match &error {
                AppAndroidDeviceError::Bridge(DeviceBridgeError::AppProtected(package)) => {
                    (Some(package.clone()), DeviceActionVerdict::AppProtected)
                },
                _ => (None, DeviceActionVerdict::Error),
            };
            let _ = await_android_owner(
                &cancellation,
                expires_at,
                Some(resource_deadline),
                append_android_audit(&action, foreground, verdict, Some(detail)),
            )
            .await;
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return android_uncertain(
                error,
                receipt,
                effect.outcome_uncertain(AppEffectStage::ProviderIo),
            );
        },
    };

    let mut completion = match project_android_result(&action, &owner_permit, raw) {
        Ok(completion) => completion,
        Err(error) => {
            let _ = await_android_owner(
                &cancellation,
                expires_at,
                Some(resource_deadline),
                append_android_audit(
                    &action,
                    None,
                    DeviceActionVerdict::Error,
                    Some("invalid_owner_response".to_owned()),
                ),
            )
            .await;
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return android_uncertain(
                error,
                receipt,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            );
        },
    };
    if await_android_owner(
        &cancellation,
        expires_at,
        Some(resource_deadline),
        append_android_audit(
            &action,
            completion.foreground_package.clone(),
            DeviceActionVerdict::Ok,
            None,
        ),
    )
    .await
    .is_err()
    {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::AuditUnavailable,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    if await_android_owner(
        &cancellation,
        expires_at,
        Some(resource_deadline),
        action.revalidate_pairing(&pairing),
    )
    .await
    .is_err()
        || !action.hub.owner_connection_is_current(&action.connection)
    {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::PhysicalOwnerIdentityMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    if owner_permit.cancellation_reason().is_some() || Utc::now() >= expires_at {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    if let Some(evidence) = completion.observation.take() {
        match (&action.live_session, &action.current) {
            (Some(live), Some(current)) => {
                let mut live = match await_android_owner(
                    &cancellation,
                    expires_at,
                    Some(resource_deadline),
                    async { Ok(live.lock().await) },
                )
                .await
                {
                    Ok(live) => live,
                    Err(error) => {
                        let receipt =
                            AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                        return android_uncertain(
                            error,
                            receipt,
                            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                        );
                    },
                };
                match live
                    .owner
                    .materialize_observation(current, evidence, Utc::now())
                {
                    Ok(observation) => live.observation = Some(observation),
                    Err(error) => {
                        let receipt =
                            AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                        return android_uncertain(
                            error,
                            receipt,
                            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                        );
                    },
                }
            },
            (None, None) => completion.observation = Some(evidence),
            _ => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                return android_uncertain(
                    AppAndroidDeviceError::ObservationMismatch,
                    receipt,
                    effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                );
            },
        }
    }
    if tokio::time::Instant::now() >= resource_deadline {
        cancellation.cancel(AppInteractiveCancellationReason::Deadline);
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return android_uncertain(
            AppAndroidDeviceError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    let interactive_receipt = match AppInteractiveSettlementReceipt::completed(
        owner_permit,
        &completion.canonical_result,
        Some(completion.evidence_digest),
        completion.evidence_bytes,
    ) {
        Ok(receipt) => receipt,
        Err(_) => {
            return android_uncertain(
                AppAndroidDeviceError::ResultTooLarge,
                None,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            );
        },
    };
    AppAndroidEffectOutcome::Observed(AppAndroidObservedEffect {
        result: completion.result,
        canonical_result: completion.canonical_result,
        observation: completion.observation,
        interactive_receipt,
        effect,
    })
}

fn android_uncertain<R>(
    error: AppAndroidDeviceError,
    interactive_receipt: Option<AppInteractiveSettlementReceipt>,
    settlement: AppEffectSettlement<R>,
) -> AppAndroidEffectOutcome<R> {
    AppAndroidEffectOutcome::Uncertain(AppAndroidUncertainEffect {
        error,
        interactive_receipt,
        settlement,
    })
}

async fn append_android_audit(
    action: &AppAndroidEffectAction,
    foreground_package: Option<String>,
    verdict: DeviceActionVerdict,
    detail: Option<String>,
) -> Result<(), AppAndroidDeviceError> {
    action
        .audit
        .append(DeviceActionRecord {
            ts_ms: Utc::now().timestamp_millis(),
            principal: action.device_key.principal.clone(),
            workspace: action.device_key.workspace.clone(),
            device_id: action.device_key.device_id.clone(),
            tool: match &action.action {
                AppAndroidAction::Snapshot => "android_snapshot",
                AppAndroidAction::Screenshot { .. } => "android_screenshot",
                AppAndroidAction::Launch { .. } | AppAndroidAction::Close { .. } => "android_app",
                AppAndroidAction::Tap { .. }
                | AppAndroidAction::Type { .. }
                | AppAndroidAction::Key { .. }
                | AppAndroidAction::Scroll { .. } => "android_act",
            }
            .to_owned(),
            action: action.wire_action.to_owned(),
            foreground_package,
            verdict,
            detail,
            screenshot: matches!(&action.action, AppAndroidAction::Screenshot { .. }),
            connection_generation: Some(action.connection.connection_id().to_string()),
            play_integrity_verdict_digest: Some(
                action.connection.play_integrity_verdict_digest().to_owned(),
            ),
        })
        .await
        .map_err(|_| AppAndroidDeviceError::AuditUnavailable)
}

fn android_bridge_audit_code(error: &AppAndroidDeviceError) -> String {
    match error {
        AppAndroidDeviceError::Bridge(DeviceBridgeError::NotConnected(_)) => "device_not_connected",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::SessionUnavailable(_)) => {
            "device_session_unavailable"
        },
        AppAndroidDeviceError::Bridge(DeviceBridgeError::Timeout(_)) => "device_timeout",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::Disconnected) => "device_disconnected",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::DeviceError(_)) => "device_refused_action",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::AppProtected(_)) => "app_protected",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::EmptyResult) => "device_returned_nothing",
        AppAndroidDeviceError::Bridge(DeviceBridgeError::ConnectionChanged) => {
            "device_connection_changed"
        },
        AppAndroidDeviceError::Bridge(DeviceBridgeError::ResultTooLarge(_)) => {
            "device_result_too_large"
        },
        AppAndroidDeviceError::CancelledAfterStart => "device_action_cancelled",
        _ => "device_owner_error",
    }
    .to_owned()
}

struct AppAndroidPreparedCompletion {
    foreground_package: Option<String>,
    result: ActionResult,
    canonical_result: Vec<u8>,
    evidence_digest: AppDigest,
    evidence_bytes: u64,
    observation: Option<AppAndroidObservationEvidence>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireEnvelope {
    #[serde(rename = "isError")]
    is_error: bool,
    content: Vec<AppAndroidWireContent>,
    #[serde(rename = "structuredContent")]
    structured_content: AppAndroidWireStructured,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default, rename = "mimeType")]
    mime_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireStructured {
    apps_owner: AppAndroidWireOwnerReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireOwnerReceipt {
    schema: String,
    permit_nonce: String,
    action: String,
    target_package: Option<String>,
    foreground_package: Option<String>,
    snapshot_sha256: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    evidence_bytes: u64,
    evidence_nodes: u64,
    truncated: bool,
    outcome: String,
    result_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireSnapshot {
    format: String,
    app: String,
    width: u32,
    height: u32,
    total: u64,
    shown: u64,
    truncated: bool,
    snapshot_sha256: String,
    elements: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppAndroidWireScreenshotMeta {
    width: u32,
    height: u32,
    format: String,
}

fn project_android_result(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    raw: Value,
) -> Result<AppAndroidPreparedCompletion, AppAndroidDeviceError> {
    match &action.action {
        AppAndroidAction::Snapshot => project_android_snapshot(action, permit, raw),
        AppAndroidAction::Screenshot { .. } => project_android_screenshot(action, permit, raw),
        AppAndroidAction::Launch { .. }
        | AppAndroidAction::Close { .. }
        | AppAndroidAction::Tap { .. }
        | AppAndroidAction::Type { .. }
        | AppAndroidAction::Key { .. }
        | AppAndroidAction::Scroll { .. } => project_android_action(action, permit, raw),
    }
}

fn validate_android_owner_receipt(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    owner: &AppAndroidWireOwnerReceipt,
    evidence_bytes: u64,
    result_sha256: &str,
) -> Result<(), AppAndroidDeviceError> {
    let expected_observation = action
        .wire_arguments
        .pointer("/_magician_apps/observation/snapshot_sha256")
        .and_then(Value::as_str);
    if owner.schema != APP_ANDROID_OWNER_WIRE_SCHEMA
        || owner.permit_nonce != action.permit_nonce
        || owner.permit_nonce != digest_hex(permit.permit_digest())?
        || owner.action != action.action.action_name()
        || owner.target_package != action.target_package
        || owner.outcome != "settled"
        || owner.result_sha256 != result_sha256
        || owner.result_sha256.len() != 64
        || !owner
            .result_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || owner.evidence_bytes != evidence_bytes
        || owner.evidence_bytes > permit.claim().evidence_bytes
        || owner.evidence_nodes > permit.claim().evidence_nodes
        || owner.snapshot_sha256.as_deref() != expected_observation
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    if let Some(target) = action.target_package.as_deref() {
        if !action.policy.allowed_packages().contains(target) {
            return Err(AppAndroidDeviceError::InvalidOwnerResponse);
        }
        let may_leave_target = matches!(
            &action.action,
            AppAndroidAction::Close { .. }
                | AppAndroidAction::Key {
                    key: AppAndroidKey::Back | AppAndroidKey::Home,
                    ..
                }
        );
        if !may_leave_target && owner.foreground_package.as_deref() != Some(target) {
            return Err(AppAndroidDeviceError::InvalidOwnerResponse);
        }
    }
    Ok(())
}

fn project_android_screenshot(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    raw: Value,
) -> Result<AppAndroidPreparedCompletion, AppAndroidDeviceError> {
    let envelope: AppAndroidWireEnvelope =
        serde_json::from_value(raw).map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    if envelope.is_error || envelope.content.len() != 2 {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let image = &envelope.content[0];
    let metadata = &envelope.content[1];
    if image.kind != "image"
        || image.text.is_some()
        || image.mime_type.as_deref() != Some("image/jpeg")
        || metadata.kind != "text"
        || metadata.data.is_some()
        || metadata.mime_type.is_some()
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let image_base64 = image
        .data
        .as_deref()
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(image_base64)
        .map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    let evidence_bytes =
        u64::try_from(image_bytes.len()).map_err(|_| AppAndroidDeviceError::EvidenceTooLarge)?;
    let meta: AppAndroidWireScreenshotMeta = serde_json::from_str(
        metadata
            .text
            .as_deref()
            .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?,
    )
    .map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    let owner = envelope.structured_content.apps_owner;
    validate_android_owner_receipt(
        action,
        permit,
        &owner,
        evidence_bytes,
        &sha256_hex(&image_bytes),
    )?;
    if meta.width == 0
        || meta.height == 0
        || meta.format != "jpeg"
        || owner.width != Some(meta.width)
        || owner.height != Some(meta.height)
        || owner.truncated
        || owner.evidence_nodes != 0
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let foreground_package = owner
        .foreground_package
        .clone()
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let content_digest = AppDigest::blake3(&image_bytes);
    let result = ActionResult::Browser {
        data: serde_json::to_value(AppAndroidScreenshotResult {
            kind: "app_android_screenshot",
            success: true,
            foreground_package: foreground_package.clone(),
            content_digest: content_digest.clone(),
            width: meta.width,
            height: meta.height,
            mime_type: "image/jpeg",
            image_base64: image_base64.to_owned(),
        })
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
    };
    android_completion(
        action,
        permit,
        Some(foreground_package),
        result,
        evidence_bytes,
        &owner,
        None,
    )
}

fn project_android_action(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    raw: Value,
) -> Result<AppAndroidPreparedCompletion, AppAndroidDeviceError> {
    let envelope: AppAndroidWireEnvelope =
        serde_json::from_value(raw).map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    if envelope.is_error || envelope.content.len() != 1 {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let content = &envelope.content[0];
    if content.kind != "text" || content.data.is_some() || content.mime_type.is_some() {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let raw_text = content
        .text
        .as_deref()
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let evidence_bytes =
        u64::try_from(raw_text.len()).map_err(|_| AppAndroidDeviceError::EvidenceTooLarge)?;
    let owner = envelope.structured_content.apps_owner;
    validate_android_owner_receipt(
        action,
        permit,
        &owner,
        evidence_bytes,
        &sha256_hex(raw_text.as_bytes()),
    )?;
    if owner.width.is_some()
        || owner.height.is_some()
        || owner.truncated
        || owner.evidence_nodes > 1
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let target_package = action
        .target_package
        .clone()
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let foreground_package = owner
        .foreground_package
        .clone()
        .filter(|package| action.policy.allowed_packages().contains(package));
    let receipt_value = serde_json::to_value(&owner)
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    let receipt_digest = AppDigest::blake3_canonical_json(&receipt_value)
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    let result = ActionResult::Browser {
        data: serde_json::to_value(AppAndroidActionResult {
            kind: "app_android_action",
            success: true,
            action: action.action.action_name(),
            target_package,
            foreground_package: foreground_package.clone(),
            receipt_digest,
        })
        .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
    };
    android_completion(
        action,
        permit,
        foreground_package,
        result,
        evidence_bytes,
        &owner,
        None,
    )
}

fn android_completion(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    foreground_package: Option<String>,
    result: ActionResult,
    evidence_bytes: u64,
    owner: &AppAndroidWireOwnerReceipt,
    observation: Option<AppAndroidObservationEvidence>,
) -> Result<AppAndroidPreparedCompletion, AppAndroidDeviceError> {
    let canonical_result = canonical_json_bytes(
        &serde_json::to_value(&result)
            .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
    )
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    let evidence_digest = AppDigest::blake3_canonical_json(&json!({
        "schema": APP_ANDROID_OWNER_WIRE_SCHEMA,
        "action": action.action.action_name(),
        "owner_receipt": owner,
        "connection_generation": action.connection.connection_id().to_string(),
        "play_integrity_verdict_digest": action.connection.play_integrity_verdict_digest(),
    }))
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    AppInteractiveSettlementReceipt::preflight_completed(
        permit,
        &canonical_result,
        Some(&evidence_digest),
        evidence_bytes,
    )?;
    Ok(AppAndroidPreparedCompletion {
        foreground_package,
        result,
        canonical_result,
        evidence_digest,
        evidence_bytes,
        observation,
    })
}

fn project_android_snapshot(
    action: &AppAndroidEffectAction,
    permit: &super::interactive::AppInteractiveOwnerIoPermit,
    raw: Value,
) -> Result<AppAndroidPreparedCompletion, AppAndroidDeviceError> {
    let envelope: AppAndroidWireEnvelope =
        serde_json::from_value(raw).map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    if envelope.is_error || envelope.content.len() != 1 {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let content = envelope
        .content
        .into_iter()
        .next()
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    if content.kind != "text" || content.data.is_some() || content.mime_type.is_some() {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let raw_text = content
        .text
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let evidence_bytes =
        u64::try_from(raw_text.len()).map_err(|_| AppAndroidDeviceError::EvidenceTooLarge)?;
    let owner = envelope.structured_content.apps_owner;
    let wire: AppAndroidWireSnapshot =
        serde_json::from_str(&raw_text).map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    if owner.schema != APP_ANDROID_OWNER_WIRE_SCHEMA
        || owner.permit_nonce != action.permit_nonce
        || owner.permit_nonce != digest_hex(permit.permit_digest())?
        || owner.action != "snapshot"
        || owner.target_package.is_some()
        || owner.outcome != "settled"
        || owner.result_sha256 != sha256_hex(raw_text.as_bytes())
        || !action
            .policy
            .allowed_packages()
            .contains(owner.foreground_package.as_deref().unwrap_or_default())
        || owner.foreground_package.as_deref() != Some(wire.app.as_str())
        || owner.width != Some(wire.width)
        || owner.height != Some(wire.height)
        || owner.snapshot_sha256.as_deref() != Some(wire.snapshot_sha256.as_str())
        || owner.evidence_bytes != evidence_bytes
        || owner.evidence_nodes != wire.shown
        || owner.evidence_nodes > permit.claim().evidence_nodes
        || evidence_bytes > permit.claim().evidence_bytes
        || owner.truncated != wire.truncated
        || wire.format != "apps_compact_v1"
        || wire.total > APP_ANDROID_SNAPSHOT_NODE_CEILING
        || wire.shown > wire.total
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let expected_sha256 = android_snapshot_sha256(
        &wire.app,
        wire.width,
        wire.height,
        wire.truncated,
        &wire.elements,
    );
    if !constant_time_eq(expected_sha256.as_bytes(), wire.snapshot_sha256.as_bytes()) {
        return Err(AppAndroidDeviceError::PhysicalOwnerIdentityMismatch);
    }
    let projected = parse_android_nodes(&wire.elements, wire.width, wire.height)?;
    if u64::try_from(projected.nodes.len()).unwrap_or(u64::MAX) != wire.shown {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let geometry = AppInteractiveGeometry {
        width: wire.width,
        height: wire.height,
        scale_millis: 1000,
    };
    geometry.validate()?;
    let content_digest = AppDigest::blake3(raw_text.as_bytes());
    let public_digest = AppDigest::blake3_canonical_json(&json!({
        "schema":"magician.app-android-observation-public-ref.v1",
        "owner_target_digest":&action.owner_target_digest,
        "foreground_package":&wire.app,
        "snapshot_sha256":&wire.snapshot_sha256,
        "geometry":&geometry,
    }))
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    let public_ref = AppReference::parse(format!(
        "android-observation:{}",
        public_digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    let result_value = serde_json::to_value(&AppAndroidSnapshotResult {
        kind: "app_android_snapshot",
        success: true,
        device_target_ref: action.policy.device_target_ref().clone(),
        foreground_package: wire.app.clone(),
        geometry: geometry.clone(),
        content_digest: content_digest.clone(),
        observation_ref: public_ref.clone(),
        nodes: projected.nodes,
        total_nodes: wire.total,
        truncated: wire.truncated,
    })
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    let result = ActionResult::Browser { data: result_value };
    let canonical_result = canonical_json_bytes(
        &serde_json::to_value(&result)
            .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
    )
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    // The effect target is stable across reconnect/restart so already
    // completed bytes remain recoverable. The exact socket generation remains
    // attributable in the durable evidence/audit receipt instead.
    let evidence_digest = AppDigest::blake3_canonical_json(&json!({
        "schema": APP_ANDROID_OWNER_WIRE_SCHEMA,
        "content_digest": AppDigest::blake3(raw_text.as_bytes()),
        "connection_generation": action.connection.connection_id().to_string(),
        "play_integrity_verdict_digest": action.connection.play_integrity_verdict_digest(),
    }))
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    AppInteractiveSettlementReceipt::preflight_completed(
        permit,
        &canonical_result,
        Some(&evidence_digest),
        evidence_bytes,
    )?;
    Ok(AppAndroidPreparedCompletion {
        foreground_package: Some(wire.app.clone()),
        result,
        canonical_result,
        evidence_digest,
        evidence_bytes,
        observation: Some(AppAndroidObservationEvidence {
            public_ref,
            owner_target_digest: action.owner_target_digest.clone(),
            foreground_package: wire.app.clone(),
            snapshot_sha256: wire.snapshot_sha256.clone(),
            geometry,
            content_digest,
            labels_digest: projected.labels_digest,
            evidence_bytes,
            evidence_nodes: wire.shown,
            element_bounds: projected.element_bounds,
        }),
    })
}

struct AppAndroidProjectedNodes {
    nodes: Vec<AppAndroidSnapshotNode>,
    element_bounds: BTreeMap<AppReference, AppAndroidBounds>,
    labels_digest: AppDigest,
}

#[derive(Serialize)]
struct AppAndroidSemanticNode<'a> {
    text: Option<&'a str>,
    description: Option<&'a str>,
    clickable: bool,
    focusable: bool,
    scrollable: bool,
    checkable: bool,
    bounds: Option<AppAndroidBounds>,
}

fn parse_android_nodes(
    table: &str,
    width: u32,
    height: u32,
) -> Result<AppAndroidProjectedNodes, AppAndroidDeviceError> {
    let mut lines = table.lines();
    if lines.next() != Some("IDX | text | desc | flags | bounds") {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    let mut nodes = Vec::new();
    let mut element_bounds = BTreeMap::new();
    for line in lines {
        if nodes.len() >= usize::try_from(APP_ANDROID_SNAPSHOT_NODE_CEILING).unwrap_or(usize::MAX) {
            return Err(AppAndroidDeviceError::EvidenceTooLarge);
        }
        let fields = line.split(" | ").collect::<Vec<_>>();
        if fields.len() != 5
            || fields[1].chars().count() > MAX_APP_ANDROID_SEMANTIC_FIELD_CHARS
            || fields[2].chars().count() > MAX_APP_ANDROID_SEMANTIC_FIELD_CHARS
        {
            return Err(AppAndroidDeviceError::InvalidOwnerResponse);
        }
        let raw_index = fields[0]
            .parse::<u32>()
            .ok()
            .filter(|index| usize::try_from(*index).ok() == Some(nodes.len()))
            .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
        let flags = fields[3];
        if flags
            .chars()
            .any(|flag| !matches!(flag, 'c' | 'f' | 's' | 'k'))
            || flags.chars().collect::<BTreeSet<_>>().len() != flags.chars().count()
        {
            return Err(AppAndroidDeviceError::InvalidOwnerResponse);
        }
        let bounds = parse_android_bounds(fields[4], width, height)?;
        let element_ref =
            AppReference::parse(format!("android-element:{}", Uuid::new_v4().simple()))
                .map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
        let node = AppAndroidSnapshotNode {
            element_ref: element_ref.clone(),
            text: (!fields[1].is_empty()).then(|| fields[1].to_owned()),
            description: (!fields[2].is_empty()).then(|| fields[2].to_owned()),
            clickable: flags.contains('c'),
            focusable: flags.contains('f'),
            scrollable: flags.contains('s'),
            checkable: flags.contains('k'),
            bounds,
        };
        let _ = raw_index;
        if node.clickable {
            let bounds = node
                .bounds
                .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
            if element_bounds.insert(element_ref, bounds).is_some() {
                return Err(AppAndroidDeviceError::InvalidOwnerResponse);
            }
        }
        nodes.push(node);
    }
    let semantic = nodes
        .iter()
        .map(|node| AppAndroidSemanticNode {
            text: node.text.as_deref(),
            description: node.description.as_deref(),
            clickable: node.clickable,
            focusable: node.focusable,
            scrollable: node.scrollable,
            checkable: node.checkable,
            bounds: node.bounds,
        })
        .collect::<Vec<_>>();
    let labels_digest = AppDigest::blake3_canonical_json(
        &serde_json::to_value(&semantic)
            .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?,
    )
    .map_err(|error| AppAndroidDeviceError::Encoding(error.to_string()))?;
    Ok(AppAndroidProjectedNodes {
        nodes,
        element_bounds,
        labels_digest,
    })
}

fn parse_android_bounds(
    value: &str,
    width: u32,
    height: u32,
) -> Result<Option<AppAndroidBounds>, AppAndroidDeviceError> {
    if value.is_empty() {
        return Ok(None);
    }
    let body = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or(AppAndroidDeviceError::InvalidOwnerResponse)?;
    let values = body
        .split(',')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppAndroidDeviceError::InvalidOwnerResponse)?;
    if values.len() != 4
        || values[0] >= values[2]
        || values[1] >= values[3]
        || values[2] > width
        || values[3] > height
    {
        return Err(AppAndroidDeviceError::InvalidOwnerResponse);
    }
    Ok(Some(AppAndroidBounds {
        left: values[0],
        top: values[1],
        right: values[2],
        bottom: values[3],
    }))
}

fn android_snapshot_sha256(
    package: &str,
    width: u32,
    height: u32,
    truncated: bool,
    table: &str,
) -> String {
    let material = format!(
        "{APP_ANDROID_OWNER_WIRE_SCHEMA}\n{package}\n{width}x{height}\n{truncated}\n{table}"
    );
    let digest = Sha256::digest(material.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

async fn await_android_owner<T, F>(
    cancellation: &AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
    resource_deadline: Option<tokio::time::Instant>,
    future: F,
) -> Result<T, AppAndroidDeviceError>
where
    F: Future<Output = Result<T, AppAndroidDeviceError>>,
{
    let stop = wait_for_android_stop(cancellation.clone(), expires_at);
    tokio::pin!(stop);
    tokio::pin!(future);
    let resource_expiry = async {
        match resource_deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(resource_expiry);
    tokio::select! {
        biased;
        _ = &mut resource_expiry => {
            cancellation.cancel(AppInteractiveCancellationReason::Deadline);
            Err(AppAndroidDeviceError::CancelledAfterStart)
        },
        _ = &mut stop => {
            cancellation.cancel(AppInteractiveCancellationReason::Deadline);
            Err(AppAndroidDeviceError::CancelledAfterStart)
        },
        result = &mut future => result,
    }
}

async fn wait_for_android_stop(
    cancellation: AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
) {
    loop {
        let now = Utc::now();
        if cancellation.is_cancelled() || now >= expires_at {
            return;
        }
        let remaining = (expires_at - now)
            .to_std()
            .unwrap_or_else(|_| Duration::from_millis(1));
        tokio::time::sleep(remaining.min(Duration::from_millis(50))).await;
    }
}

fn valid_android_package(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= MAX_APP_ANDROID_PACKAGE_BYTES
        && value.contains('.')
        && value.split('.').all(|component| {
            !component.is_empty()
                && component
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic)
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn digest_hex(digest: &AppDigest) -> Result<&str, AppAndroidDeviceError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or_else(|| AppAndroidDeviceError::Encoding("invalid canonical digest".to_owned()))
}

#[derive(Debug, Error)]
pub enum AppAndroidDeviceError {
    #[error("Android target policy is invalid")]
    InvalidTargetPolicy,
    #[error("Android action input is outside the closed reviewed schema")]
    InvalidInput,
    #[error("Android action does not consume the exact fresh same-owner observation")]
    ObservationMismatch,
    #[error("Android action is not present in the reviewed grant")]
    ActionNotReviewed,
    #[error("Android descriptor, grant or implementation does not match review")]
    ReviewMismatch,
    #[error("the exact paired Android physical owner changed or is unavailable")]
    PhysicalOwnerIdentityMismatch,
    #[error("Android owner returned an invalid or unattributable response")]
    InvalidOwnerResponse,
    #[error("Android evidence exceeds its reviewed bound")]
    EvidenceTooLarge,
    #[error("Android action was cancelled after durable dispatch-start")]
    CancelledAfterStart,
    #[error("Android action does not match the common effect binding")]
    EffectBindingMismatch,
    #[error("Android result exceeds the reviewed bound")]
    ResultTooLarge,
    #[error("the durable Android device-action audit is unavailable")]
    AuditUnavailable,
    #[error("failed to encode Android identity/result: {0}")]
    Encoding(String),
    #[error(transparent)]
    Bridge(#[from] DeviceBridgeError),
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    Interactive(#[from] AppInteractiveError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target_ref() -> AppReference {
        AppReference::parse(format!("android-device:{}", "a".repeat(64))).unwrap()
    }

    fn review_digest() -> AppDigest {
        AppDigest::blake3(b"owner-reviewed-android-packages")
    }

    #[test]
    fn target_policy_requires_an_opaque_target_and_exact_packages() {
        let policy = AppAndroidTargetPolicy::reviewed(
            target_ref(),
            7,
            review_digest(),
            BTreeSet::from(["com.example.fixture".to_owned()]),
        )
        .unwrap();
        assert_eq!(policy.allowed_packages().len(), 1);
        assert!(AppAndroidTargetPolicy::reviewed(
            AppReference::parse("device:raw-serial").unwrap(),
            7,
            review_digest(),
            BTreeSet::from(["com.example.fixture".to_owned()]),
        )
        .is_err());
        assert!(AppAndroidTargetPolicy::reviewed(
            target_ref(),
            7,
            review_digest(),
            BTreeSet::from(["not a package".to_owned()]),
        )
        .is_err());
    }

    #[test]
    fn semantic_projection_mints_unguessable_refs_and_keeps_indices_private() {
        let table = concat!(
            "IDX | text | desc | flags | bounds\n",
            "0 | Continue | Next step | cf | [10,20,110,70]\n",
            "1 |  | Results | s | [0,80,200,400]"
        );
        let first = parse_android_nodes(table, 300, 600).unwrap();
        let second = parse_android_nodes(table, 300, 600).unwrap();
        assert_eq!(first.labels_digest, second.labels_digest);
        assert_eq!(first.nodes.len(), 2);
        assert!(first
            .nodes
            .iter()
            .all(|node| node.element_ref.as_str().starts_with("android-element:")));
        assert_ne!(
            first.nodes[0].element_ref, second.nodes[0].element_ref,
            "logical element refs must not be derived from low-entropy row indices"
        );
        assert_eq!(
            first.element_bounds.get(&first.nodes[0].element_ref),
            Some(&AppAndroidBounds {
                left: 10,
                top: 20,
                right: 110,
                bottom: 70,
            })
        );
        assert!(!first
            .element_bounds
            .contains_key(&first.nodes[1].element_ref));
        let public = serde_json::to_value(&first.nodes).unwrap().to_string();
        assert!(!public.contains("raw_index"));
        assert!(!public.contains("resource_id"));
    }

    #[test]
    fn semantic_projection_rejects_reordered_rows_and_out_of_view_bounds() {
        assert!(parse_android_nodes(
            "IDX | text | desc | flags | bounds\n1 | A | B | c | [0,0,1,1]",
            10,
            10,
        )
        .is_err());
        assert!(parse_android_nodes(
            "IDX | text | desc | flags | bounds\n0 | A | B | c | [0,0,11,1]",
            10,
            10,
        )
        .is_err());
    }

    #[test]
    fn handset_snapshot_digest_binds_package_geometry_truncation_and_tree() {
        let table = "IDX | text | desc | flags | bounds";
        let digest = android_snapshot_sha256("com.example.fixture", 1080, 2400, false, table);
        assert_eq!(digest.len(), 64);
        assert_ne!(
            digest,
            android_snapshot_sha256("com.example.other", 1080, 2400, false, table)
        );
        assert_ne!(
            digest,
            android_snapshot_sha256("com.example.fixture", 1080, 2400, true, table)
        );
        assert!(constant_time_eq(digest.as_bytes(), digest.as_bytes()));
        assert!(!constant_time_eq(digest.as_bytes(), b"short"));
    }

    #[test]
    fn handset_apps_owner_response_has_one_exact_structured_shape() {
        let raw_text = serde_json::json!({
            "format": "apps_compact_v1",
            "app": "com.example.fixture",
            "width": 1080,
            "height": 2400,
            "total": 0,
            "shown": 0,
            "truncated": false,
            "snapshot_sha256": "0".repeat(64),
            "elements": "IDX | text | desc | flags | bounds",
        })
        .to_string();
        let value = serde_json::json!({
            "isError": false,
            "content": [{ "type": "text", "text": raw_text }],
            "structuredContent": {
                "apps_owner": {
                    "schema": APP_ANDROID_OWNER_WIRE_SCHEMA,
                    "permit_nonce": "a".repeat(64),
                    "action": "snapshot",
                    "target_package": null,
                    "foreground_package": "com.example.fixture",
                    "width": 1080,
                    "height": 2400,
                    "snapshot_sha256": "0".repeat(64),
                    "evidence_bytes": 1,
                    "evidence_nodes": 0,
                    "truncated": false,
                    "outcome": "settled",
                    "result_sha256": sha256_hex(raw_text.as_bytes()),
                }
            }
        });
        assert!(serde_json::from_value::<AppAndroidWireEnvelope>(value.clone()).is_ok());

        let mut widened = value;
        widened["structuredContent"]["foreground_package"] =
            Value::String("com.example.fixture".to_owned());
        assert!(serde_json::from_value::<AppAndroidWireEnvelope>(widened).is_err());
    }

    #[test]
    fn only_snapshot_is_reviewed_and_resource_bounded() {
        let action_ref = AppReference::parse("action:android-snapshot").unwrap();
        let implementation = AppDigest::blake3(b"implementation-plan");
        let reviewed =
            reviewed_android_snapshot_action(action_ref.clone(), implementation.clone()).unwrap();
        assert_eq!(reviewed.action_ref(), &action_ref);
        assert_eq!(reviewed.class(), AppInteractiveActionClass::Observe);
        assert_eq!(reviewed.owner_implementation_digest(), &implementation);
        assert_eq!(
            reviewed.result_byte_ceiling(),
            APP_ANDROID_SNAPSHOT_RESULT_CEILING
        );
        let resources = android_snapshot_resource_ceilings(30).unwrap();
        assert_eq!(resources.max_sessions(), 1);
        assert_eq!(resources.max_steps(), 1);
        assert_eq!(
            resources.max_evidence_bytes(),
            APP_ANDROID_SNAPSHOT_EVIDENCE_CEILING
        );
    }
}
