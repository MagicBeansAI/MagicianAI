//! App-safe adapter for the existing `BrowserDispatcher` physical owner.
//!
//! The normal browser skill intentionally mirrors raw agent-browser argv. Apps
//! must not receive that surface. This adapter admits a small closed action set
//! and installation/run-owned isolated sessions, binds every action to the
//! common interactive and effect typestates, and returns bounded evidence with
//! opaque element identities. App workflows expose only the reviewed
//! Observe/snapshot vertical; every other browser action remains denied.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;
use uuid::Uuid;

use super::{
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
    execution::{
        actions::ActionResult,
        primitive_dispatch::{
            browser::{AgentBrowserSession, BrowserDispatcher, ConnectionMode},
            runner::PrimitiveDispatcher,
        },
    },
    json_traversal::canonical_json_bytes,
};

pub(crate) const APP_BROWSER_PROFILE_V1: &str = "magician.app-browser-session.v1";
pub(crate) const APP_BROWSER_IMPLEMENTATION_REVISION: &str =
    "magician.app-browser-implementation.2026-08-22.1";
#[allow(dead_code)]
const APP_BROWSER_OBSERVE_ACTION_REF: &str = "interactive:browser:observe";
#[allow(dead_code)]
const APP_BROWSER_NAVIGATE_ACTION_REF: &str = "interactive:browser:navigate";
#[allow(dead_code)]
const APP_BROWSER_SCROLL_ACTION_REF: &str = "interactive:browser:scroll";
#[allow(dead_code)]
const APP_BROWSER_CLICK_ACTION_REF: &str = "interactive:browser:click";
pub const APP_BROWSER_OBSERVE_RESULT_CEILING: u64 = 512 * 1024;
const APP_BROWSER_OBSERVE_CONTENT_CEILING: usize = 384 * 1024;
pub const APP_BROWSER_ACTION_RESULT_CEILING: u64 = 32 * 1024;
const APP_BROWSER_STATE_EVIDENCE_CEILING: u64 = 64 * 1024;
const MAX_APP_BROWSER_ORIGINS: usize = 64;
const MAX_APP_BROWSER_TABS: u16 = 16;
const MAX_APP_BROWSER_URL_BYTES: usize = 8 * 1024;
const MAX_APP_BROWSER_ELEMENTS: usize = 1024;
const MAX_APP_BROWSER_SCROLL_PIXELS: u32 = 16_384;
const APP_BROWSER_OBSERVATION_TTL_SECONDS: i64 = 30;
const APP_BROWSER_MAX_CLI_BYTES: u64 = 64 * 1024 * 1024;
const APP_BROWSER_FILE_HASH_SLOTS: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppBrowserProfileClass {
    InstallationEphemeralHeadless,
    InstallationEphemeralHeaded,
}

