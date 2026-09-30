//! Fail-closed desktop boundary for app-owned macOS computer use.
//!
//! The public host gateway still has a legacy raw AX route for non-App callers.
//! App execution must use this module instead: it verifies a short-lived
//! one-shot permit, current host/TCC/application identity and a closed action
//! before lowering to one fixed CUA verb. No app-facing string can select a
//! CUA action, script, process executable, selector or filesystem path.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use magician_app_contract::macos_host::{
    app_macos_host_parse_element_token, app_macos_host_protected_bundle_id,
    app_macos_host_valid_snapshot_id, AppMacosHostAction, SignedAppMacosHostRequest,
    APP_MACOS_HOST_MAX_EVIDENCE_BYTES, APP_MACOS_HOST_MAX_RESULT_BYTES,
    APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES,
};
use serde_json::{json, Value};
use tokio::sync::Notify;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

pub(crate) const APP_MACOS_HOST_MAX_REQUEST_BYTES: usize = 256 * 1024;
const APP_MACOS_HOST_MAX_LIVE_NONCES: usize = 4_096;
const APP_MACOS_HOST_MAX_ACTIVE_ACTIONS: usize = 32;
const APP_MACOS_HOST_MAX_CUA_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const APP_MACOS_HOST_MAX_CUA_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
const APP_MACOS_HOST_MAX_CUA_ARTIFACT_ENTRIES: usize = 256;
const APP_MACOS_HOST_MAX_APPLICATION_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const APP_MACOS_HOST_MAX_INFO_PLIST_BYTES: u64 = 4 * 1024 * 1024;
const APP_MACOS_HOST_MAX_CODE_RESOURCES_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone)]
struct AppMacosHostVerifier {
    key_id: String,
    signing_key: [u8; 32],
    host_identity_digest: String,
    owner_profile_digest: String,
    owner_implementation_digest: String,
    cua_driver_binary_digest: String,
    tcc_policy_digest: String,
    tcc_epoch: u64,
    application_identities: HashMap<String, String>,
    cua_driver_binary: PathBuf,
}

/// Desktop-owned verifier and one-shot replay ledger. Production starts with
/// no verifier, so the typed route is unavailable until the runtime/desktop
/// pairing owner installs an exact key and current identity snapshot.
#[derive(Default)]
pub(crate) struct AppMacosHostState {
    verifier: Option<AppMacosHostVerifier>,
    consumed_nonces: HashMap<String, i64>,
    active_actions: HashMap<String, AppMacosHostActiveAction>,
}

struct AppMacosHostActiveAction {
    request_signature: String,
    stop_signal: AppMacosHostStopSignal,
}

impl AppMacosHostState {
    pub(crate) fn clear_verifier(&mut self) {
        self.verifier = None;
        self.consumed_nonces.clear();
        for signal in self.active_actions.values() {
            signal.stop_signal.stop();
        }
        self.active_actions.clear();
    }

    #[allow(dead_code)] // Wired by the runtime/desktop pairing slice.
    pub(crate) fn install_verifier(
        &mut self,
        key_id: String,
        signing_key: [u8; 32],
        host_identity_digest: String,
        owner_profile_digest: String,
        owner_implementation_digest: String,
        cua_driver_binary_digest: String,
        tcc_policy_digest: String,
        tcc_epoch: u64,
        application_identities: HashMap<String, String>,
        prevalidated_binary: AppMacosPrevalidatedCuaBinary,
    ) -> Result<(), AppMacosHostBoundaryError> {
        let (cua_driver_binary, prevalidated_digest) = prevalidated_binary.into_parts();
        if key_id.is_empty()
            || key_id.len() > 64
            || signing_key == [0_u8; 32]
            || tcc_epoch == 0
            || application_identities.is_empty()
            || application_identities.len() > 64
            || !is_digest(&host_identity_digest)
            || !is_digest(&owner_profile_digest)
            || !is_digest(&owner_implementation_digest)
            || !is_digest(&cua_driver_binary_digest)
            || !is_digest(&tcc_policy_digest)
            || !cua_driver_binary.is_absolute()
            || prevalidated_digest != cua_driver_binary_digest
            || application_identities.iter().any(|(bundle, digest)| {
                bundle.is_empty()
                    || app_macos_host_protected_bundle_id(bundle)
                    || !is_digest(digest)
            })
        {
            return Err(AppMacosHostBoundaryError::InvalidVerifier);
        }
        self.verifier = Some(AppMacosHostVerifier {
            key_id,
            signing_key,
            host_identity_digest,
            owner_profile_digest,
            owner_implementation_digest,
            cua_driver_binary_digest,
            tcc_policy_digest,
            tcc_epoch,
            application_identities,
            cua_driver_binary,
        });
        self.consumed_nonces.clear();
        for signal in self.active_actions.values() {
            signal.stop_signal.stop();
        }
        self.active_actions.clear();
        Ok(())
    }

    pub(crate) fn verifier_matches(
        &self,
        key_id: &str,
        binary_digest: &str,
        owner_profile_digest: &str,
        owner_implementation_digest: &str,
    ) -> bool {
        self.verifier.as_ref().is_some_and(|verifier| {
            verifier.key_id == key_id
                && verifier.cua_driver_binary_digest == binary_digest
                && verifier.owner_profile_digest == owner_profile_digest
                && verifier.owner_implementation_digest == owner_implementation_digest
        })
    }

    pub(crate) fn authorize(
        &mut self,
        body: &[u8],
        now_ms: i64,
    ) -> Result<AuthorizedAppMacosHostAction, AppMacosHostBoundaryError> {
        if body.is_empty() || body.len() > APP_MACOS_HOST_MAX_REQUEST_BYTES {
            return Err(AppMacosHostBoundaryError::InvalidRequest);
        }
        let verifier = self
            .verifier
            .as_ref()
            .ok_or(AppMacosHostBoundaryError::Unavailable)?;
        let request: SignedAppMacosHostRequest =
            serde_json::from_slice(body).map_err(|_| AppMacosHostBoundaryError::InvalidRequest)?;
        request
            .verify(
                &verifier.signing_key,
                now_ms,
                &verifier.key_id,
                &verifier.host_identity_digest,
            )
            .map_err(|_| AppMacosHostBoundaryError::InvalidPermit)?;
        let claims = &request.claims;
        if claims.tcc_policy_digest != verifier.tcc_policy_digest
            || claims.tcc_epoch != verifier.tcc_epoch
            || claims.owner_profile_digest != verifier.owner_profile_digest
            || claims.owner_implementation_digest != verifier.owner_implementation_digest
            || claims.cua_driver_binary_digest != verifier.cua_driver_binary_digest
            || app_macos_host_protected_bundle_id(&claims.bundle_id)
            || verifier
                .application_identities
                .get(&claims.bundle_id)
                .is_none_or(|digest| digest != &claims.application_identity_digest)
        {
            return Err(AppMacosHostBoundaryError::StaleHostIdentity);
        }

        self.consumed_nonces
            .retain(|_, expires_at_ms| *expires_at_ms > now_ms);
        if self.consumed_nonces.contains_key(&claims.nonce)
            || self.consumed_nonces.len() >= APP_MACOS_HOST_MAX_LIVE_NONCES
            || self.active_actions.len() >= APP_MACOS_HOST_MAX_ACTIVE_ACTIONS
        {
            return Err(AppMacosHostBoundaryError::ReplayOrCapacity);
        }
        let response_byte_ceiling = claims
            .result_byte_ceiling
            .checked_add(claims.evidence_byte_ceiling)
            .and_then(|value| value.checked_add(APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES))
            .and_then(|value| usize::try_from(value).ok())
            .filter(|ceiling| {
                *ceiling > 0
                    && *ceiling
                        <= (APP_MACOS_HOST_MAX_RESULT_BYTES
                            + APP_MACOS_HOST_MAX_EVIDENCE_BYTES
                            + APP_MACOS_HOST_RESPONSE_ENVELOPE_BYTES)
                            as usize
            })
            .ok_or(AppMacosHostBoundaryError::InvalidPermit)?;
        let correlation_ref = format!("app-macos-host:{}", claims.nonce);
        let stop_signal = AppMacosHostStopSignal::new();
        let request_signature = request.signature.clone();
        self.consumed_nonces
            .insert(claims.nonce.clone(), claims.expires_at_ms);
        self.active_actions.insert(
            correlation_ref.clone(),
            AppMacosHostActiveAction {
                request_signature,
                stop_signal: stop_signal.clone(),
            },
        );
        Ok(AuthorizedAppMacosHostAction {
            action: request.action,
            correlation_ref,
            effect_binding_digest: claims.effect_binding_digest.clone(),
            result_byte_ceiling: claims.result_byte_ceiling,
            evidence_byte_ceiling: claims.evidence_byte_ceiling,
            application_identity_digest: claims.application_identity_digest.clone(),
            tcc_policy_digest: claims.tcc_policy_digest.clone(),
            tcc_epoch: claims.tcc_epoch,
            observation_content_digest: claims.observation_content_digest.clone(),
            observation_revalidation_byte_ceiling: claims.observation_revalidation_byte_ceiling,
            expires_at_ms: claims.expires_at_ms,
            response_byte_ceiling,
            stop_signal,
            cua_driver_binary: verifier.cua_driver_binary.clone(),
            cua_driver_binary_digest: verifier.cua_driver_binary_digest.clone(),
        })
    }