impl AppBrowserProfileClass {
    fn connection_mode(self) -> ConnectionMode {
        match self {
            Self::InstallationEphemeralHeadless => ConnectionMode::Headless,
            Self::InstallationEphemeralHeaded => ConnectionMode::Headed,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AppBrowserOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl AppBrowserOrigin {
    pub(crate) fn parse(value: &str) -> Result<Self, AppBrowserError> {
        let parsed = Url::parse(value).map_err(|_| AppBrowserError::InvalidOrigin)?;
        if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(AppBrowserError::InvalidOrigin);
        }
        Self::from_url(&parsed)
    }

    fn from_url(parsed: &Url) -> Result<Self, AppBrowserError> {
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(AppBrowserError::InvalidOrigin);
        }
        let host = parsed
            .host_str()
            .map(str::to_ascii_lowercase)
            .ok_or(AppBrowserError::InvalidOrigin)?;
        let port = parsed
            .port_or_known_default()
            .ok_or(AppBrowserError::InvalidOrigin)?;
        Ok(Self {
            scheme: parsed.scheme().to_owned(),
            host,
            port,
        })
    }

    fn label(&self) -> String {
        let default_port = (self.scheme == "https" && self.port == 443)
            || (self.scheme == "http" && self.port == 80);
        if default_port {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

/// Reviewed top-level browser target policy. Subresource egress continues to
/// belong to the browser owner; this policy additionally fences every app
/// navigation, redirect and popup/new-tab top-level origin.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBrowserTargetPolicy {
    allowed_origins: BTreeSet<AppBrowserOrigin>,
    max_owned_tabs: u16,
    digest: AppDigest,
}

impl AppBrowserTargetPolicy {
    /// Closed policy for the first read-only vertical. A fresh isolated owner
    /// may expose only `about:blank`; any HTTP(S) state is denied.
    pub(crate) fn observe_only() -> Result<Self, AppBrowserError> {
        let mut value = Self {
            allowed_origins: BTreeSet::new(),
            max_owned_tabs: 1,
            digest: AppDigest::blake3(b"pending-browser-policy"),
        };
        value.digest = AppDigest::blake3_canonical_json(&json!({
            "schema": APP_BROWSER_PROFILE_V1,
            "mode": "observe_only_about_blank",
            "allowed_origins": &value.allowed_origins,
            "max_owned_tabs": value.max_owned_tabs,
        }))
        .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        Ok(value)
    }

    pub(crate) fn reviewed(
        allowed_origins: BTreeSet<AppBrowserOrigin>,
        max_owned_tabs: u16,
    ) -> Result<Self, AppBrowserError> {
        if allowed_origins.is_empty()
            || allowed_origins.len() > MAX_APP_BROWSER_ORIGINS
            || max_owned_tabs == 0
            || max_owned_tabs > MAX_APP_BROWSER_TABS
        {
            return Err(AppBrowserError::InvalidTargetPolicy);
        }
        let mut value = Self {
            allowed_origins,
            max_owned_tabs,
            digest: AppDigest::blake3(b"pending-browser-policy"),
        };
        value.digest = AppDigest::blake3_canonical_json(&json!({
            "schema": APP_BROWSER_PROFILE_V1,
            "allowed_origins": &value.allowed_origins,
            "max_owned_tabs": value.max_owned_tabs,
        }))
        .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        Ok(value)
    }

    pub fn digest(&self) -> &AppDigest {
        &self.digest
    }

    pub fn allowed_origins(&self) -> &BTreeSet<AppBrowserOrigin> {
        &self.allowed_origins
    }

    fn permits_url(&self, raw: &str) -> Result<AppBrowserOrigin, AppBrowserError> {
        if raw.len() > MAX_APP_BROWSER_URL_BYTES {
            return Err(AppBrowserError::TargetDenied);
        }
        let parsed = Url::parse(raw).map_err(|_| AppBrowserError::TargetDenied)?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(AppBrowserError::TargetDenied);
        }
        let origin = AppBrowserOrigin::from_url(&parsed)?;
        self.allowed_origins
            .contains(&origin)
            .then_some(origin)
            .ok_or(AppBrowserError::TargetDenied)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppBrowserScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl AppBrowserScrollDirection {
    fn cli_label(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// Closed app input. There is intentionally no argv, eval, selector, CDP,
/// profile path, cookie/storage, upload/download path or secret field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppBrowserAction {
    Observe,
    Navigate {
        url: String,
    },
    Scroll {
        observation_ref: AppReference,
        direction: AppBrowserScrollDirection,
        pixels: u32,
    },
    Click {
        observation_ref: AppReference,
        element_ref: AppReference,
    },
}

#[allow(dead_code)] // Stable action-ref projection is retained for package qualification.
impl AppBrowserAction {
    fn action_ref(&self) -> Result<AppReference, AppBrowserError> {
        let value = match self {
            Self::Observe => APP_BROWSER_OBSERVE_ACTION_REF,
            Self::Navigate { .. } => APP_BROWSER_NAVIGATE_ACTION_REF,
            Self::Scroll { .. } => APP_BROWSER_SCROLL_ACTION_REF,
            Self::Click { .. } => APP_BROWSER_CLICK_ACTION_REF,
        };
        AppReference::parse(value)
            .map_err(|_| AppBrowserError::Encoding("invalid static action ref".to_owned()))
    }

    fn command(
        &self,
        policy: &AppBrowserTargetPolicy,
        physical_element: Option<&str>,
    ) -> Result<(&'static str, Value), AppBrowserError> {
        match self {
            Self::Observe => Ok(("snapshot", json!({"args": ["-i"]}))),
            Self::Navigate { url } => {
                policy.permits_url(url)?;
                Ok(("open", json!({"args": [url]})))
            },
            Self::Scroll {
                direction, pixels, ..
            } => {
                if *pixels == 0 || *pixels > MAX_APP_BROWSER_SCROLL_PIXELS {
                    return Err(AppBrowserError::InvalidInput);
                }
                Ok((
                    "scroll",
                    json!({"args": [direction.cli_label(), pixels.to_string()]}),
                ))
            },
            Self::Click { .. } => Ok((
                "click",
                json!({"args": [physical_element.ok_or(AppBrowserError::ObservationMismatch)?]}),
            )),
        }
    }

    fn requires_observation(&self) -> bool {
        matches!(self, Self::Scroll { .. } | Self::Click { .. })
    }

    fn resource_claim(&self) -> AppInteractiveResourceClaim {
        match self {
            Self::Observe => AppInteractiveResourceClaim {
                evidence_bytes: APP_BROWSER_OBSERVE_RESULT_CEILING
                    + APP_BROWSER_STATE_EVIDENCE_CEILING,
                evidence_nodes: MAX_APP_BROWSER_ELEMENTS as u64,
                pixels: 0,
                artifact_bytes: 0,
                output_bytes: APP_BROWSER_OBSERVE_RESULT_CEILING,
            },
            Self::Navigate { .. } | Self::Scroll { .. } | Self::Click { .. } => {
                AppInteractiveResourceClaim {
                    evidence_bytes: APP_BROWSER_STATE_EVIDENCE_CEILING,
                    evidence_nodes: MAX_APP_BROWSER_TABS as u64,
                    pixels: 0,
                    artifact_bytes: 0,
                    output_bytes: APP_BROWSER_ACTION_RESULT_CEILING,
                }
            },
        }
    }
}

pub fn app_browser_runtime_implementation_digest() -> AppDigest {
    static DIGEST: OnceLock<AppDigest> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let mut hasher = blake3::Hasher::new();
            hash_browser_implementation_component(
                &mut hasher,
                "revision",
                APP_BROWSER_IMPLEMENTATION_REVISION.as_bytes(),
            );
            hash_browser_implementation_component(
                &mut hasher,
                "apps/browser_capability.rs",
                include_bytes!("browser_capability.rs"),
            );
            hash_browser_implementation_component(
                &mut hasher,
                "execution/primitive_dispatch/browser/dispatch.rs",
                include_bytes!("../execution/primitive_dispatch/browser/dispatch.rs"),
            );
            hash_browser_implementation_component(
                &mut hasher,
                "execution/primitive_dispatch/browser/session.rs",
                include_bytes!("../execution/primitive_dispatch/browser/session.rs"),
            );
            hash_browser_implementation_component(
                &mut hasher,
                "execution/primitive_dispatch/browser/owned_tabs_header.rs",
                include_bytes!("../execution/primitive_dispatch/browser/owned_tabs_header.rs"),
            );
            AppDigest::blake3(hasher.finalize().as_bytes())
        })
        .clone()
}

fn hash_browser_implementation_component(hasher: &mut blake3::Hasher, name: &str, bytes: &[u8]) {
    hasher.update(b"magician.app-browser.implementation-component.v1\0");
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(crate) fn app_browser_profile_digest(
    profile: AppBrowserProfileClass,
) -> Result<AppDigest, AppBrowserError> {
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_BROWSER_PROFILE_V1,
        "profile": profile,
        "persistent_profile": false,
        "owner_cdp": false,
        "caller_profile_path": false,
    }))
    .map_err(|error| AppBrowserError::Encoding(error.to_string()))
}

/// Safe actions used by install review. These digests must be projected into
/// the package lock before the activation owner can mark them dispatchable.
#[allow(dead_code)] // Full reviewed roster is consumed by authoring/qualification fixtures.
pub(crate) fn reviewed_browser_actions(
) -> Result<Vec<AppInteractiveReviewedAction>, AppBrowserError> {
    Ok(vec![
        reviewed_browser_action(
            AppReference::parse(APP_BROWSER_OBSERVE_ACTION_REF)
                .map_err(|_| AppBrowserError::Encoding("invalid static action ref".to_owned()))?,
            &AppBrowserAction::Observe,
        )?,
        reviewed_browser_action(
            AppReference::parse(APP_BROWSER_NAVIGATE_ACTION_REF)
                .map_err(|_| AppBrowserError::Encoding("invalid static action ref".to_owned()))?,
            &AppBrowserAction::Navigate {
                url: "https://example.invalid/".to_owned(),
            },
        )?,
        reviewed_browser_action(
            AppReference::parse(APP_BROWSER_SCROLL_ACTION_REF)
                .map_err(|_| AppBrowserError::Encoding("invalid static action ref".to_owned()))?,
            &AppBrowserAction::Scroll {
                observation_ref: AppReference::parse("browser-observation:review").map_err(
                    |_| AppBrowserError::Encoding("invalid static observation ref".to_owned()),
                )?,
                direction: AppBrowserScrollDirection::Down,
                pixels: 1,
            },
        )?,
        reviewed_browser_action(
            AppReference::parse(APP_BROWSER_CLICK_ACTION_REF)
                .map_err(|_| AppBrowserError::Encoding("invalid static action ref".to_owned()))?,
            &AppBrowserAction::Click {
                observation_ref: AppReference::parse("browser-observation:review").map_err(
                    |_| AppBrowserError::Encoding("invalid static observation ref".to_owned()),
                )?,
                element_ref: AppReference::parse("browser-element:review").map_err(|_| {
                    AppBrowserError::Encoding("invalid static element ref".to_owned())
                })?,
            },
        )?,
    ])
}

pub(crate) fn browser_observe_input_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,"required":["action"],
        "properties":{"action":{"const":"observe"}}
    })
}

pub fn browser_observe_result_schema() -> Value {
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["kind","success","origin","content_digest","content","elements","observation_ref"],
        "properties":{
            "kind":{"const":"app_browser_interactive"},
            "success":{"type":"boolean"},
            "origin":{"type":["string","null"],"maxLength":512},
            "content_digest":{"type":["string","null"],"maxLength":80},
            "content":{"type":["string","null"],"maxLength":393216},
            "elements":{"type":"array","maxItems":1024,"items":{"type":"string","maxLength":96}},
            "observation_ref":{"type":["string","null"],"maxLength":192}
        }
    })
}

#[allow(dead_code)] // Snapshot-only package compatibility.
pub(crate) fn browser_observe_resource_ceilings(
    max_duration_seconds: u64,
) -> Result<AppInteractiveResourceCeilings, AppBrowserError> {
    AppInteractiveResourceCeilings::reviewed(
        1,
        1,
        max_duration_seconds.min(300),
        APP_BROWSER_OBSERVE_RESULT_CEILING + APP_BROWSER_STATE_EVIDENCE_CEILING,
        MAX_APP_BROWSER_ELEMENTS as u64,
        1280 * 800,
        0,
        APP_BROWSER_OBSERVE_RESULT_CEILING,
    )
    .map_err(Into::into)
}

pub fn browser_action_result_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "required":["kind","success","origin","content_digest","content","elements"],
        "properties":{
            "kind":{"const":"app_browser_interactive"},
            "success":{"type":"boolean"},
            "origin":{"type":["string","null"],"maxLength":512},
            "content_digest":{"type":["string","null"],"maxLength":80},
            "content":{"type":["string","null"],"maxLength":524288},
            "elements":{"type":"array","maxItems":1024,"items":{"type":"string","maxLength":96}}
        }
    })
}

pub fn browser_action_input_schema(action_name: &str) -> Option<Value> {
    match action_name {
        "snapshot" => Some(browser_observe_input_schema()),
        "navigate" => Some(json!({
            "type":"object","additionalProperties":false,"required":["action","url"],
            "properties":{
                "action":{"const":"navigate"},
                "url":{"type":"string","format":"uri","maxLength":8192}
            }
        })),
        "scroll" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","direction","pixels"],
            "properties":{
                "action":{"const":"scroll"},
                "observation_ref":{"type":"string","maxLength":192},
                "direction":{"enum":["up","down","left","right"]},
                "pixels":{"type":"integer","minimum":1,"maximum":16384}
            }
        })),
        "click" => Some(json!({
            "type":"object","additionalProperties":false,
            "required":["action","observation_ref","element_ref"],
            "properties":{
                "action":{"const":"click"},
                "observation_ref":{"type":"string","maxLength":192},
                "element_ref":{"type":"string","maxLength":192}
            }
        })),
        _ => None,
    }
}

pub(crate) fn reviewed_browser_action(
    action_ref: AppReference,
    action: &AppBrowserAction,
) -> Result<AppInteractiveReviewedAction, AppBrowserError> {
    let (class, input_schema, result_schema, ceiling, required_kind, invalidates) = match action {
        AppBrowserAction::Observe => (
            AppInteractiveActionClass::Observe,
            browser_observe_input_schema(),
            browser_observe_result_schema(),
            APP_BROWSER_OBSERVE_RESULT_CEILING,
            None,
            false,
        ),
        AppBrowserAction::Navigate { .. } => (
            AppInteractiveActionClass::NavigateOrLaunch,
            browser_action_input_schema("navigate").expect("static browser action schema"),
            browser_action_result_schema(),
            APP_BROWSER_ACTION_RESULT_CEILING,
            None,
            true,
        ),
        AppBrowserAction::Scroll { .. } => (
            AppInteractiveActionClass::Interact,
            browser_action_input_schema("scroll").expect("static browser action schema"),
            browser_action_result_schema(),
            APP_BROWSER_ACTION_RESULT_CEILING,
            Some(AppInteractiveObservationKind::StructuredTree),
            true,
        ),
        AppBrowserAction::Click { .. } => (
            AppInteractiveActionClass::OutwardCommit,
            browser_action_input_schema("click").expect("static browser action schema"),
            browser_action_result_schema(),
            APP_BROWSER_ACTION_RESULT_CEILING,
            Some(AppInteractiveObservationKind::StructuredTree),
            true,
        ),
    };
    AppInteractiveReviewedAction::reviewed(
        action_ref,
        class,
        AppDigest::blake3_canonical_json(&input_schema)
            .map_err(|error| AppBrowserError::Encoding(error.to_string()))?,
        AppDigest::blake3_canonical_json(&result_schema)
            .map_err(|error| AppBrowserError::Encoding(error.to_string()))?,
        app_browser_runtime_implementation_digest(),
        ceiling,
        required_kind,
        invalidates,
    )
    .map_err(Into::into)
}

/// Installation/run-owned isolated physical browser plus the common session
/// handle. Neither the CLI path nor the underlying browser session ID escapes.
pub(crate) struct AppBrowserSessionOwner {
    session: Arc<AgentBrowserSession>,
    dispatcher: BrowserDispatcher,
    interactive: AppInteractiveSessionHandle,
    grant: AppInteractiveGrantDescriptor,
    policy: AppBrowserTargetPolicy,
    profile: AppBrowserProfileClass,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    cli_path: PathBuf,
    cli_identity_digest: AppDigest,
    closed: Arc<AtomicBool>,
}

/// Browser state retained under the common live-interactive registry. The
/// physical session and opaque selector map never enter durable workflow
/// state; stop/revoke still acts through the shared cancellation identity.
pub(crate) struct AppBrowserLiveSession {
    owner: AppBrowserSessionOwner,
    observation: Option<AppBrowserObservation>,
}