    pub(crate) fn finish_action(&mut self, correlation_ref: &str) {
        self.active_actions.remove(correlation_ref);
    }

    pub(crate) fn request_owner_stop(&self) -> usize {
        for action in self.active_actions.values() {
            action.stop_signal.stop();
        }
        self.active_actions.len()
    }

    pub(crate) fn cancel_exact(&self, body: &[u8]) -> Result<bool, AppMacosHostBoundaryError> {
        if body.is_empty() || body.len() > APP_MACOS_HOST_MAX_REQUEST_BYTES {
            return Err(AppMacosHostBoundaryError::InvalidRequest);
        }
        let request: SignedAppMacosHostRequest =
            serde_json::from_slice(body).map_err(|_| AppMacosHostBoundaryError::InvalidRequest)?;
        let correlation_ref = format!("app-macos-host:{}", request.claims.nonce);
        let Some(active) = self.active_actions.get(&correlation_ref) else {
            return Ok(false);
        };
        if !constant_time_text_eq(&active.request_signature, &request.signature) {
            return Err(AppMacosHostBoundaryError::InvalidPermit);
        }
        active.stop_signal.stop();
        Ok(true)
    }

    pub(crate) fn activity_status(&self) -> AppMacosHostActivityStatus {
        AppMacosHostActivityStatus {
            paired: self.verifier.is_some(),
            active_actions: u32::try_from(self.active_actions.len()).unwrap_or(u32::MAX),
            stop_requested: self
                .active_actions
                .values()
                .any(|action| action.stop_signal.is_stopped()),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppMacosHostActivityStatus {
    pub paired: bool,
    pub active_actions: u32,
    pub stop_requested: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct AppMacosHostStopSignal {
    stopped: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AppMacosHostStopSignal {
    pub(crate) fn new() -> Self {
        Self {
            stopped: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            // One action owns one stop waiter. `notify_one` retains a permit
            // across the narrow check-to-await race; `notify_waiters` would
            // drop the notification when the waiter was not registered yet.
            self.notify.notify_one();
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) async fn stopped(&self) {
        if self.is_stopped() {
            return;
        }
        self.notify.notified().await;
    }
}

#[derive(Debug)]
pub(crate) struct AuthorizedAppMacosHostAction {
    action: AppMacosHostAction,
    correlation_ref: String,
    effect_binding_digest: String,
    result_byte_ceiling: u64,
    evidence_byte_ceiling: u64,
    application_identity_digest: String,
    tcc_policy_digest: String,
    tcc_epoch: u64,
    observation_content_digest: Option<String>,
    observation_revalidation_byte_ceiling: Option<u64>,
    expires_at_ms: i64,
    response_byte_ceiling: usize,
    stop_signal: AppMacosHostStopSignal,
    cua_driver_binary: PathBuf,
    cua_driver_binary_digest: String,
}

impl AuthorizedAppMacosHostAction {
    pub(crate) fn correlation_ref(&self) -> &str {
        &self.correlation_ref
    }

    pub(crate) fn lower(self) -> Result<AppMacosLoweredCuaCall, AppMacosHostBoundaryError> {
        let bundle_id = self.action.bundle_id().to_owned();
        let process_id = self.action.process_id();
        let window_id = self.action.window_id();
        let requires_screen_recording = self.action.requires_screen_recording();
        let dynamic_observation_target =
            matches!(&self.action, AppMacosHostAction::ObserveApplication { .. });
        let mut element_binding = None;
        // Every lowering below is checked against CuaDriver 0.28's
        // `describe <tool>` input_schema (all are additionalProperties:false).
        let (verb, args, mutation) = match self.action {
            AppMacosHostAction::Launch { bundle_id } => {
                ("launch_app", json!({ "bundle_id": bundle_id }), true)
            },
            AppMacosHostAction::Focus { process_id, .. } => {
                ("bring_to_front", json!({ "pid": process_id }), true)
            },
            AppMacosHostAction::Observe {
                process_id,
                window_id,
                ..
            } => (
                "get_window_state",
                json!({ "pid": process_id, "window_id": window_id }),
                false,
            ),
            AppMacosHostAction::ObserveApplication { .. } => ("get_window_state", json!({}), false),
            AppMacosHostAction::CapturePixels { .. } => {
                // CuaDriver 0.28 has no standalone screenshot tool: window
                // pixels arrive base64-encoded inside `get_window_state`
                // (`include_accessibility_tree:false` is the capture-only
                // form) and `get_desktop_state` captures whole displays. Pixel
                // capture stays unavailable until the Apps owner reviews a
                // bounded pixel-evidence result shape for that reply.
                return Err(AppMacosHostBoundaryError::InvalidAction);
            },
            AppMacosHostAction::ClickElement {
                process_id,
                window_id,
                element_token,
                click_count,
                ..
            } => {
                element_binding = Some(AppMacosElementBinding::token(&element_token)?);
                (
                    if click_count == 2 {
                        "double_click"
                    } else {
                        "click"
                    },
                    json!({
                        "pid": process_id,
                        "window_id": window_id,
                        "element_token": element_token,
                    }),
                    true,
                )
            },
            AppMacosHostAction::TypeText {
                process_id,
                window_id,
                element_token,
                text,
                ..
            } => {
                element_binding = Some(AppMacosElementBinding::token(&element_token)?);
                (
                    "type_text",
                    json!({
                        "pid": process_id,
                        "window_id": window_id,
                        "element_token": element_token,
                        "text": text,
                    }),
                    true,
                )
            },
            AppMacosHostAction::PressKey {
                process_id,
                window_id,
                key,
                modifiers,
                ..
            } => (
                "press_key",
                json!({
                    "pid": process_id,
                    "window_id": window_id,
                    "key": key.as_cua_name(),
                    "modifiers": modifiers
                        .into_iter()
                        .map(|modifier| modifier.as_cua_name())
                        .chain(key.cua_implied_modifier())
                        .collect::<Vec<_>>(),
                }),
                true,
            ),
            AppMacosHostAction::ScrollElement {
                process_id,
                window_id,
                element_token,
                direction,
                amount,
                ..
            } => {
                element_binding = Some(AppMacosElementBinding::token(&element_token)?);
                (
                    "scroll",
                    json!({
                        "pid": process_id,
                        "window_id": window_id,
                        "element_token": element_token,
                        "direction": direction.as_cua_name(),
                        "amount": amount,
                    }),
                    true,
                )
            },
            AppMacosHostAction::DragElements {
                process_id,
                window_id,
                source_element_token,
                destination_element_token,
                screenshot_scale_millis,
                ..
            } => {
                element_binding = Some(AppMacosElementBinding::drag(
                    &source_element_token,
                    &destination_element_token,
                    screenshot_scale_millis,
                )?);
                // Pixel coordinates are filled only from the fresh fenced
                // snapshot; until then the call is unbound and refused.
                (
                    "drag",
                    json!({ "pid": process_id, "window_id": window_id }),
                    true,
                )
            },
        };
        Ok(AppMacosLoweredCuaCall {
            verb,
            args,
            mutation,
            correlation_ref: self.correlation_ref,
            effect_binding_digest: self.effect_binding_digest,
            result_byte_ceiling: self.result_byte_ceiling,
            evidence_byte_ceiling: self.evidence_byte_ceiling,
            application_identity_digest: self.application_identity_digest,
            tcc_policy_digest: self.tcc_policy_digest,
            tcc_epoch: self.tcc_epoch,
            observation_content_digest: self.observation_content_digest,
            observation_revalidation_byte_ceiling: self.observation_revalidation_byte_ceiling,
            expires_at_ms: self.expires_at_ms,
            response_byte_ceiling: self.response_byte_ceiling,
            stop_signal: self.stop_signal,
            cua_driver_binary: self.cua_driver_binary,
            cua_driver_binary_digest: self.cua_driver_binary_digest,
            bundle_id,
            process_id,
            window_id,
            requires_screen_recording,
            dynamic_observation_target,
            element_binding,
        })
    }
}

#[cfg(test)]
impl AuthorizedAppMacosHostAction {
    /// Live-owner tests only: an already-authorized action that skips pairing
    /// and permit verification. Every digest the owner re-verifies before I/O
    /// (binary, application identity, TCC policy, observation content) must
    /// still be the real current value or the owner refuses the action.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_live_owner_test(
        action: AppMacosHostAction,
        application_identity_digest: String,
        tcc_policy_digest: String,
        observation_content_digest: Option<String>,
        evidence_byte_ceiling: u64,
        response_byte_ceiling: usize,
        expires_at_ms: i64,
        cua_driver_binary: PathBuf,
        cua_driver_binary_digest: String,
    ) -> Self {
        let observation_revalidation_byte_ceiling = observation_content_digest
            .as_ref()
            .map(|_| evidence_byte_ceiling);
        Self {
            action,
            correlation_ref: "correlation:live-owner-test".to_owned(),
            effect_binding_digest: format!("blake3:{}", "0a".repeat(32)),
            result_byte_ceiling: 256 * 1024,
            evidence_byte_ceiling,
            application_identity_digest,
            tcc_policy_digest,
            tcc_epoch: 1,
            observation_content_digest,
            observation_revalidation_byte_ceiling,
            expires_at_ms,
            response_byte_ceiling,
            stop_signal: AppMacosHostStopSignal::new(),
            cua_driver_binary,
            cua_driver_binary_digest,
        }
    }
}

/// How an element-addressed call is rebound to the fresh snapshot minted by
/// the desktop's observation fence. CuaDriver 0.28 supersedes every older
/// `element_token` whenever `get_window_state` runs, so the token the runtime
/// observed can never be sent as-is after that fence; the element fence
/// (the element's own line and its ancestors' indexes and roles, unchanged)
/// is what proves its index names the same row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMacosElementBinding {
    Token {
        element_index: u32,
        bound: bool,
    },
    Drag {
        source_index: u32,
        destination_index: u32,
        screenshot_scale_millis: u16,
        bound: bool,
    },
}

impl AppMacosElementBinding {
    fn token(element_token: &str) -> Result<Self, AppMacosHostBoundaryError> {
        let (_, element_index) = app_macos_host_parse_element_token(element_token)
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        Ok(Self::Token {
            element_index,
            bound: false,
        })
    }

    fn drag(
        source_token: &str,
        destination_token: &str,
        screenshot_scale_millis: u16,
    ) -> Result<Self, AppMacosHostBoundaryError> {
        let (_, source_index) = app_macos_host_parse_element_token(source_token)
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        let (_, destination_index) = app_macos_host_parse_element_token(destination_token)
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        Ok(Self::Drag {
            source_index,
            destination_index,
            screenshot_scale_millis,
            bound: false,
        })
    }

    fn bound(self) -> bool {
        match self {
            Self::Token { bound, .. } | Self::Drag { bound, .. } => bound,
        }
    }
}

/// One row of a fresh CuaDriver 0.28 `elements[]` array, by index.
fn app_macos_fresh_element<'a>(
    elements: &'a [Value],
    element_index: u32,
) -> Option<&'a serde_json::Map<String, Value>> {
    let mut matches = elements
        .iter()
        .filter_map(Value::as_object)
        .filter(|element| {
            element.get("element_index").and_then(Value::as_u64) == Some(u64::from(element_index))
        });
    let element = matches.next()?;
    matches.next().is_none().then_some(element)
}

/// A fresh element an action may target: not menu chrome. Menu rows have no
/// element fence, so a menu item's index is never proven stable.
fn app_macos_fresh_target<'a>(
    elements: &'a [Value],
    element_index: u32,
) -> Option<&'a serde_json::Map<String, Value>> {
    app_macos_fresh_element(elements, element_index).filter(|element| {
        !element
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(magician_app_contract::macos_host::app_macos_host_is_menu_chrome_role)
    })
}

/// A fresh element's frame `{x,y,w,h}` in screen points. Frames CuaDriver
/// reports for virtualized off-viewport rows (`h:1`) are not drag targets.
fn app_macos_fresh_frame(element: &serde_json::Map<String, Value>) -> Option<(f64, f64, f64, f64)> {
    let frame = element.get("frame")?.as_object()?;
    let read = |name: &str| {
        frame
            .get(name)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
    };
    let (x, y, w, h) = (read("x")?, read("y")?, read("w")?, read("h")?);
    (w >= 2.0 && h >= 2.0).then_some((x, y, w, h))
}

#[derive(Debug)]
pub(crate) struct AppMacosLoweredCuaCall {
    verb: &'static str,
    args: Value,
    mutation: bool,
    correlation_ref: String,
    effect_binding_digest: String,
    result_byte_ceiling: u64,
    evidence_byte_ceiling: u64,
    application_identity_digest: String,
    tcc_policy_digest: String,
    tcc_epoch: u64,
    observation_content_digest: Option<String>,
    observation_revalidation_byte_ceiling: Option<u64>,
    expires_at_ms: i64,
    response_byte_ceiling: usize,
    bundle_id: String,
    process_id: Option<u32>,
    window_id: Option<u32>,
    requires_screen_recording: bool,
    dynamic_observation_target: bool,
    element_binding: Option<AppMacosElementBinding>,
    stop_signal: AppMacosHostStopSignal,
    cua_driver_binary: PathBuf,
    cua_driver_binary_digest: String,
}