impl AppBrowserLiveSession {
    pub(crate) fn new(owner: AppBrowserSessionOwner) -> Self {
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
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        now: DateTime<Utc>,
    ) -> Result<(), AppBrowserError> {
        if grant.profile() != AppInteractiveExecutionProfile::BrowserSession
            || grant.target_policy_digest() != self.owner.policy.digest()
            || grant.owner_profile_digest() != &app_browser_profile_digest(self.owner.profile)?
            || grant.owner_implementation_digest() != &app_browser_runtime_implementation_digest()
        {
            return Err(AppBrowserError::ReviewMismatch);
        }
        self.owner
            .interactive
            .rebind_exact_leaf(&grant, current, now)?;
        self.owner.grant = grant;
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
                self.observation = Some(AppBrowserObservation {
                    public_ref,
                    interactive,
                    owner_target_digest: previous.owner_target_digest,
                    element_selectors: previous.element_selectors,
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_action(
        session: Arc<tokio::sync::Mutex<Self>>,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        action: AppBrowserAction,
        now: DateTime<Utc>,
    ) -> Result<AppBrowserPreparedAction, AppBrowserError> {
        let mut live = session.lock().await;
        let observation = if action.requires_observation() {
            live.observation.take()
        } else {
            None
        };
        let mut prepared = live
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
        prepared.live_session = Some(Arc::clone(&session));
        prepared.current = Some(current.clone());
        Ok(prepared)
    }
}

#[allow(dead_code)] // Convenience lifecycle methods remain part of the sealed physical owner API.
impl AppBrowserSessionOwner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn acquire_isolated(
        storage_root: &Path,
        principal: &str,
        workspace: &str,
        grant: AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        policy: AppBrowserTargetPolicy,
        profile: AppBrowserProfileClass,
        resource_lease_ref: AppReference,
        session_ordinal: u16,
        cancellation: AppInteractiveCancellation,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppBrowserError> {
        let implementation_digest = app_browser_runtime_implementation_digest();
        let profile_digest = app_browser_profile_digest(profile)?;
        if grant.profile() != AppInteractiveExecutionProfile::BrowserSession
            || grant.background() != AppInteractiveBackgroundPosture::DirectOwner
            || grant.target_policy_digest() != policy.digest()
            || grant.owner_profile_digest() != &profile_digest
            || grant.owner_implementation_digest() != &implementation_digest
            || grant.installation_generation() == 0
        {
            return Err(AppBrowserError::ReviewMismatch);
        }
        let scope_digest = AppDigest::blake3_canonical_json(&json!({
            "principal": principal,
            "workspace": workspace,
        }))
        .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        let (cli_path, cli_identity_digest) = resolve_browser_cli_identity(
            storage_root.to_path_buf(),
            principal.to_owned(),
            workspace.to_owned(),
        )
        .await?;
        let session_id = app_browser_session_id(
            &grant,
            current.run_ref(),
            &scope_digest,
            &resource_lease_ref,
            session_ordinal,
            profile,
            &cli_identity_digest,
        )?;
        let session = Arc::new(
            AgentBrowserSession::new_with_session_id(
                session_id.clone(),
                profile.connection_mode(),
                cli_path.clone(),
            )
            .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?
            .with_capture_limit_bytes(APP_BROWSER_OBSERVE_CONTENT_CEILING),
        );
        let owner_target_digest = AppDigest::blake3_canonical_json(&json!({
            "schema": APP_BROWSER_PROFILE_V1,
            "implementation_digest": implementation_digest,
            "profile_digest": profile_digest,
            "target_policy_digest": policy.digest(),
            "session_id_digest": AppDigest::blake3(session_id.as_bytes()),
            "cli_identity_digest": cli_identity_digest,
            "installation_id": grant.installation_id(),
            "installation_generation": grant.installation_generation(),
            "run_ref": current.run_ref(),
            "scope_digest": scope_digest,
            "resource_lease_ref": resource_lease_ref,
            "session_ordinal": session_ordinal,
        }))
        .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        let owner_target_ref = AppReference::parse(format!(
            "browser-session:{}",
            digest_hex(&owner_target_digest)?
        ))
        .map_err(|_| AppBrowserError::Encoding("invalid browser owner target".to_owned()))?;
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
        let dispatcher = BrowserDispatcher::with_options(
            Arc::clone(&session),
            None,
            grant.resources().max_duration_seconds().min(300),
        );
        Ok(Self {
            session,
            dispatcher,
            interactive,
            grant,
            policy,
            profile,
            owner_target_ref,
            owner_target_digest,
            cli_path,
            cli_identity_digest,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) async fn prepare_action(
        &mut self,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        action: AppBrowserAction,
        observation: Option<AppBrowserObservation>,
        now: DateTime<Utc>,
    ) -> Result<AppBrowserPreparedAction, AppBrowserError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AppBrowserError::PhysicalOwnerUnavailable);
        }
        if hash_browser_cli_identity(self.cli_path.clone()).await? != self.cli_identity_digest {
            return Err(AppBrowserError::ReviewMismatch);
        }
        let action_ref = locked_action.action_ref().clone();
        let reviewed = self
            .grant
            .actions()
            .get(&action_ref)
            .ok_or(AppBrowserError::ActionNotReviewed)?;
        let expected_review = reviewed_browser_action(action_ref.clone(), &action)?;
        if reviewed != &expected_review
            || reviewed.requires_observation() != action.requires_observation()
        {
            return Err(AppBrowserError::ReviewMismatch);
        }
        validate_locked_browser_action(primitive, locked_action, reviewed, &source_digest)?;
        // Validate the entire typed physical lowering before the common
        // interactive handle charges budget or advances session generation.
        let canonical_value = serde_json::to_value(&action)
            .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        let canonical_input = canonical_json_bytes(&canonical_value)
            .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
        let mut physical_element = None;
        let interactive_observation = match observation {
            Some(observation) => {
                if observation.owner_target_digest != self.owner_target_digest {
                    return Err(AppBrowserError::ObservationMismatch);
                }
                let requested_observation = match &action {
                    AppBrowserAction::Scroll {
                        observation_ref, ..
                    }
                    | AppBrowserAction::Click {
                        observation_ref, ..
                    } => Some(observation_ref),
                    _ => None,
                };
                if requested_observation != Some(&observation.public_ref) {
                    return Err(AppBrowserError::ObservationMismatch);
                }
                if let AppBrowserAction::Click { element_ref, .. } = &action {
                    physical_element = Some(
                        observation
                            .element_selectors
                            .get(element_ref)
                            .cloned()
                            .ok_or(AppBrowserError::ObservationMismatch)?,
                    );
                }
                Some(observation.interactive)
            },
            None => None,
        };
        let (command, arguments) = action.command(&self.policy, physical_element.as_deref())?;
        let permit = self.interactive.authorize_action(
            &self.grant,
            current,
            &action_ref,
            &canonical_input,
            action.resource_claim(),
            interactive_observation,
            now,
        )?;
        Ok(AppBrowserPreparedAction {
            dispatcher: self.dispatcher.clone(),
            policy: self.policy.clone(),
            profile: self.profile,
            source_digest,
            primitive_ref: primitive.primitive_ref().clone(),
            reviewed_action: reviewed.clone(),
            command,
            arguments,
            action,
            canonical_input,
            owner_target_ref: self.owner_target_ref.clone(),
            owner_target_digest: self.owner_target_digest.clone(),
            cli_path: self.cli_path.clone(),
            cli_identity_digest: self.cli_identity_digest.clone(),
            interactive_permit: permit,
            session_owner: None,
            live_session: None,
            current: None,
        })
    }

    /// First vertical convenience: one prepared action retains the complete
    /// session owner through physical I/O and drops it immediately afterward.
    /// Multi-step owners must use an explicit run-owned session registry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_single_action(
        mut self,
        current: &AppInteractiveCurrentFence,
        primitive: &AppLockedPrimitiveBinding,
        locked_action: &AppLockedPrimitiveActionBinding,
        source_digest: AppDigest,
        action: AppBrowserAction,
        now: DateTime<Utc>,
    ) -> Result<AppBrowserPreparedAction, AppBrowserError> {
        let mut prepared = self
            .prepare_action(
                current,
                primitive,
                locked_action,
                source_digest,
                action,
                None,
                now,
            )
            .await?;
        prepared.session_owner = Some(Box::new(self));
        Ok(prepared)
    }

    pub(crate) fn materialize_observation(
        &mut self,
        current: &AppInteractiveCurrentFence,
        evidence: AppBrowserObservationEvidence,
        labels_digest: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppBrowserObservation, AppBrowserError> {
        if evidence.owner_target_digest != self.owner_target_digest {
            return Err(AppBrowserError::ObservationMismatch);
        }
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(
                APP_BROWSER_OBSERVATION_TTL_SECONDS,
            ))
            .ok_or(AppBrowserError::ObservationMismatch)?
            .min(self.grant.expires_at());
        let interactive = self.interactive.issue_observation(
            &self.grant,
            current,
            AppInteractiveObservationKind::StructuredTree,
            evidence.geometry,
            evidence.content_digest,
            labels_digest,
            evidence.evidence_bytes,
            evidence.evidence_nodes,
            now,
            expires_at,
        )?;
        Ok(AppBrowserObservation {
            public_ref: interactive.observation_ref().clone(),
            interactive,
            owner_target_digest: self.owner_target_digest.clone(),
            element_selectors: evidence.element_selectors,
        })
    }

    pub(crate) fn stop(&self, reason: AppInteractiveCancellationReason) {
        self.interactive.cancellation().cancel(reason);
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppBrowserError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        self.interactive
            .cancellation()
            .cancel(AppInteractiveCancellationReason::OwnerStop);
        self.session
            .shutdown()
            .await
            .map_err(|_| AppBrowserError::CleanupFailed)?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for AppBrowserSessionOwner {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.interactive
            .cancellation()
            .cancel(AppInteractiveCancellationReason::RunCancelled);
        let session = Arc::clone(&self.session);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = session.shutdown().await;
            });
        }
    }
}

pub(crate) struct AppBrowserObservation {
    public_ref: AppReference,
    interactive: AppInteractiveObservationRef,
    owner_target_digest: AppDigest,
    element_selectors: BTreeMap<AppReference, String>,
}

#[allow(dead_code)] // Opaque selector inspection is retained for qualification tests.
impl AppBrowserObservation {
    pub(crate) fn element_refs(&self) -> impl Iterator<Item = &AppReference> {
        self.element_selectors.keys()
    }
}

pub(crate) struct AppBrowserPreparedAction {
    dispatcher: BrowserDispatcher,
    policy: AppBrowserTargetPolicy,
    #[allow(dead_code)] // Retained as audited physical-route evidence.
    profile: AppBrowserProfileClass,
    source_digest: AppDigest,
    primitive_ref: AppReference,
    reviewed_action: AppInteractiveReviewedAction,
    command: &'static str,
    arguments: Value,
    action: AppBrowserAction,
    canonical_input: Vec<u8>,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    cli_path: PathBuf,
    cli_identity_digest: AppDigest,
    interactive_permit: AppInteractiveActionPermit,
    session_owner: Option<Box<AppBrowserSessionOwner>>,
    live_session: Option<Arc<tokio::sync::Mutex<AppBrowserLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

#[allow(dead_code)] // Canonical input inspection is retained for qualification.
impl AppBrowserPreparedAction {
    pub(crate) fn canonical_input(&self) -> &[u8] {
        &self.canonical_input
    }

    pub(crate) fn bind_effect(
        self,
        binding: &AppEffectBinding,
        now: DateTime<Utc>,
    ) -> Result<AppBrowserEffectAction, AppBrowserError> {
        let interactive_effect =
            self.interactive_permit
                .bind_effect(binding, &self.canonical_input, now)?;
        Ok(AppBrowserEffectAction {
            dispatcher: self.dispatcher,
            policy: self.policy,
            command: self.command,
            arguments: self.arguments,
            action: self.action,
            canonical_input: self.canonical_input,
            owner_target_digest: self.owner_target_digest,
            cli_path: self.cli_path,
            cli_identity_digest: self.cli_identity_digest,
            interactive_effect,
            _session_owner: self.session_owner,
            live_session: self.live_session,
            current: self.current,
        })
    }
}

impl AppEffectPhysicalOwner for AppBrowserPreparedAction {
    fn attest_effect_target(
        &self,
        tool_ref: &AppReference,
        primitive: &AppLockedPrimitiveBinding,
        action: &AppLockedPrimitiveActionBinding,
    ) -> Result<AppEffectPhysicalTarget, AppEffectKernelError> {
        let exact_alias = format!("capability:browser__{}", action.name());
        if !matches!(tool_ref.as_str(), "capability:browser") && tool_ref.as_str() != exact_alias
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

pub(crate) struct AppBrowserEffectAction {
    dispatcher: BrowserDispatcher,
    policy: AppBrowserTargetPolicy,
    command: &'static str,
    arguments: Value,
    action: AppBrowserAction,
    #[allow(dead_code)] // Retained as sealed dispatch evidence.
    canonical_input: Vec<u8>,
    owner_target_digest: AppDigest,
    cli_path: PathBuf,
    cli_identity_digest: AppDigest,
    interactive_effect: AppInteractiveEffectPermit,
    _session_owner: Option<Box<AppBrowserSessionOwner>>,
    live_session: Option<Arc<tokio::sync::Mutex<AppBrowserLiveSession>>>,
    current: Option<AppInteractiveCurrentFence>,
}

impl AppBrowserEffectAction {
    pub(crate) fn interactive_inspection(&self) -> AppInteractiveEffectInspection {
        self.interactive_effect.inspection()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBrowserActionResult {
    kind: &'static str,
    success: bool,
    origin: Option<String>,
    content_digest: Option<AppDigest>,
    content: Option<String>,
    elements: Vec<AppReference>,
    observation_ref: Option<AppReference>,
}

impl AppBrowserActionResult {
    pub fn success(&self) -> bool {
        self.success
    }

    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn elements(&self) -> &[AppReference] {
        &self.elements
    }

    pub fn observation_ref(&self) -> Option<&AppReference> {
        self.observation_ref.as_ref()
    }
}

#[derive(Clone)]
pub(crate) struct AppBrowserObservationEvidence {
    owner_target_digest: AppDigest,
    geometry: AppInteractiveGeometry,
    content_digest: AppDigest,
    element_selectors: BTreeMap<AppReference, String>,
    evidence_bytes: u64,
    evidence_nodes: u64,
}

pub(crate) struct AppBrowserObservedEffect<R> {
    result: ActionResult,
    canonical_result: Vec<u8>,
    observation: Option<AppBrowserObservationEvidence>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    effect: AppEffectInFlight<R>,
}

impl<R> AppBrowserObservedEffect<R> {
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
        error: AppBrowserError,
    ) -> AppBrowserUncertainEffect<R> {
        AppBrowserUncertainEffect {
            error,
            interactive_receipt: Some(self.interactive_receipt),
            settlement: self.effect.outcome_uncertain(stage),
        }
    }

    pub(crate) fn commit(
        self,
    ) -> Result<AppBrowserCommittedEffect<R>, AppBrowserUncertainEffect<R>> {
        let settlement = match self.effect.commit_result(&self.canonical_result) {
            Ok(settlement) => settlement,
            Err(settlement) => {
                return Err(AppBrowserUncertainEffect {
                    error: AppBrowserError::ResultTooLarge,
                    interactive_receipt: Some(self.interactive_receipt),
                    settlement,
                })
            },
        };
        Ok(AppBrowserCommittedEffect {
            result: self.result,
            canonical_result: self.canonical_result,
            observation: self.observation,
            interactive_receipt: self.interactive_receipt,
            settlement,
        })
    }
}

pub(crate) struct AppBrowserCommittedEffect<R> {
    result: ActionResult,
    canonical_result: Vec<u8>,
    observation: Option<AppBrowserObservationEvidence>,
    interactive_receipt: AppInteractiveSettlementReceipt,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppBrowserCommittedEffect<R> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ActionResult,
        Vec<u8>,
        Option<AppBrowserObservationEvidence>,
        AppInteractiveSettlementReceipt,
        AppEffectSettlement<R>,
    ) {
        (
            self.result,
            self.canonical_result,
            self.observation,
            self.interactive_receipt,
            self.settlement,
        )
    }
}

pub(crate) struct AppBrowserUncertainEffect<R> {
    error: AppBrowserError,
    interactive_receipt: Option<AppInteractiveSettlementReceipt>,
    settlement: AppEffectSettlement<R>,
}

impl<R> AppBrowserUncertainEffect<R> {
    pub(crate) fn interactive_receipt(&self) -> Option<&AppInteractiveSettlementReceipt> {
        self.interactive_receipt.as_ref()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        AppBrowserError,
        Option<AppInteractiveSettlementReceipt>,
        AppEffectSettlement<R>,
    ) {
        (self.error, self.interactive_receipt, self.settlement)
    }
}

pub(crate) enum AppBrowserEffectOutcome<R> {
    Observed(AppBrowserObservedEffect<R>),
    Uncertain(AppBrowserUncertainEffect<R>),
}

/// Bounded pre-start capacity for the final physical CLI identity fence. The
/// workflow reserves it before durable dispatch-start, so the atomic launch
/// edge never waits on an unbounded file-hash queue.
pub(crate) struct AppBrowserIoSlot {
    _permit: OwnedSemaphorePermit,
}

impl AppBrowserIoSlot {
    pub(crate) fn is_live_for_start(&self) -> bool {
        true
    }
}

pub(crate) async fn reserve_browser_io_slot() -> Result<AppBrowserIoSlot, AppBrowserError> {
    let permit = Arc::clone(browser_file_hash_slots())
        .acquire_owned()
        .await
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    Ok(AppBrowserIoSlot { _permit: permit })
}

/// Consume the sole common-effect provider token and call the existing browser
/// dispatcher. Any inability to re-observe current URL/tabs after the first
/// poll is uncertain; it is never converted into proven-unspent cancellation.
pub(crate) async fn execute_started_browser<R>(
    io_slot: AppBrowserIoSlot,
    action: AppBrowserEffectAction,
    mut effect: AppEffectInFlight<R>,
    physical_timeout: std::time::Duration,
) -> AppBrowserEffectOutcome<R> {
    let resource_deadline = tokio::time::Instant::now() + physical_timeout;
    let Some(authorization) = effect.take_provider_io_authorization() else {
        let receipt = action.interactive_effect.outcome_uncertain().ok();
        return uncertain(
            AppBrowserError::EffectBindingMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    };
    let owner_permit = match action
        .interactive_effect
        .start_owner_io(authorization, Utc::now())
    {
        Ok(permit) => permit,
        Err(failure) => {
            let (_, permit) = failure.into_parts();
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(permit).ok();
            return uncertain(
                AppBrowserError::EffectBindingMismatch,
                receipt,
                effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
            );
        },
    };
    if owner_permit.cancellation_reason().is_some()
        || tokio::time::Instant::now() >= resource_deadline
    {
        owner_permit
            .cancellation()
            .cancel(AppInteractiveCancellationReason::Deadline);
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ProviderIo),
        );
    }
    let deadline_now = Utc::now();
    let resource_expires_at = chrono::Duration::from_std(physical_timeout)
        .ok()
        .and_then(|duration| deadline_now.checked_add_signed(duration))
        .unwrap_or(deadline_now);
    let cancellation = owner_permit.cancellation();
    let expires_at = owner_permit.expires_at().min(resource_expires_at);
    if !matches!(
        await_browser_owner(
            &cancellation,
            expires_at,
            resource_deadline,
            hash_browser_cli_identity_in_slot(io_slot, action.cli_path.clone()),
        )
        .await,
        Ok(digest) if digest == action.cli_identity_digest
    ) {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::ReviewMismatch,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    }
    // The bounded hash is deliberately outside the physical owner process,
    // but cancellation or expiry may arrive while that blocking read is in
    // flight. Recheck before the first dispatcher poll; after this point every
    // owner await is raced against the same stop/deadline fence.
    if owner_permit.cancellation_reason().is_some() || Utc::now() >= owner_permit.expires_at() {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::FinalPreIoFence),
        );
    }
    let dispatched =
        match await_browser_owner(&cancellation, expires_at, resource_deadline, async {
            action
                .dispatcher
                .dispatch(action.command, &action.arguments)
                .await
                .map_err(|_| AppBrowserError::PhysicalOwnerFailed)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                return uncertain(
                    error,
                    receipt,
                    effect.outcome_uncertain(AppEffectStage::ProviderIo),
                );
            },
        };
    if !dispatched.success && !matches!(action.action, AppBrowserAction::Observe) {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::PhysicalOwnerFailed,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ProviderIo),
        );
    }
    if !dispatched.artifacts.is_empty() {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::ForbiddenArtifact,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    if owner_permit.cancellation_reason().is_some()
        || tokio::time::Instant::now() >= resource_deadline
    {
        cancellation.cancel(AppInteractiveCancellationReason::Deadline);
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ProviderIo),
        );
    }
    let state = match inspect_browser_state(
        &action.dispatcher,
        &action.policy,
        &cancellation,
        expires_at,
        resource_deadline,
    )
    .await
    {
        Ok(state) => state,
        Err(error) => {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return uncertain(
                error,
                receipt,
                effect.outcome_uncertain(AppEffectStage::ProviderIo),
            );
        },
    };
    let mut observation = None;
    let mut result = AppBrowserActionResult {
        kind: "app_browser_interactive",
        success: dispatched.success,
        origin: state.origin.map(|origin| origin.label()),
        content_digest: None,
        content: None,
        elements: Vec::new(),
        observation_ref: None,
    };
    let raw_evidence = match combined_evidence_bytes(
        dispatched.stdout.as_bytes(),
        &state.raw_evidence,
        usize::try_from(owner_permit.claim().evidence_bytes).unwrap_or(usize::MAX),
    ) {
        Ok(bytes) => bytes,
        Err(error) => {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return uncertain(
                error,
                receipt,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            );
        },
    };
    let evidence_bytes = u64::try_from(raw_evidence.len()).unwrap_or(u64::MAX);
    let evidence_digest = (!raw_evidence.is_empty()).then(|| AppDigest::blake3(&raw_evidence));
    if matches!(action.action, AppBrowserAction::Observe) && dispatched.success {
        let content_digest = AppDigest::blake3(dispatched.stdout.as_bytes());
        let (content, element_selectors) = match opaque_browser_elements(&dispatched.stdout) {
            Ok(value) => value,
            Err(error) => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                return uncertain(
                    error,
                    receipt,
                    effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                );
            },
        };
        result.content_digest = Some(content_digest.clone());
        result.content = Some(content);
        result.elements = element_selectors.keys().cloned().collect();
        let viewport =
            match await_browser_owner(&cancellation, expires_at, resource_deadline, async {
                action
                    .dispatcher
                    .exact_live_viewport()
                    .await
                    .filter(|viewport| viewport.0 > 0 && viewport.1 > 0)
                    .ok_or(AppBrowserError::StateUnobservable)
            })
            .await
            {
                Ok(viewport) => viewport,
                Err(_) => {
                    let receipt =
                        AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                    return uncertain(
                        AppBrowserError::StateUnobservable,
                        receipt,
                        effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                    );
                },
            };
        observation = Some(AppBrowserObservationEvidence {
            owner_target_digest: action.owner_target_digest.clone(),
            geometry: AppInteractiveGeometry {
                width: viewport.0,
                height: viewport.1,
                scale_millis: 1000,
            },
            content_digest,
            evidence_nodes: u64::try_from(element_selectors.len()).unwrap_or(u64::MAX),
            element_selectors,
            evidence_bytes,
        });
    }
    if owner_permit.cancellation_reason().is_some() {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ProviderIo),
        );
    }
    if let Some(evidence) = observation.clone() {
        match (&action.live_session, &action.current) {
            (Some(live), Some(current)) => {
                let labels_digest = evidence.content_digest.clone();
                let mut live =
                    match await_browser_owner(&cancellation, expires_at, resource_deadline, async {
                        Ok(live.lock().await)
                    })
                    .await
                    {
                        Ok(live) => live,
                        Err(error) => {
                            let receipt =
                                AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit)
                                    .ok();
                            return uncertain(
                                error,
                                receipt,
                                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                            );
                        },
                    };
                match live.owner.materialize_observation(
                    current,
                    evidence,
                    labels_digest,
                    Utc::now(),
                ) {
                    Ok(observation) => {
                        result.observation_ref = Some(observation.public_ref.clone());
                        live.observation = Some(observation);
                    },
                    Err(error) => {
                        let receipt =
                            AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                        return uncertain(
                            error,
                            receipt,
                            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                        );
                    },
                }
            },
            (None, None) => {},
            _ => {
                let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
                return uncertain(
                    AppBrowserError::ObservationMismatch,
                    receipt,
                    effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
                );
            },
        }
    }
    let result = match serde_json::to_value(&result) {
        Ok(data) => ActionResult::Browser { data },
        Err(_) => {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return uncertain(
                AppBrowserError::Encoding("browser result is not serializable".to_owned()),
                receipt,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            );
        },
    };
    let canonical_result = match serde_json::to_value(&result)
        .map_err(|error| error.to_string())
        .and_then(|value| canonical_json_bytes(&value).map_err(|error| error.to_string()))
    {
        Ok(bytes) => bytes,
        Err(_) => {
            let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
            return uncertain(
                AppBrowserError::Encoding("browser result is not canonical".to_owned()),
                receipt,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            );
        },
    };
    if AppInteractiveSettlementReceipt::preflight_completed(
        &owner_permit,
        &canonical_result,
        evidence_digest.as_ref(),
        evidence_bytes,
    )
    .is_err()
    {
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::ResultTooLarge,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    if owner_permit.cancellation_reason().is_some()
        || tokio::time::Instant::now() >= resource_deadline
    {
        cancellation.cancel(AppInteractiveCancellationReason::Deadline);
        let receipt = AppInteractiveSettlementReceipt::outcome_uncertain(owner_permit).ok();
        return uncertain(
            AppBrowserError::CancelledAfterStart,
            receipt,
            effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
        );
    }
    let interactive_receipt = match AppInteractiveSettlementReceipt::completed(
        owner_permit,
        &canonical_result,
        evidence_digest,
        evidence_bytes,
    ) {
        Ok(receipt) => receipt,
        Err(_) => {
            return uncertain(
                AppBrowserError::ResultTooLarge,
                None,
                effect.outcome_uncertain(AppEffectStage::ResultMaterialization),
            )
        },
    };
    AppBrowserEffectOutcome::Observed(AppBrowserObservedEffect {
        result,
        canonical_result,
        observation,
        interactive_receipt,
        effect,
    })
}