impl AppMacosLoweredCuaCall {
    pub(crate) fn verb(&self) -> &'static str {
        self.verb
    }

    pub(crate) fn args_bytes(&self) -> Result<Vec<u8>, AppMacosHostBoundaryError> {
        serde_json::to_vec(&self.args).map_err(|_| AppMacosHostBoundaryError::InvalidAction)
    }

    pub(crate) fn mutation(&self) -> bool {
        self.mutation
    }

    pub(crate) fn correlation_ref(&self) -> &str {
        &self.correlation_ref
    }

    pub(crate) fn effect_binding_digest(&self) -> &str {
        &self.effect_binding_digest
    }

    pub(crate) fn response_byte_ceiling(&self) -> usize {
        self.response_byte_ceiling
    }

    pub(crate) fn result_byte_ceiling(&self) -> u64 {
        self.result_byte_ceiling
    }

    pub(crate) fn evidence_byte_ceiling(&self) -> u64 {
        self.evidence_byte_ceiling
    }

    pub(crate) fn application_identity_digest(&self) -> &str {
        &self.application_identity_digest
    }

    pub(crate) fn tcc_policy_digest(&self) -> &str {
        &self.tcc_policy_digest
    }

    pub(crate) fn tcc_epoch(&self) -> u64 {
        self.tcc_epoch
    }

    pub(crate) fn observation_content_digest(&self) -> Option<&str> {
        self.observation_content_digest.as_deref()
    }

    /// The element indexes whose fence the permit's
    /// `observation_content_digest` covers: the target of a token action, both
    /// drag endpoints in order, or none for a key press, which names no
    /// element and is fenced on its window
    /// (`app_macos_host_observation_fence_digest` with no indexes).
    pub(crate) fn observation_fence_indexes(&self) -> Vec<u32> {
        match self.element_binding {
            Some(AppMacosElementBinding::Token { element_index, .. }) => vec![element_index],
            Some(AppMacosElementBinding::Drag {
                source_index,
                destination_index,
                ..
            }) => vec![source_index, destination_index],
            None => Vec::new(),
        }
    }

    pub(crate) fn observation_revalidation_byte_ceiling(&self) -> Option<u64> {
        self.observation_revalidation_byte_ceiling
    }

    pub(crate) fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub(crate) fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    pub(crate) fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub(crate) fn window_id(&self) -> Option<u32> {
        self.window_id
    }

    pub(crate) fn requires_screen_recording(&self) -> bool {
        self.requires_screen_recording
    }

    pub(crate) fn dynamic_observation_target(&self) -> bool {
        self.dynamic_observation_target
    }

    pub(crate) fn bind_dynamic_observation_target(
        &mut self,
        process_id: u32,
        window_id: u32,
    ) -> Result<(), AppMacosHostBoundaryError> {
        if !self.dynamic_observation_target
            || self.process_id.is_some()
            || self.window_id.is_some()
            || process_id == 0
            || window_id == 0
        {
            return Err(AppMacosHostBoundaryError::InvalidAction);
        }
        self.process_id = Some(process_id);
        self.window_id = Some(window_id);
        self.args = json!({ "pid": process_id, "window_id": window_id });
        Ok(())
    }

    /// True while an element-addressed call still names the runtime's
    /// (now superseded) snapshot and must not reach the physical owner.
    pub(crate) fn requires_fresh_element_binding(&self) -> bool {
        self.element_binding.is_some_and(|binding| !binding.bound())
    }

    /// Rebind the element address to the fresh `get_window_state` reply whose
    /// element fence just matched the permit's. Token actions get `<fresh snapshot>:<same index>`;
    /// drags get window-local screenshot-pixel centres computed from the
    /// fresh frames, the fresh AXWindow frame and the observation's scale.
    pub(crate) fn bind_fresh_observation(
        &mut self,
        fresh: &Value,
    ) -> Result<(), AppMacosHostBoundaryError> {
        let Some(binding) = self.element_binding else {
            return Ok(());
        };
        if binding.bound() {
            return Err(AppMacosHostBoundaryError::InvalidAction);
        }
        let snapshot_id = fresh
            .get("snapshot_id")
            .and_then(Value::as_str)
            .filter(|snapshot_id| app_macos_host_valid_snapshot_id(snapshot_id))
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        let elements = fresh
            .get("elements")
            .and_then(Value::as_array)
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        let args = self
            .args
            .as_object_mut()
            .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
        match binding {
            AppMacosElementBinding::Token { element_index, .. } => {
                let fresh_token = format!("{snapshot_id}:{element_index}");
                let element = app_macos_fresh_target(elements, element_index)
                    .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
                if element.get("element_token").and_then(Value::as_str)
                    != Some(fresh_token.as_str())
                {
                    return Err(AppMacosHostBoundaryError::InvalidAction);
                }
                args.insert("element_token".to_owned(), Value::String(fresh_token));
                self.element_binding = Some(AppMacosElementBinding::Token {
                    element_index,
                    bound: true,
                });
            },
            AppMacosElementBinding::Drag {
                source_index,
                destination_index,
                screenshot_scale_millis,
                ..
            } => {
                // The snapshot is scoped to `window_id`; its AXWindow row is
                // the window whose screenshot defines the pixel origin.
                let window = app_macos_fresh_element(elements, 0)
                    .filter(|window| window.get("role").and_then(Value::as_str) == Some("AXWindow"))
                    .and_then(app_macos_fresh_frame)
                    .ok_or(AppMacosHostBoundaryError::InvalidAction)?;
                let scale = f64::from(screenshot_scale_millis) / 1_000.0;
                let centre = |index: u32| {
                    let (x, y, w, h) =
                        app_macos_fresh_target(elements, index).and_then(app_macos_fresh_frame)?;
                    let local_x = (x + w / 2.0 - window.0) * scale;
                    let local_y = (y + h / 2.0 - window.1) * scale;
                    (local_x >= 0.0
                        && local_y >= 0.0
                        && local_x < window.2 * scale
                        && local_y < window.3 * scale)
                        .then_some((local_x.round(), local_y.round()))
                };
                let (from_x, from_y) =
                    centre(source_index).ok_or(AppMacosHostBoundaryError::InvalidAction)?;
                let (to_x, to_y) =
                    centre(destination_index).ok_or(AppMacosHostBoundaryError::InvalidAction)?;
                for (name, value) in [
                    ("from_x", from_x),
                    ("from_y", from_y),
                    ("to_x", to_x),
                    ("to_y", to_y),
                ] {
                    args.insert(name.to_owned(), json!(value));
                }
                self.element_binding = Some(AppMacosElementBinding::Drag {
                    source_index,
                    destination_index,
                    screenshot_scale_millis,
                    bound: true,
                });
            },
        }
        Ok(())
    }

    pub(crate) fn stop_signal(&self) -> AppMacosHostStopSignal {
        self.stop_signal.clone()
    }

    pub(crate) fn cua_driver_binary(&self) -> &Path {
        &self.cua_driver_binary
    }

    pub(crate) fn cua_driver_binary_digest(&self) -> &str {
        &self.cua_driver_binary_digest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMacosHostBoundaryError {
    Unavailable,
    InvalidVerifier,
    InvalidRequest,
    InvalidPermit,
    StaleHostIdentity,
    ReplayOrCapacity,
    InvalidAction,
}

impl std::fmt::Display for AppMacosHostBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "typed app macOS host owner is not paired",
            Self::InvalidVerifier => "typed app macOS verifier configuration is invalid",
            Self::InvalidRequest => "typed app macOS request is invalid",
            Self::InvalidPermit => "typed app macOS permit is invalid or expired",
            Self::StaleHostIdentity => "typed app macOS host, TCC or application identity is stale",
            Self::ReplayOrCapacity => {
                "typed app macOS permit was replayed or replay ledger is full"
            },
            Self::InvalidAction => "typed app macOS action cannot be lowered",
        })
    }
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn constant_time_text_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

/// Re-resolve and hash the exact installed/running application immediately
/// before physical owner I/O. The digest includes the canonical bundle path,
/// Info.plist, code-signing resource envelope and executable bytes. A caller
/// cannot provide any path; Launch Services or the exact running PID owns path
/// resolution. The pairing owner must use this same function when it snapshots
/// `application_identities` for verifier installation.
#[allow(dead_code)] // Used by the authenticated pairing bootstrap slice.
pub(crate) async fn app_macos_host_current_application_identity_digest(
    bundle_id: &str,
    process_id: Option<u32>,
) -> Option<String> {
    let bundle_id = bundle_id.to_owned();
    tokio::task::spawn_blocking(move || {
        app_macos_host_current_application_identity_digest_blocking(&bundle_id, process_id)
    })
    .await
    .ok()
    .flatten()
}

pub(crate) fn app_macos_host_current_application_identity_digest_blocking(
    bundle_id: &str,
    process_id: Option<u32>,
) -> Option<String> {
    let (bundle_path, executable_path) =
        app_macos_host_resolve_application_paths(bundle_id, process_id)?;
    app_macos_host_hash_application_identity(bundle_id, &bundle_path, &executable_path)
}