fn uncertain<R>(
    error: AppBrowserError,
    interactive_receipt: Option<AppInteractiveSettlementReceipt>,
    settlement: AppEffectSettlement<R>,
) -> AppBrowserEffectOutcome<R> {
    AppBrowserEffectOutcome::Uncertain(AppBrowserUncertainEffect {
        error,
        interactive_receipt,
        settlement,
    })
}

struct BrowserStateObservation {
    origin: Option<AppBrowserOrigin>,
    raw_evidence: Vec<u8>,
}

async fn inspect_browser_state(
    dispatcher: &BrowserDispatcher,
    policy: &AppBrowserTargetPolicy,
    cancellation: &AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
    resource_deadline: tokio::time::Instant,
) -> Result<BrowserStateObservation, AppBrowserError> {
    let current = await_browser_owner(cancellation, expires_at, resource_deadline, async {
        dispatcher
            .dispatch("get", &json!({"args": ["url"]}))
            .await
            .map_err(|_| AppBrowserError::StateUnobservable)
    })
    .await?;
    if !current.success {
        return Err(AppBrowserError::StateUnobservable);
    }
    let current_url = extract_browser_url(&current)?;
    let origin = if current_url == "about:blank" {
        None
    } else {
        Some(policy.permits_url(&current_url)?)
    };
    let tabs = await_browser_owner(cancellation, expires_at, resource_deadline, async {
        dispatcher
            .dispatch("tab", &json!({"args": ["list", "--json"]}))
            .await
            .map_err(|_| AppBrowserError::StateUnobservable)
    })
    .await?;
    if !tabs.success {
        return Err(AppBrowserError::StateUnobservable);
    }
    enforce_owned_tab_policy(&tabs, policy)?;
    Ok(BrowserStateObservation {
        origin,
        raw_evidence: combined_evidence_bytes(
            current.stdout.as_bytes(),
            tabs.stdout.as_bytes(),
            APP_BROWSER_STATE_EVIDENCE_CEILING as usize,
        )?,
    })
}

/// Race every physical browser await against the stop-only capability, the
/// exact interactive permit deadline, and the earlier absolute resource
/// deadline. Dropping the dispatcher future drops its `kill_on_drop` child, so
/// cancellation cannot leave an owner subprocess running in the background
/// while the common effect is settled uncertain.
async fn await_browser_owner<T, F>(
    cancellation: &AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
    resource_deadline: tokio::time::Instant,
    future: F,
) -> Result<T, AppBrowserError>
where
    F: Future<Output = Result<T, AppBrowserError>>,
{
    let stop = wait_for_browser_stop(cancellation.clone(), expires_at);
    tokio::pin!(stop);
    tokio::pin!(future);
    let resource_expiry = tokio::time::sleep_until(resource_deadline);
    tokio::pin!(resource_expiry);
    tokio::select! {
        biased;
        _ = &mut resource_expiry => {
            cancellation.cancel(AppInteractiveCancellationReason::Deadline);
            Err(AppBrowserError::CancelledAfterStart)
        },
        _ = &mut stop => {
            cancellation.cancel(AppInteractiveCancellationReason::Deadline);
            Err(AppBrowserError::CancelledAfterStart)
        },
        result = &mut future => result,
    }
}

async fn wait_for_browser_stop(
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
            .unwrap_or_default()
            .min(std::time::Duration::from_millis(25));
        tokio::time::sleep(remaining).await;
    }
}

fn extract_browser_url(
    result: &crate::magician_v2::execution::primitive_dispatch::runner::PrimitiveToolResult,
) -> Result<String, AppBrowserError> {
    if let Some(value) = result.parsed_json.as_ref().and_then(Value::as_str) {
        return Ok(value.trim().to_owned());
    }
    let trimmed = result.stdout.trim();
    if let Ok(value) = serde_json::from_str::<String>(trimmed) {
        return Ok(value.trim().to_owned());
    }
    if trimmed.is_empty() || trimmed.len() > MAX_APP_BROWSER_URL_BYTES {
        return Err(AppBrowserError::StateUnobservable);
    }
    Ok(trimmed.to_owned())
}

fn enforce_owned_tab_policy(
    result: &crate::magician_v2::execution::primitive_dispatch::runner::PrimitiveToolResult,
    policy: &AppBrowserTargetPolicy,
) -> Result<(), AppBrowserError> {
    let value = result
        .parsed_json
        .clone()
        .or_else(|| serde_json::from_str(result.stdout.trim()).ok())
        .ok_or(AppBrowserError::StateUnobservable)?;
    let tabs = value
        .get("data")
        .and_then(|value| value.get("tabs"))
        .or_else(|| value.get("tabs"))
        .and_then(Value::as_array)
        .ok_or(AppBrowserError::StateUnobservable)?;
    if tabs.is_empty() || tabs.len() > usize::from(policy.max_owned_tabs) {
        return Err(AppBrowserError::PopupPolicyViolation);
    }
    for tab in tabs {
        let url = tab
            .get("url")
            .and_then(Value::as_str)
            .ok_or(AppBrowserError::StateUnobservable)?;
        if url != "about:blank" {
            policy.permits_url(url)?;
        }
    }
    Ok(())
}

fn opaque_browser_elements(
    raw: &str,
) -> Result<(String, BTreeMap<AppReference, String>), AppBrowserError> {
    let mut raw_refs = BTreeSet::new();
    let bytes = raw.as_bytes();
    let mut index = 0usize;
    while index + 2 <= bytes.len() {
        if bytes[index] == b'@' && bytes.get(index + 1) == Some(&b'e') {
            let mut end = index + 2;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end > index + 2 {
                raw_refs.insert(raw[index..end].to_owned());
                if raw_refs.len() > MAX_APP_BROWSER_ELEMENTS {
                    return Err(AppBrowserError::EvidenceTooLarge);
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    let mut map = BTreeMap::new();
    for raw_ref in &raw_refs {
        // Physical selectors are low-entropy (`@eN`). A public derivation from
        // content/owner digests would therefore be reversible by enumeration.
        // Mint an unguessable logical identity and retain the association only
        // in the move-only owner observation.
        let opaque = AppReference::parse(format!("browser-element:{}", Uuid::new_v4()))
            .map_err(|_| AppBrowserError::Encoding("invalid element ref".to_owned()))?;
        if map.insert(opaque, raw_ref.clone()).is_some() {
            return Err(AppBrowserError::Encoding(
                "duplicate random element ref".to_owned(),
            ));
        }
    }
    let mut replacements = map.iter().collect::<Vec<_>>();
    replacements.sort_by(|(_, left), (_, right)| right.len().cmp(&left.len()));
    let mut sanitized = raw.to_owned();
    for (opaque, physical) in replacements {
        sanitized = sanitized.replace(physical, opaque.as_str());
    }
    // Leave deterministic room for the typed envelope and opaque element refs
    // inside the reviewed 512-KiB canonical result ceiling.
    if sanitized.len() > APP_BROWSER_OBSERVE_CONTENT_CEILING {
        return Err(AppBrowserError::EvidenceTooLarge);
    }
    Ok((sanitized, map))
}

fn validate_locked_browser_action(
    primitive: &AppLockedPrimitiveBinding,
    locked: &AppLockedPrimitiveActionBinding,
    reviewed: &AppInteractiveReviewedAction,
    source_digest: &AppDigest,
) -> Result<(), AppBrowserError> {
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
        return Err(AppBrowserError::ReviewMismatch);
    }
    Ok(())
}

fn app_browser_session_id(
    grant: &AppInteractiveGrantDescriptor,
    run_ref: &AppReference,
    scope_digest: &AppDigest,
    resource_lease_ref: &AppReference,
    session_ordinal: u16,
    profile: AppBrowserProfileClass,
    cli_identity_digest: &AppDigest,
) -> Result<String, AppBrowserError> {
    let digest = AppDigest::blake3_canonical_json(&json!({
        "schema": APP_BROWSER_PROFILE_V1,
        "grant_descriptor_digest": grant.descriptor_digest(),
        "installation_id": grant.installation_id(),
        "installation_generation": grant.installation_generation(),
        "run_ref": run_ref,
        "scope_digest": scope_digest,
        "resource_lease_ref": resource_lease_ref,
        "session_ordinal": session_ordinal,
        "profile": profile,
        "cli_identity_digest": cli_identity_digest,
    }))
    .map_err(|error| AppBrowserError::Encoding(error.to_string()))?;
    Ok(format!("app-browser-{}", &digest_hex(&digest)?[..32]))
}

fn browser_file_hash_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(APP_BROWSER_FILE_HASH_SLOTS)))
}

async fn resolve_browser_cli_identity(
    storage_root: PathBuf,
    principal: String,
    workspace: String,
) -> Result<(PathBuf, AppDigest), AppBrowserError> {
    run_browser_file_task(move || {
        let path = AgentBrowserSession::resolve_cli_path_for_scope(
            Some(&storage_root),
            Some(&principal),
            Some(&workspace),
        )
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
        let digest = hash_browser_cli_identity_blocking(&path)?;
        Ok((path, digest))
    })
    .await
}

async fn hash_browser_cli_identity(path: PathBuf) -> Result<AppDigest, AppBrowserError> {
    run_browser_file_task(move || hash_browser_cli_identity_blocking(&path)).await
}

async fn hash_browser_cli_identity_in_slot(
    slot: AppBrowserIoSlot,
    path: PathBuf,
) -> Result<AppDigest, AppBrowserError> {
    let result = tokio::task::spawn_blocking(move || hash_browser_cli_identity_blocking(&path))
        .await
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    drop(slot);
    result
}

async fn run_browser_file_task<T>(
    task: impl FnOnce() -> Result<T, AppBrowserError> + Send + 'static,
) -> Result<T, AppBrowserError>
where
    T: Send + 'static,
{
    let slot = Arc::clone(browser_file_hash_slots())
        .acquire_owned()
        .await
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    let result = tokio::task::spawn_blocking(task)
        .await
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    drop(slot);
    result
}