#[cfg(target_os = "macos")]
fn app_macos_host_resolve_application_paths(
    bundle_id: &str,
    process_id: Option<u32>,
) -> Option<(PathBuf, PathBuf)> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSRunningApplication, NSWorkspace};
    use objc2_foundation::{NSBundle, NSString};

    autoreleasepool(|_| {
        if let Some(process_id) = process_id {
            let process_id = i32::try_from(process_id).ok()?;
            let running =
                NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)?;
            if running.isTerminated() || running.bundleIdentifier()?.to_string() != bundle_id {
                return None;
            }
            let bundle_path = PathBuf::from(running.bundleURL()?.path()?.to_string());
            let executable_path = PathBuf::from(running.executableURL()?.path()?.to_string());
            return Some((bundle_path, executable_path));
        }

        let identifier = NSString::from_str(bundle_id);
        let bundle_url =
            NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&identifier)?;
        let bundle = NSBundle::bundleWithURL(&bundle_url)?;
        if bundle.bundleIdentifier()?.to_string() != bundle_id {
            return None;
        }
        Some((
            PathBuf::from(bundle.bundleURL().path()?.to_string()),
            PathBuf::from(bundle.executableURL()?.path()?.to_string()),
        ))
    })
}

#[cfg(not(target_os = "macos"))]
fn app_macos_host_resolve_application_paths(
    _bundle_id: &str,
    _process_id: Option<u32>,
) -> Option<(PathBuf, PathBuf)> {
    None
}

fn app_macos_host_hash_application_identity(
    bundle_id: &str,
    bundle_path: &Path,
    executable_path: &Path,
) -> Option<String> {
    let canonical_bundle = bundle_path.canonicalize().ok()?;
    let canonical_executable = executable_path.canonicalize().ok()?;
    if !canonical_bundle.is_absolute()
        || !canonical_executable.starts_with(&canonical_bundle)
        || std::fs::symlink_metadata(executable_path)
            .ok()?
            .file_type()
            .is_symlink()
    {
        return None;
    }
    let info_plist = canonical_bundle.join("Contents/Info.plist");
    let code_resources = canonical_bundle.join("Contents/_CodeSignature/CodeResources");
    let mut hasher = blake3::Hasher::new();
    hash_app_macos_identity_bytes(&mut hasher, "domain", b"magician.app-macos-identity.v1\0");
    hash_app_macos_identity_bytes(&mut hasher, "bundle_id", bundle_id.as_bytes());
    hash_app_macos_identity_bytes(
        &mut hasher,
        "canonical_bundle_path",
        canonical_bundle.as_os_str().as_encoded_bytes(),
    );
    hash_app_macos_identity_file(
        &mut hasher,
        "info_plist",
        &info_plist,
        APP_MACOS_HOST_MAX_INFO_PLIST_BYTES,
        &canonical_bundle,
    )?;
    hash_app_macos_identity_file(
        &mut hasher,
        "code_resources",
        &code_resources,
        APP_MACOS_HOST_MAX_CODE_RESOURCES_BYTES,
        &canonical_bundle,
    )?;
    hash_app_macos_identity_file(
        &mut hasher,
        "executable",
        &canonical_executable,
        APP_MACOS_HOST_MAX_APPLICATION_EXECUTABLE_BYTES,
        &canonical_bundle,
    )?;
    Some(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn hash_app_macos_identity_bytes(hasher: &mut blake3::Hasher, label: &str, bytes: &[u8]) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_app_macos_identity_file(
    hasher: &mut blake3::Hasher,
    label: &str,
    path: &Path,
    byte_ceiling: u64,
    canonical_bundle: &Path,
) -> Option<()> {
    let path_metadata = std::fs::symlink_metadata(path).ok()?;
    if path_metadata.file_type().is_symlink() {
        return None;
    }
    let canonical_path = path.canonicalize().ok()?;
    if !canonical_path.starts_with(canonical_bundle) {
        return None;
    }
    let mut file = std::fs::File::open(&canonical_path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > byte_ceiling {
        return None;
    }
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    let mut observed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes = file.read(&mut buffer).ok()?;
        if bytes == 0 {
            break;
        }
        observed = observed.checked_add(u64::try_from(bytes).ok()?)?;
        if observed > byte_ceiling {
            return None;
        }
        hasher.update(&buffer[..bytes]);
    }
    (observed == metadata.len()).then_some(())
}

pub(crate) fn app_macos_host_binary_digest(path: &Path) -> Option<String> {
    app_macos_host_read_binary_identity(path).map(|identity| identity.digest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMacosCuaArtifactEntryKind {
    Directory,
    File { executable: bool },
}

#[derive(Debug, Clone)]
struct AppMacosCuaArtifactEntry {
    /// Relative to the artifact base; the first component is the bundle
    /// directory (`CuaDriver.app`) or, for a bare executable, its file name.
    relative: PathBuf,
    kind: AppMacosCuaArtifactEntryKind,
}

/// Hashed identity of the unit a CUA executable runs as. CuaDriver 0.28
/// ships as a signed `.app` whose executable carries restricted entitlements
/// (application-identifier, keychain-access-groups) that AMFI honours only
/// beside the bundle's embedded provisioning profile: a lone copy of the
/// executable is killed at exec. The identity therefore covers every file of
/// the enclosing bundle, and staging copies the whole bundle.
struct AppMacosBinaryIdentity {
    canonical_path: PathBuf,
    /// Directory the entries are relative to (the bundle's parent).
    base: PathBuf,
    executable_relative: PathBuf,
    entries: Vec<AppMacosCuaArtifactEntry>,
    /// Bound digest: artifact content plus the executable's canonical path
    /// and file identity, so equal bytes at another path never alias.
    digest: String,
    /// Path-independent content digest; names the staged artifact.
    content_digest: String,
}

/// Move-only proof that the exact staged executable was hashed on bounded
/// blocking capacity before the async host-state mutex was acquired.
pub(crate) struct AppMacosPrevalidatedCuaBinary {
    canonical_path: PathBuf,
    digest: String,
}

impl AppMacosPrevalidatedCuaBinary {
    pub(crate) fn path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    fn into_parts(self) -> (PathBuf, String) {
        (self.canonical_path, self.digest)
    }
}

pub(crate) fn app_macos_host_prevalidate_binary(
    path: &Path,
    expected_digest: &str,
) -> Option<AppMacosPrevalidatedCuaBinary> {
    let identity = app_macos_host_read_binary_identity(path)?;
    (identity.digest == expected_digest).then_some(AppMacosPrevalidatedCuaBinary {
        canonical_path: identity.canonical_path,
        digest: identity.digest,
    })
}

/// `<Name>.app/Contents/MacOS/<exe>` → the `.app` directory; anything else
/// is a bare executable that is its own artifact.
fn app_macos_host_cua_artifact_root(executable: &Path) -> PathBuf {
    let bundle = executable
        .parent()
        .filter(|macos| macos.file_name().is_some_and(|name| name == "MacOS"))
        .and_then(Path::parent)
        .filter(|contents| contents.file_name().is_some_and(|name| name == "Contents"))
        .and_then(Path::parent)
        .filter(|bundle| {
            bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        });
    bundle.map_or_else(|| executable.to_path_buf(), Path::to_path_buf)
}

/// Bounded, symlink-free, deterministic listing of the artifact tree.
fn app_macos_host_collect_cua_artifact(
    artifact_root: &Path,
    base: &Path,
) -> Option<Vec<AppMacosCuaArtifactEntry>> {
    let mut entries = Vec::new();
    let mut pending = vec![artifact_root.to_path_buf()];
    let mut total_bytes = 0_u64;
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        let relative = path.strip_prefix(base).ok()?.to_path_buf();
        if metadata.file_type().is_symlink() || relative.as_os_str().is_empty() {
            return None;
        }
        if metadata.is_dir() {
            for child in std::fs::read_dir(&path).ok()? {
                pending.push(child.ok()?.path());
            }
            entries.push(AppMacosCuaArtifactEntry {
                relative,
                kind: AppMacosCuaArtifactEntryKind::Directory,
            });
        } else if metadata.is_file() {
            total_bytes = total_bytes.checked_add(metadata.len())?;
            #[cfg(unix)]
            let executable = metadata.mode() & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = true;
            entries.push(AppMacosCuaArtifactEntry {
                relative,
                kind: AppMacosCuaArtifactEntryKind::File { executable },
            });
        } else {
            return None;
        }
        if entries.len() > APP_MACOS_HOST_MAX_CUA_ARTIFACT_ENTRIES
            || total_bytes > APP_MACOS_HOST_MAX_CUA_ARTIFACT_BYTES
        {
            return None;
        }
    }
    entries.sort_by(|left, right| {
        left.relative
            .as_os_str()
            .as_encoded_bytes()
            .cmp(right.relative.as_os_str().as_encoded_bytes())
    });
    Some(entries)
}

fn app_macos_host_read_binary_identity(path: &Path) -> Option<AppMacosBinaryIdentity> {
    if !path.is_absolute()
        || std::fs::symlink_metadata(path)
            .ok()?
            .file_type()
            .is_symlink()
    {
        return None;
    }
    let canonical_path = path.canonicalize().ok()?;
    if !canonical_path.is_absolute() {
        return None;
    }
    let executable_metadata = std::fs::symlink_metadata(&canonical_path).ok()?;
    if !executable_metadata.is_file()
        || executable_metadata.len() == 0
        || executable_metadata.len() > APP_MACOS_HOST_MAX_CUA_BINARY_BYTES
    {
        return None;
    }
    let artifact_root = app_macos_host_cua_artifact_root(&canonical_path);
    let base = artifact_root.parent()?.to_path_buf();
    let executable_relative = canonical_path.strip_prefix(&base).ok()?.to_path_buf();
    let entries = app_macos_host_collect_cua_artifact(&artifact_root, &base)?;
    if !entries.iter().any(|entry| {
        entry.relative == executable_relative
            && entry.kind == AppMacosCuaArtifactEntryKind::File { executable: true }
    }) {
        return None;
    }

    let mut bound = blake3::Hasher::new();
    let mut content = blake3::Hasher::new();
    hash_app_macos_identity_bytes(
        &mut bound,
        "domain",
        b"magician.app-macos-cua-executable-identity.v3\0",
    );
    hash_app_macos_identity_bytes(
        &mut content,
        "domain",
        b"magician.app-macos-cua-artifact-content.v3\0",
    );
    hash_app_macos_identity_bytes(
        &mut bound,
        "canonical_path",
        canonical_path.as_os_str().as_encoded_bytes(),
    );
    #[cfg(unix)]
    {
        let identity = &executable_metadata;
        hash_app_macos_identity_bytes(&mut bound, "device", &identity.dev().to_le_bytes());
        hash_app_macos_identity_bytes(&mut bound, "inode", &identity.ino().to_le_bytes());
        hash_app_macos_identity_bytes(&mut bound, "mode", &identity.mode().to_le_bytes());
        hash_app_macos_identity_bytes(&mut bound, "owner", &identity.uid().to_le_bytes());
        hash_app_macos_identity_bytes(&mut bound, "group", &identity.gid().to_le_bytes());
    }
    for hasher in [&mut bound, &mut content] {
        hash_app_macos_identity_bytes(
            hasher,
            "executable",
            executable_relative.as_os_str().as_encoded_bytes(),
        );
    }
    let mut buffer = vec![0_u8; 64 * 1024];
    for entry in &entries {
        let kind: &[u8] = match entry.kind {
            AppMacosCuaArtifactEntryKind::Directory => b"directory",
            AppMacosCuaArtifactEntryKind::File { executable: true } => b"executable-file",
            AppMacosCuaArtifactEntryKind::File { executable: false } => b"file",
        };
        for hasher in [&mut bound, &mut content] {
            hash_app_macos_identity_bytes(
                hasher,
                "entry",
                entry.relative.as_os_str().as_encoded_bytes(),
            );
            hash_app_macos_identity_bytes(hasher, "kind", kind);
        }
        if entry.kind == AppMacosCuaArtifactEntryKind::Directory {
            continue;
        }
        let mut file = std::fs::File::open(base.join(&entry.relative)).ok()?;
        let before = file.metadata().ok()?;
        if !before.is_file() {
            return None;
        }
        for hasher in [&mut bound, &mut content] {
            hasher.update(&before.len().to_le_bytes());
        }
        let mut observed = 0_u64;
        loop {
            let bytes = file.read(&mut buffer).ok()?;
            if bytes == 0 {
                break;
            }
            observed = observed.checked_add(u64::try_from(bytes).ok()?)?;
            if observed > before.len() {
                return None;
            }
            bound.update(&buffer[..bytes]);
            content.update(&buffer[..bytes]);
        }
        let after = file.metadata().ok()?;
        if observed != before.len()
            || before.len() != after.len()
            || before.modified().ok()? != after.modified().ok()?
        {
            return None;
        }
    }
    Some(AppMacosBinaryIdentity {
        canonical_path,
        base,
        executable_relative,
        entries,
        digest: format!("blake3:{}", bound.finalize().to_hex()),
        content_digest: format!("blake3:{}", content.finalize().to_hex()),
    })
}

/// Copy an owner-selected CUA artifact (the whole signed `.app` bundle when
/// the executable lives in one) into the desktop's private,
/// content-addressed artifact directory. Pairing persists only the returned
/// staged executable path and signs its v3 bundle+path+file-identity digest.
#[cfg(unix)]
pub(crate) fn app_macos_host_stage_binary(
    source: &Path,
    owner_root: &Path,
) -> Option<(PathBuf, String)> {
    let source_identity = app_macos_host_read_binary_identity(source)?;
    if !owner_root.is_absolute() {
        return None;
    }
    let parent = owner_root.parent()?;
    reject_symlink_path_components(parent)?;
    let canonical_parent = parent.canonicalize().ok()?;
    if canonical_parent != parent {
        return None;
    }
    match std::fs::symlink_metadata(owner_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {},
        Ok(_) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(false)
                .mode(0o700)
                .create(owner_root)
                .ok()?;
        },
        Err(_) => return None,
    }
    let root_metadata = std::fs::symlink_metadata(owner_root).ok()?;
    if !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || root_metadata.mode() & 0o077 != 0
        || root_metadata.uid() != std::fs::symlink_metadata(parent).ok()?.uid()
    {
        return None;
    }
    let canonical_root = owner_root.canonicalize().ok()?;
    if canonical_root != canonical_parent.join(owner_root.file_name()?) {
        return None;
    }
    let content_hex = source_identity.content_digest.strip_prefix("blake3:")?;
    let staged_root = canonical_root.join(format!("cua-owner-v3-{content_hex}"));
    if std::fs::symlink_metadata(&staged_root).is_err() {
        let temporary = canonical_root.join(format!(".stage-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&temporary)
            .ok()?;
        let committed = (|| {
            for entry in &source_identity.entries {
                let destination = temporary.join(&entry.relative);
                match entry.kind {
                    AppMacosCuaArtifactEntryKind::Directory => {
                        std::fs::DirBuilder::new()
                            .recursive(false)
                            .mode(0o700)
                            .create(&destination)
                            .ok()?;
                    },
                    AppMacosCuaArtifactEntryKind::File { executable } => {
                        let mut from =
                            std::fs::File::open(source_identity.base.join(&entry.relative)).ok()?;
                        let mut staged = std::fs::OpenOptions::new()
                            .create_new(true)
                            .write(true)
                            .mode(0o600)
                            .open(&destination)
                            .ok()?;
                        std::io::copy(&mut from, &mut staged).ok()?;
                        staged.sync_all().ok()?;
                        std::fs::set_permissions(
                            &destination,
                            std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
                        )
                        .ok()?;
                    },
                }
            }
            // Directory rename is create-if-absent for a non-empty source: an
            // attacker-planted or concurrently published artifact is never
            // replaced, and the content check below rejects a foreign one.
            std::fs::rename(&temporary, &staged_root).ok()?;
            std::fs::File::open(&canonical_root).ok()?.sync_all().ok()?;
            Some(())
        })();
        if committed.is_none() {
            let _ = std::fs::remove_dir_all(&temporary);
        }
    }
    let staged_executable = staged_root.join(&source_identity.executable_relative);
    let staged_identity = app_macos_host_read_binary_identity(&staged_executable)?;
    (staged_identity.content_digest == source_identity.content_digest
        && staged_identity.canonical_path.starts_with(&staged_root))
    .then_some((staged_identity.canonical_path, staged_identity.digest))
}

#[cfg(unix)]
fn reject_symlink_path_components(path: &Path) -> Option<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
    }
    Some(())
}

#[cfg(not(unix))]
pub(crate) fn app_macos_host_stage_binary(
    _source: &Path,
    _owner_root: &Path,
) -> Option<(PathBuf, String)> {
    None
}

/// Staged executable whose full v3 identity (bundle bytes, canonical path,
/// file identity) matched the approved digest immediately before spawn.
///
/// The owner execs this path in place. An earlier design exec'd an unlinked
/// `/dev/fd/N` copy so later path replacement could not affect exec, but
/// macOS refuses exec through `/dev/fd` and AMFI kills a CuaDriver 0.28
/// executable separated from its signed bundle. The residual window is a
/// same-uid writer changing the private, owner-only (0700 dir, 0500/0400
/// file) staged bundle between this re-hash and exec.
pub(crate) struct AppMacosVerifiedCuaExecutable {
    canonical_path: PathBuf,
}

impl AppMacosVerifiedCuaExecutable {
    pub(crate) fn command_path(&self) -> &Path {
        &self.canonical_path
    }
}

pub(crate) fn app_macos_host_verify_executable(
    staged_path: &Path,
    expected_digest: &str,
) -> Option<AppMacosVerifiedCuaExecutable> {
    let identity = app_macos_host_read_binary_identity(staged_path)?;
    (identity.digest == expected_digest).then_some(AppMacosVerifiedCuaExecutable {
        canonical_path: identity.canonical_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_identity_changes_with_executable_or_signing_envelope() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-identity-{}-{unique}",
            std::process::id()
        ));
        let contents = root.join("Example.app/Contents");
        let signature = contents.join("_CodeSignature");
        let executable = contents.join("MacOS/Example");
        std::fs::create_dir_all(&signature).expect("signature directory");
        std::fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable directory");
        std::fs::write(contents.join("Info.plist"), b"plist-v1").expect("plist");
        std::fs::write(signature.join("CodeResources"), b"signature-v1").expect("signature");
        std::fs::write(&executable, b"executable-v1").expect("executable");
        let bundle = root.join("Example.app");
        let first =
            app_macos_host_hash_application_identity("com.example.Editor", &bundle, &executable)
                .expect("first digest");
        std::fs::write(&executable, b"executable-v2").expect("changed executable");
        let changed_executable =
            app_macos_host_hash_application_identity("com.example.Editor", &bundle, &executable)
                .expect("changed digest");
        assert_ne!(first, changed_executable);
        std::fs::write(&executable, b"executable-v1").expect("restored executable");
        std::fs::write(signature.join("CodeResources"), b"signature-v2")
            .expect("changed signature");
        let changed_signature =
            app_macos_host_hash_application_identity("com.example.Editor", &bundle, &executable)
                .expect("changed signature digest");
        assert_ne!(first, changed_signature);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn cua_identity_binds_staged_path_and_exec_verifies_in_place() {
        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-cua-stage-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let root = root.canonicalize().expect("canonical root");
        let first = root.join("first-cua");
        let second = root.join("second-cua");
        std::fs::write(&first, b"#!/bin/sh\nexit 0\n").expect("first");
        std::fs::write(&second, b"#!/bin/sh\nexit 0\n").expect("second");
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o700))
            .expect("first mode");
        std::fs::set_permissions(&second, std::fs::Permissions::from_mode(0o700))
            .expect("second mode");
        assert_ne!(
            app_macos_host_binary_digest(&first),
            app_macos_host_binary_digest(&second),
            "same bytes at a different executable path must not alias"
        );

        let artifact_root = root.join("owner-artifacts");
        let (staged, digest) = app_macos_host_stage_binary(&first, &artifact_root).expect("stage");
        let verified =
            app_macos_host_verify_executable(&staged, &digest).expect("verified executable");
        // macOS refuses exec through /dev/fd; the staged path itself is run.
        assert_eq!(verified.command_path(), staged.as_path());
        assert!(staged.starts_with(artifact_root.canonicalize().expect("artifact root")));
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o700))
            .expect("make staged artifact owner-writable for hostile tamper");
        std::fs::write(&staged, b"#!/bin/sh\nexit 99\n").expect("replace staged bytes");
        assert!(app_macos_host_verify_executable(&staged, &digest).is_none());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn cua_bundle_executable_stages_and_binds_the_whole_signed_bundle() {
        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-cua-bundle-{}",
            uuid::Uuid::new_v4()
        ));
        let contents = root.join("Tool.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).expect("bundle");
        let root = root.canonicalize().expect("canonical root");
        let contents = root.join("Tool.app/Contents");
        let executable = contents.join("MacOS/tool");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("executable");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("executable mode");
        std::fs::write(contents.join("Info.plist"), b"plist-v1").expect("plist");
        std::fs::write(contents.join("embedded.provisionprofile"), b"profile-v1").expect("profile");

        let before = app_macos_host_binary_digest(&executable).expect("bundle digest");
        let artifact_root = root.join("owner-artifacts");
        let (staged, digest) =
            app_macos_host_stage_binary(&executable, &artifact_root).expect("stage bundle");
        assert!(staged.ends_with("Tool.app/Contents/MacOS/tool"));
        let staged_contents = staged
            .parent()
            .and_then(Path::parent)
            .expect("staged contents");
        assert_eq!(
            std::fs::read(staged_contents.join("embedded.provisionprofile")).expect("profile"),
            b"profile-v1"
        );
        assert!(app_macos_host_verify_executable(&staged, &digest).is_some());

        // A change anywhere in the signed bundle is a different owner.
        std::fs::write(contents.join("embedded.provisionprofile"), b"profile-v2")
            .expect("changed profile");
        assert_ne!(
            app_macos_host_binary_digest(&executable).expect("changed digest"),
            before
        );
        std::os::unix::fs::symlink(&root, contents.join("escape")).expect("symlink");
        assert!(app_macos_host_binary_digest(&executable).is_none());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn staged_owner_root_rejects_preplanted_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-cua-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let root = root.canonicalize().expect("canonical root");
        let source = root.join("cua");
        let redirect = root.join("redirect");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("source");
        std::fs::create_dir(&redirect).expect("redirect");
        let owner_root = root.join("owner-artifacts");
        symlink(&redirect, &owner_root).expect("preplant symlink");
        assert!(app_macos_host_stage_binary(&source, &owner_root).is_none());
        assert!(std::fs::read_dir(&redirect)
            .expect("redirect contents")
            .next()
            .is_none());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn unpaired_owner_fails_closed_before_lowering() {
        let mut state = AppMacosHostState::default();
        assert_eq!(
            state.authorize(b"{}", 1_000).unwrap_err(),
            AppMacosHostBoundaryError::Unavailable
        );
    }

    fn authorized(action: AppMacosHostAction) -> AuthorizedAppMacosHostAction {
        AuthorizedAppMacosHostAction {
            action,
            correlation_ref: "correlation:1".to_owned(),
            effect_binding_digest: format!("blake3:{}", "01".repeat(32)),
            result_byte_ceiling: 4_096,
            evidence_byte_ceiling: 0,
            application_identity_digest: format!("blake3:{}", "06".repeat(32)),
            tcc_policy_digest: format!("blake3:{}", "08".repeat(32)),
            tcc_epoch: 1,
            observation_content_digest: Some(format!("blake3:{}", "05".repeat(32))),
            observation_revalidation_byte_ceiling: Some(8_192),
            expires_at_ms: 10_000,
            response_byte_ceiling: 4_096,
            stop_signal: AppMacosHostStopSignal::new(),
            cua_driver_binary: PathBuf::from(
                "/Applications/CuaDriver.app/Contents/MacOS/cua-driver",
            ),
            cua_driver_binary_digest: format!("blake3:{}", "03".repeat(32)),
        }
    }

    fn lowered_args(lowered: &AppMacosLoweredCuaCall) -> Value {
        serde_json::from_slice(&lowered.args_bytes().expect("args")).expect("json")
    }

    /// Fresh fenced `get_window_state` reply in CuaDriver 0.28's shape: a
    /// window at (546,132) 740x625pt holding rows 3 and 7.
    fn fresh_window_state() -> Value {
        json!({
            "snapshot_id": "s00000007",
            "elements": [
                {"element_index": 0, "element_token": "s00000007:0", "role": "AXWindow",
                 "depth": 0, "frame": {"x": 546.0, "y": 132.0, "w": 740.0, "h": 625.0}},
                {"element_index": 3, "element_token": "s00000007:3", "role": "AXRow",
                 "depth": 3, "frame": {"x": 546.0, "y": 230.0, "w": 232.0, "h": 46.0}},
                {"element_index": 7, "element_token": "s00000007:7", "role": "AXButton",
                 "depth": 5, "frame": {"x": 562.0, "y": 334.0, "w": 123.0, "h": 38.0}},
                {"element_index": 9, "element_token": "s00000007:9", "role": "AXRow",
                 "depth": 3, "frame": {"x": 546.0, "y": 900.0, "w": 232.0, "h": 1.0}}
            ]
        })
    }

    #[test]
    fn lowered_surface_has_only_fixed_cua_verbs() {
        let lowered = authorized(AppMacosHostAction::TypeText {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:1".to_owned(),
            element_token: "s00000006:3".to_owned(),
            text: "bounded text".to_owned(),
        })
        .lower()
        .expect("lower");
        assert_eq!(lowered.verb(), "type_text");
        let args = lowered_args(&lowered);
        let object = args.as_object().expect("object");
        assert_eq!(object.get("pid").and_then(Value::as_u64), Some(42));
        assert!(!object.contains_key("element_index"));
        assert!(!object.contains_key("action_name"));
        assert!(!object.contains_key("args_json"));
        assert!(!object.contains_key("script"));
        assert!(!object.contains_key("path"));
    }

    #[test]
    fn element_actions_rebind_to_the_fenced_fresh_snapshot_only() {
        let mut lowered = authorized(AppMacosHostAction::ClickElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:1".to_owned(),
            element_token: "s00000006:3".to_owned(),
            click_count: 1,
        })
        .lower()
        .expect("lower");
        assert!(lowered.requires_fresh_element_binding());
        // The fence covers the target element only.
        assert_eq!(lowered.observation_fence_indexes(), vec![3]);
        lowered
            .bind_fresh_observation(&fresh_window_state())
            .expect("rebind");
        assert_eq!(lowered.observation_fence_indexes(), vec![3]);
        assert!(!lowered.requires_fresh_element_binding());
        assert_eq!(
            lowered_args(&lowered),
            json!({"pid": 42, "window_id": 9, "element_token": "s00000007:3"})
        );
        // A binding is single-use; a second fence reply cannot re-aim it.
        assert!(lowered
            .bind_fresh_observation(&fresh_window_state())
            .is_err());

        let mut missing = authorized(AppMacosHostAction::ScrollElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:1".to_owned(),
            element_token: "s00000006:4".to_owned(),
            direction: magician_app_contract::macos_host::AppMacosHostScrollDirection::Down,
            amount: 3,
        })
        .lower()
        .expect("lower scroll");
        assert_eq!(
            lowered_args(&missing),
            json!({"pid": 42, "window_id": 9, "element_token": "s00000006:4",
                   "direction": "down", "amount": 3})
        );
        assert!(missing
            .bind_fresh_observation(&fresh_window_state())
            .is_err());
        assert!(missing.requires_fresh_element_binding());
    }

    #[test]
    fn menu_items_are_not_element_action_targets() {
        // Menu rows have no element fence, so a menu item's index is not
        // proven stable between the observation and the action.
        let mut fresh = fresh_window_state();
        fresh["elements"]
            .as_array_mut()
            .expect("elements")
            .push(json!({
                "element_index": 40, "element_token": "s00000007:40", "role": "AXMenuItem",
                "label": "Undo", "depth": 4, "frame": {"x": 600.0, "y": 140.0, "w": 80.0, "h": 20.0}
            }));
        let mut lowered = authorized(AppMacosHostAction::ClickElement {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:1".to_owned(),
            element_token: "s00000006:40".to_owned(),
            click_count: 1,
        })
        .lower()
        .expect("lower");
        assert!(lowered.bind_fresh_observation(&fresh).is_err());
        assert!(lowered.requires_fresh_element_binding());
    }

    #[test]
    fn drag_places_window_local_screenshot_pixels_from_fresh_frames() {
        let drag = |destination: &str| {
            authorized(AppMacosHostAction::DragElements {
                bundle_id: "com.example.Editor".to_owned(),
                process_id: 42,
                window_id: 9,
                observation_ref: "interactive-observation:1".to_owned(),
                source_element_token: "s00000006:3".to_owned(),
                destination_element_token: destination.to_owned(),
                screenshot_scale_millis: 2_000,
            })
            .lower()
            .expect("lower drag")
        };
        let mut lowered = drag("s00000006:7");
        assert_eq!(lowered.verb(), "drag");
        // Both endpoints are fenced, source first.
        assert_eq!(lowered.observation_fence_indexes(), vec![3, 7]);
        lowered
            .bind_fresh_observation(&fresh_window_state())
            .expect("bind drag");
        assert_eq!(
            lowered_args(&lowered),
            json!({"pid": 42, "window_id": 9,
                   "from_x": 232.0, "from_y": 242.0, "to_x": 155.0, "to_y": 442.0})
        );
        // Virtualized off-viewport rows (`h:1`) are never drag targets.
        let mut virtualized = drag("s00000006:9");
        assert!(virtualized
            .bind_fresh_observation(&fresh_window_state())
            .is_err());
        assert!(virtualized.requires_fresh_element_binding());
    }

    #[test]
    fn focus_and_keys_lower_to_cua_0_28_names() {
        let focus = authorized(AppMacosHostAction::Focus {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
        })
        .lower()
        .expect("lower focus");
        assert_eq!(focus.verb(), "bring_to_front");
        assert_eq!(lowered_args(&focus), json!({"pid": 42}));
        assert!(!focus.requires_fresh_element_binding());

        let key = authorized(AppMacosHostAction::PressKey {
            bundle_id: "com.example.Editor".to_owned(),
            process_id: 42,
            window_id: 9,
            observation_ref: "interactive-observation:1".to_owned(),
            key: magician_app_contract::macos_host::AppMacosHostKey::DeleteForward,
            modifiers: vec![magician_app_contract::macos_host::AppMacosHostModifier::Shift],
        })
        .lower()
        .expect("lower key");
        assert_eq!(
            lowered_args(&key),
            json!({"pid": 42, "window_id": 9, "key": "delete", "modifiers": ["shift", "fn"]})
        );
        // A key names no element: its fence is the window's.
        assert!(key.observation_fence_indexes().is_empty());
        assert!(!key.requires_fresh_element_binding());
    }

    #[test]
    fn filesystem_backed_pixel_capture_has_no_typed_lowering() {
        let action = AuthorizedAppMacosHostAction {
            action: AppMacosHostAction::CapturePixels {
                bundle_id: "com.example.Editor".to_owned(),
                process_id: 42,
                window_id: 9,
            },
            correlation_ref: "correlation:2".to_owned(),
            effect_binding_digest: format!("blake3:{}", "02".repeat(32)),
            result_byte_ceiling: 4_096,
            evidence_byte_ceiling: 4_096,
            application_identity_digest: format!("blake3:{}", "07".repeat(32)),
            tcc_policy_digest: format!("blake3:{}", "09".repeat(32)),
            tcc_epoch: 2,
            observation_content_digest: None,
            observation_revalidation_byte_ceiling: None,
            expires_at_ms: 10_000,
            response_byte_ceiling: 12_288,
            stop_signal: AppMacosHostStopSignal::new(),
            cua_driver_binary: PathBuf::from(
                "/Applications/CuaDriver.app/Contents/MacOS/cua-driver",
            ),
            cua_driver_binary_digest: format!("blake3:{}", "04".repeat(32)),
        };
        assert_eq!(
            action.lower().unwrap_err(),
            AppMacosHostBoundaryError::InvalidAction
        );
    }

    #[tokio::test]
    async fn owner_stop_before_wait_is_not_lost() {
        let signal = AppMacosHostStopSignal::new();
        signal.stop();
        signal.stopped().await;
        assert!(signal.is_stopped());
    }
}