fn hash_browser_cli_identity_blocking(path: &Path) -> Result<AppDigest, AppBrowserError> {
    let canonical_path = path
        .canonicalize()
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    let mut file =
        File::open(&canonical_path).map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > APP_BROWSER_MAX_CLI_BYTES {
        return Err(AppBrowserError::PhysicalOwnerUnavailable);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.app-browser.cli-content.v1\0");
    hasher.update(&metadata.len().to_le_bytes());
    let mut observed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes = file
            .read(&mut buffer)
            .map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?;
        if bytes == 0 {
            break;
        }
        observed = observed
            .checked_add(
                u64::try_from(bytes).map_err(|_| AppBrowserError::PhysicalOwnerUnavailable)?,
            )
            .ok_or(AppBrowserError::PhysicalOwnerUnavailable)?;
        if observed > APP_BROWSER_MAX_CLI_BYTES {
            return Err(AppBrowserError::PhysicalOwnerUnavailable);
        }
        hasher.update(&buffer[..bytes]);
    }
    if observed != metadata.len() {
        return Err(AppBrowserError::PhysicalOwnerUnavailable);
    }
    let path_digest = AppDigest::blake3(canonical_path.as_os_str().as_encoded_bytes());
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_BROWSER_PROFILE_V1,
        "canonical_path_digest": path_digest,
        "content_digest": AppDigest::blake3(hasher.finalize().as_bytes()),
        "bytes": observed,
    }))
    .map_err(|error| AppBrowserError::Encoding(error.to_string()))
}

fn combined_evidence_bytes(
    left: &[u8],
    right: &[u8],
    ceiling: usize,
) -> Result<Vec<u8>, AppBrowserError> {
    let delimiter_bytes = usize::from(!left.is_empty() && !right.is_empty());
    let length = left
        .len()
        .checked_add(right.len())
        .and_then(|value| value.checked_add(delimiter_bytes))
        .ok_or(AppBrowserError::EvidenceTooLarge)?;
    if length > ceiling {
        return Err(AppBrowserError::EvidenceTooLarge);
    }
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(left);
    if delimiter_bytes != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(right);
    Ok(bytes)
}

fn digest_hex(digest: &AppDigest) -> Result<&str, AppBrowserError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or_else(|| AppBrowserError::Encoding("invalid canonical digest".to_owned()))
}

#[derive(Debug, Error)]
pub(crate) enum AppBrowserError {
    #[error("browser target origin is invalid")]
    InvalidOrigin,
    #[error("browser target policy is invalid")]
    InvalidTargetPolicy,
    #[error("browser target, redirect or popup is outside the reviewed origin policy")]
    TargetDenied,
    #[error("browser popup/new-tab state exceeds the reviewed policy")]
    PopupPolicyViolation,
    #[error("browser action input is invalid")]
    InvalidInput,
    #[error("browser action is not present in the reviewed grant")]
    ActionNotReviewed,
    #[error("browser descriptor, profile or physical implementation does not match review")]
    ReviewMismatch,
    #[error("browser observation belongs to another owner/session")]
    ObservationMismatch,
    #[error("browser owner cannot re-observe current URL and tabs")]
    StateUnobservable,
    #[error("browser evidence exceeds its reviewed bound")]
    EvidenceTooLarge,
    #[error("browser actions cannot return direct filesystem artifacts")]
    ForbiddenArtifact,
    #[error("the exact browser physical owner is unavailable")]
    PhysicalOwnerUnavailable,
    #[error("browser physical execution failed after provider I/O was authorized")]
    PhysicalOwnerFailed,
    #[error("browser action was cancelled after durable dispatch-start")]
    CancelledAfterStart,
    #[error("browser action does not match the common effect binding")]
    EffectBindingMismatch,
    #[error("browser result exceeds the reviewed bound")]
    ResultTooLarge,
    #[error("browser session cleanup failed")]
    #[allow(dead_code)] // Reserved for a physical owner cleanup failure.
    CleanupFailed,
    #[error("failed to encode browser identity/result: {0}")]
    Encoding(String),
    #[error(transparent)]
    Interactive(#[from] AppInteractiveError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::primitive_dispatch::runner::PrimitiveToolResult;

    #[test]
    fn origin_policy_is_exact_and_rejects_embedded_credentials() {
        let policy = AppBrowserTargetPolicy::reviewed(
            BTreeSet::from([
                AppBrowserOrigin::parse("https://example.com").unwrap(),
                AppBrowserOrigin::parse("https://example.com:8443").unwrap(),
            ]),
            2,
        )
        .unwrap();
        assert!(policy.permits_url("https://example.com/path?q=1").is_ok());
        assert!(policy.permits_url("https://example.com:8443/path").is_ok());
        assert!(policy.permits_url("https://sub.example.com/").is_err());
        assert!(policy
            .permits_url("https://user:pass@example.com/")
            .is_err());
        assert!(policy.permits_url("http://example.com/").is_err());
        assert!(AppBrowserOrigin::parse("https://example.com/safe-prefix").is_err());
        assert!(AppBrowserOrigin::parse("https://example.com/?scope=wide").is_err());
        assert!(AppBrowserOrigin::parse("https://example.com/#fragment").is_err());
    }

    #[test]
    fn element_projection_removes_every_physical_selector() {
        let (sanitized, selectors) =
            opaque_browser_elements("button @e1 label\ntextbox @e10 value").unwrap();
        assert_eq!(selectors.len(), 2);
        assert!(!sanitized.contains("@e1"));
        assert!(!sanitized.contains("@e10"));
        assert!(selectors
            .keys()
            .all(|value| value.as_str().starts_with("browser-element:")));
        let (_, second) = opaque_browser_elements("button @e1 label").unwrap();
        assert_ne!(
            selectors
                .iter()
                .find(|(_, physical)| physical.as_str() == "@e1")
                .map(|(opaque, _)| opaque),
            second.keys().next(),
        );
    }

    #[test]
    fn popup_policy_checks_every_owned_top_level_url() {
        let policy = AppBrowserTargetPolicy::reviewed(
            BTreeSet::from([AppBrowserOrigin::parse("https://example.com").unwrap()]),
            2,
        )
        .unwrap();
        let allowed = PrimitiveToolResult {
            success: true,
            stdout: serde_json::to_string(&json!({
                "data":{"tabs":[
                    {"url":"https://example.com/a"},
                    {"url":"about:blank"}
                ]}
            }))
            .unwrap(),
            ..PrimitiveToolResult::default()
        };
        assert!(enforce_owned_tab_policy(&allowed, &policy).is_ok());
        let denied = PrimitiveToolResult {
            success: true,
            stdout: serde_json::to_string(&json!({
                "data":{"tabs":[{"url":"https://attacker.example/"}]}
            }))
            .unwrap(),
            ..PrimitiveToolResult::default()
        };
        assert!(matches!(
            enforce_owned_tab_policy(&denied, &policy),
            Err(AppBrowserError::TargetDenied)
        ));
    }

    #[test]
    fn reviewed_subset_has_no_raw_browser_transport_actions() {
        let actions = reviewed_browser_actions().unwrap();
        assert_eq!(actions.len(), 4);
        let refs = actions
            .iter()
            .map(|action| action.action_ref().as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            refs,
            BTreeSet::from([
                APP_BROWSER_OBSERVE_ACTION_REF,
                APP_BROWSER_NAVIGATE_ACTION_REF,
                APP_BROWSER_SCROLL_ACTION_REF,
                APP_BROWSER_CLICK_ACTION_REF,
            ])
        );
        assert!(!refs.iter().any(|value| {
            value.contains("eval")
                || value.contains("cdp")
                || value.contains("cookie")
                || value.contains("upload")
                || value.contains("download")
                || value.contains("argv")
        }));
    }

    #[test]
    fn empty_evidence_has_no_synthetic_delimiter() {
        assert!(combined_evidence_bytes(b"", b"", 0).unwrap().is_empty());
        assert_eq!(combined_evidence_bytes(b"left", b"", 4).unwrap(), b"left");
        assert_eq!(combined_evidence_bytes(b"", b"right", 5).unwrap(), b"right");
        assert_eq!(
            combined_evidence_bytes(b"left", b"right", 10).unwrap(),
            b"left\0right"
        );
    }
}
