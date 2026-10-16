//! Shared authority and typestate contracts for app-owned interactive I/O.
//!
//! Browser, macOS and Android keep their existing physical execution owners.
//! This module supplies the owner-neutral review/session/observation/action
//! boundary they share.  Handles and permits deliberately implement neither
//! `Clone` nor Serde: persisted bytes, package input and default/custom UI code
//! cannot become interactive authority.

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    effect_kernel::{AppEffectBinding, AppEffectProviderIoAuthorization},
    models::{AppDigest, AppInstallationId, AppReference, AppRevision},
};

pub(crate) const APP_INTERACTIVE_CONTRACT_V1: &str = "magician.app-interactive.v1";
pub(crate) const MAX_APP_INTERACTIVE_ACTIONS: usize = 64;
pub(crate) const MAX_APP_INTERACTIVE_INPUT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_APP_INTERACTIVE_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_APP_INTERACTIVE_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_APP_INTERACTIVE_EVIDENCE_NODES: u64 = 64 * 1024;
pub(crate) const MAX_APP_INTERACTIVE_PIXELS: u64 = 16_777_216;
pub(crate) const MAX_APP_INTERACTIVE_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_APP_INTERACTIVE_STEPS: u32 = 10_000;
pub(crate) const MAX_APP_INTERACTIVE_DURATION_SECONDS: u64 = 24 * 60 * 60;
pub(crate) const MAX_APP_INTERACTIVE_SESSIONS: u16 = 32;
pub(crate) const MAX_APP_INTERACTIVE_GRANT_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
pub(crate) const MAX_APP_INTERACTIVE_ORIGINS: usize = 64;
pub(crate) const MAX_APP_INTERACTIVE_TARGET_SELECTORS: usize = 64;
pub const APP_INTERACTIVE_REQUEST_SCHEMA_V1: &str =
    "magician.app-interactive-capability-request.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveExecutionProfile {
    BrowserSession,
    MacosHost,
    AndroidDevice,
}

/// Physical authority owner requested by one exact manifest dependency.
///
/// This is intentionally distinct from transport names (CDP, AX, ADB, etc.).
/// Those remain private to the owner adapter and can never appear in an app
/// manifest, review grant or generated client carrier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveOwnerKind {
    Browser,
    Macos,
    Android,
}

impl AppInteractiveOwnerKind {
    pub fn execution_profile(self) -> AppInteractiveExecutionProfile {
        match self {
            Self::Browser => AppInteractiveExecutionProfile::BrowserSession,
            Self::Macos => AppInteractiveExecutionProfile::MacosHost,
            Self::Android => AppInteractiveExecutionProfile::AndroidDevice,
        }
    }
}

/// Closed owner profile classes understood by the current physical adapters.
/// New physical breadth must add a reviewed class rather than accepting an
/// arbitrary transport/profile string from a package.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveTargetProfileClass {
    InstallationEphemeralHeadless,
    OwnerReviewedMacosPairing,
    OwnerReviewedAndroidPairing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveActionClass {
    Observe,
    NavigateOrLaunch,
    Interact,
    CapturePixels,
    TransferArtifact,
    OutwardCommit,
}

/// Resolve the only action classes the three sealed physical owners currently
/// implement. The action name comes from an exact selected descriptor leaf;
/// there is deliberately no wildcard, default action or transport verb
/// fallback here.
pub(crate) fn reviewed_interactive_action_class(
    owner: AppInteractiveOwnerKind,
    action_name: &str,
) -> Option<AppInteractiveActionClass> {
    use AppInteractiveActionClass::{
        CapturePixels, Interact, NavigateOrLaunch, Observe, OutwardCommit,
    };
    match (owner, action_name) {
        (AppInteractiveOwnerKind::Browser, "snapshot") => Some(Observe),
        (AppInteractiveOwnerKind::Browser, "navigate") => Some(NavigateOrLaunch),
        (AppInteractiveOwnerKind::Browser, "scroll") => Some(Interact),
        (AppInteractiveOwnerKind::Browser, "click") => Some(OutwardCommit),
        (AppInteractiveOwnerKind::Macos, "launch" | "focus") => Some(NavigateOrLaunch),
        (AppInteractiveOwnerKind::Macos, "snapshot") => Some(Observe),
        (AppInteractiveOwnerKind::Macos, "type" | "scroll") => Some(Interact),
        (AppInteractiveOwnerKind::Macos, "click" | "key" | "drag") => Some(OutwardCommit),
        (AppInteractiveOwnerKind::Android, "snapshot") => Some(Observe),
        (AppInteractiveOwnerKind::Android, "screenshot") => Some(CapturePixels),
        (AppInteractiveOwnerKind::Android, "launch" | "close") => Some(NavigateOrLaunch),
        (AppInteractiveOwnerKind::Android, "type" | "scroll") => Some(Interact),
        (AppInteractiveOwnerKind::Android, "tap" | "key") => Some(OutwardCommit),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveBackgroundPosture {
    DirectOwner,
    ReviewedBoundedBackground,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveCapturePosture {
    StructuredEvidenceOnly,
    ReviewedPixels,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveTransferPosture {
    Denied,
    ReviewedArtifacts,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveSessionPosture {
    InvocationBound,
    RunBound,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveObservationKind {
    StructuredTree,
    Text,
    Pixels,
}

/// Logical target selectors reviewed without exposing a raw process, device,
/// window, CDP target or control token. `current_reviewed_pairing` means the
/// physical owner must resolve and revalidate its already owner-approved
/// pairing immediately before disclosure; it never means "any target".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveTargetSelectors {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub bundle_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub package_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub application_refs: BTreeSet<AppReference>,
    #[serde(default)]
    pub current_reviewed_pairing: bool,
}

impl AppInteractiveTargetSelectors {
    fn count(&self) -> usize {
        self.bundle_ids.len() + self.package_ids.len() + self.application_refs.len()
    }

    fn validate(&self, owner: AppInteractiveOwnerKind) -> Result<(), AppInteractiveError> {
        if self.count() > MAX_APP_INTERACTIVE_TARGET_SELECTORS
            || self
                .bundle_ids
                .iter()
                .chain(self.package_ids.iter())
                .any(|selector| {
                    selector.is_empty()
                        || selector.len() > 255
                        || selector.chars().any(char::is_whitespace)
                })
        {
            return Err(AppInteractiveError::InvalidCapabilityRequest);
        }
        let valid = match owner {
            AppInteractiveOwnerKind::Browser => {
                self.bundle_ids.is_empty()
                    && self.package_ids.is_empty()
                    && self.application_refs.is_empty()
                    && !self.current_reviewed_pairing
            },
            AppInteractiveOwnerKind::Macos => {
                self.package_ids.is_empty()
                    && (!self.bundle_ids.is_empty()
                        || !self.application_refs.is_empty()
                        || self.current_reviewed_pairing)
            },
            AppInteractiveOwnerKind::Android => {
                self.bundle_ids.is_empty()
                    && (!self.package_ids.is_empty()
                        || !self.application_refs.is_empty()
                        || self.current_reviewed_pairing)
            },
        };
        valid
            .then_some(())
            .ok_or(AppInteractiveError::InvalidCapabilityRequest)
    }

    fn is_subset_of(&self, requested: &Self) -> bool {
        self.bundle_ids.is_subset(&requested.bundle_ids)
            && self.package_ids.is_subset(&requested.package_ids)
            && self.application_refs.is_subset(&requested.application_refs)
            && (!self.current_reviewed_pairing || requested.current_reviewed_pairing)
    }
}

/// Expiry and session posture requested by a package and narrowed by its
/// owner. Durations are ceilings, not promises; runtime may always shorten
/// either lifetime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveExpirySessionPosture {
    pub grant_lifetime_seconds: u64,
    pub max_session_seconds: u64,
    pub session: AppInteractiveSessionPosture,
}

impl AppInteractiveExpirySessionPosture {
    fn validate(&self) -> Result<(), AppInteractiveError> {
        if self.grant_lifetime_seconds == 0
            || self.grant_lifetime_seconds > MAX_APP_INTERACTIVE_GRANT_LIFETIME_SECONDS
            || self.max_session_seconds == 0
            || self.max_session_seconds > MAX_APP_INTERACTIVE_DURATION_SECONDS
            || self.max_session_seconds > self.grant_lifetime_seconds
        {
            return Err(AppInteractiveError::InvalidCapabilityRequest);
        }
        Ok(())
    }

    fn narrows(&self, requested: &Self) -> bool {
        self.grant_lifetime_seconds <= requested.grant_lifetime_seconds
            && self.max_session_seconds <= requested.max_session_seconds
            && self.session == requested.session
    }
}

/// Complete manifest-side interactive dependency request. This value is
/// strict, digestible and safely persistable; it is evidence for owner review,
/// never a live interactive capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveCapabilityRequest {
    #[serde(default = "interactive_request_schema")]
    pub schema: String,
    pub owner: AppInteractiveOwnerKind,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub allowed_origins: BTreeSet<String>,
    pub target_profile_class: AppInteractiveTargetProfileClass,
    pub target_selectors: AppInteractiveTargetSelectors,
    pub action_classes: BTreeSet<AppInteractiveActionClass>,
    pub background: AppInteractiveBackgroundPosture,
    pub capture: AppInteractiveCapturePosture,
    pub transfer: AppInteractiveTransferPosture,
    pub resources: AppInteractiveResourceCeilings,
    pub expiry_session: AppInteractiveExpirySessionPosture,
}

fn interactive_request_schema() -> String {
    APP_INTERACTIVE_REQUEST_SCHEMA_V1.to_owned()
}

impl AppInteractiveCapabilityRequest {
    /// Validate the currently admitted production subset. Closed enum values
    /// for future Browser/macOS/Android breadth remain representable, but are
    /// denied until their physical-owner implementation is separately admitted.
    pub fn validate_for_admission(&self) -> Result<(), AppInteractiveError> {
        if self.schema != APP_INTERACTIVE_REQUEST_SCHEMA_V1
            || self.action_classes.len() != 1
            || self.background != AppInteractiveBackgroundPosture::DirectOwner
            || self.transfer != AppInteractiveTransferPosture::Denied
            || self.allowed_origins.len() > MAX_APP_INTERACTIVE_ORIGINS
        {
            return Err(AppInteractiveError::CapabilityRequestNotAdmitted);
        }
        self.resources.validate()?;
        self.expiry_session.validate()?;
        self.target_selectors.validate(self.owner)?;
        match (self.owner, self.target_profile_class) {
            (
                AppInteractiveOwnerKind::Browser,
                AppInteractiveTargetProfileClass::InstallationEphemeralHeadless,
            ) => {
                if self.capture != AppInteractiveCapturePosture::StructuredEvidenceOnly
                    || self.allowed_origins.is_empty()
                    || self
                        .allowed_origins
                        .iter()
                        .any(|origin| !valid_reviewed_origin(origin))
                    || !self.action_classes.iter().all(|class| {
                        matches!(
                            class,
                            AppInteractiveActionClass::Observe
                                | AppInteractiveActionClass::NavigateOrLaunch
                                | AppInteractiveActionClass::Interact
                                | AppInteractiveActionClass::OutwardCommit
                        )
                    })
                {
                    return Err(AppInteractiveError::InvalidCapabilityRequest);
                }
            },
            (
                AppInteractiveOwnerKind::Macos,
                AppInteractiveTargetProfileClass::OwnerReviewedMacosPairing,
            ) => {
                if self.capture != AppInteractiveCapturePosture::StructuredEvidenceOnly
                    || !self.allowed_origins.is_empty()
                    || !self.action_classes.iter().all(|class| {
                        matches!(
                            class,
                            AppInteractiveActionClass::Observe
                                | AppInteractiveActionClass::NavigateOrLaunch
                                | AppInteractiveActionClass::Interact
                                | AppInteractiveActionClass::OutwardCommit
                        )
                    })
                {
                    return Err(AppInteractiveError::InvalidCapabilityRequest);
                }
            },
            (
                AppInteractiveOwnerKind::Android,
                AppInteractiveTargetProfileClass::OwnerReviewedAndroidPairing,
            ) => {
                let pixels = self
                    .action_classes
                    .contains(&AppInteractiveActionClass::CapturePixels);
                if !self.allowed_origins.is_empty()
                    || self.capture
                        != if pixels {
                            AppInteractiveCapturePosture::ReviewedPixels
                        } else {
                            AppInteractiveCapturePosture::StructuredEvidenceOnly
                        }
                    || !self.action_classes.iter().all(|class| {
                        matches!(
                            class,
                            AppInteractiveActionClass::Observe
                                | AppInteractiveActionClass::NavigateOrLaunch
                                | AppInteractiveActionClass::Interact
                                | AppInteractiveActionClass::CapturePixels
                                | AppInteractiveActionClass::OutwardCommit
                        )
                    })
                {
                    return Err(AppInteractiveError::InvalidCapabilityRequest);
                }
            },
            _ => return Err(AppInteractiveError::InvalidCapabilityRequest),
        }
        Ok(())
    }

    pub fn request_digest(&self) -> Result<AppDigest, AppInteractiveError> {
        self.validate_for_admission()?;
        AppDigest::blake3_canonical_json(
            &serde_json::to_value(self)
                .map_err(|error| AppInteractiveError::Encoding(error.to_string()))?,
        )
        .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
    }

    pub fn narrows(&self, requested: &Self) -> bool {
        self.validate_for_admission().is_ok()
            && requested.validate_for_admission().is_ok()
            && self.owner == requested.owner
            && self.target_profile_class == requested.target_profile_class
            && self.allowed_origins.is_subset(&requested.allowed_origins)
            && self
                .target_selectors
                .is_subset_of(&requested.target_selectors)
            && self.action_classes.is_subset(&requested.action_classes)
            && self.background == requested.background
            && self.capture == requested.capture
            && self.transfer == requested.transfer
            && self.resources.narrows(&requested.resources)
            && self.expiry_session.narrows(&requested.expiry_session)
    }

    /// One-release compatibility projection for an already explicit, exact
    /// single `snapshot` dependency. Callers must first prove the descriptor,
    /// selected action, schemas, implementation and result ceiling; this
    /// helper never upgrades a name-only or all-actions declaration.
    pub(crate) fn legacy_observe(
        owner: AppInteractiveOwnerKind,
        result_byte_ceiling: u64,
    ) -> Result<Self, AppInteractiveError> {
        let (target_profile_class, target_selectors, allowed_origins) = match owner {
            AppInteractiveOwnerKind::Browser => (
                AppInteractiveTargetProfileClass::InstallationEphemeralHeadless,
                AppInteractiveTargetSelectors::default(),
                BTreeSet::from(["about:blank".to_owned()]),
            ),
            AppInteractiveOwnerKind::Macos => (
                AppInteractiveTargetProfileClass::OwnerReviewedMacosPairing,
                AppInteractiveTargetSelectors {
                    current_reviewed_pairing: true,
                    ..AppInteractiveTargetSelectors::default()
                },
                BTreeSet::new(),
            ),
            AppInteractiveOwnerKind::Android => (
                AppInteractiveTargetProfileClass::OwnerReviewedAndroidPairing,
                AppInteractiveTargetSelectors {
                    current_reviewed_pairing: true,
                    ..AppInteractiveTargetSelectors::default()
                },
                BTreeSet::new(),
            ),
        };
        let value = Self {
            schema: interactive_request_schema(),
            owner,
            allowed_origins,
            target_profile_class,
            target_selectors,
            action_classes: BTreeSet::from([AppInteractiveActionClass::Observe]),
            background: AppInteractiveBackgroundPosture::DirectOwner,
            capture: AppInteractiveCapturePosture::StructuredEvidenceOnly,
            transfer: AppInteractiveTransferPosture::Denied,
            resources: AppInteractiveResourceCeilings::reviewed(
                1,
                1,
                300,
                result_byte_ceiling.min(MAX_APP_INTERACTIVE_EVIDENCE_BYTES),
                MAX_APP_INTERACTIVE_EVIDENCE_NODES,
                0,
                0,
                result_byte_ceiling,
            )?,
            expiry_session: AppInteractiveExpirySessionPosture {
                grant_lifetime_seconds: MAX_APP_INTERACTIVE_GRANT_LIFETIME_SECONDS,
                max_session_seconds: 300,
                session: AppInteractiveSessionPosture::InvocationBound,
            },
        };
        value.validate_for_admission()?;
        Ok(value)
    }
}

fn valid_reviewed_origin(origin: &str) -> bool {
    if origin == "about:blank" {
        return true;
    }
    let Ok(parsed) = url::Url::parse(origin) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && origin.len() <= 512
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveEffectClass {
    Pure,
    ClockRead,
    NetworkRead,
    WorkspaceRead,
    WorkspaceWrite,
    StructuredDataRead,
    HostRead,
    ExternalMutation,
    DeviceInteraction,
}

/// Exact locked action contract copied into the owner-reviewed grant. This is
/// safe to persist, but remains evidence only; it cannot mint an owner-I/O
/// permit without matching the current package lock and physical owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveLockedActionContract {
    pub action_ref: AppReference,
    pub class: AppInteractiveActionClass,
    pub action_digest: AppDigest,
    pub input_schema_digest: AppDigest,
    pub result_schema_digest: AppDigest,
    pub effects: BTreeSet<AppInteractiveEffectClass>,
    pub implementation_plan_digest: AppDigest,
    pub result_byte_ceiling: u64,
}

impl AppInteractiveLockedActionContract {
    fn validate(&self) -> Result<(), AppInteractiveError> {
        if self.effects.is_empty()
            || self.result_byte_ceiling == 0
            || self.result_byte_ceiling > MAX_APP_INTERACTIVE_OUTPUT_BYTES
            || (self.class != AppInteractiveActionClass::Observe
                && !self.effects.iter().any(|effect| {
                    matches!(
                        effect,
                        AppInteractiveEffectClass::ExternalMutation
                            | AppInteractiveEffectClass::DeviceInteraction
                            | AppInteractiveEffectClass::NetworkRead
                    )
                }))
        {
            return Err(AppInteractiveError::InvalidReviewedAction);
        }
        Ok(())
    }
}

pub const APP_INTERACTIVE_REVIEWED_GRANT_SCHEMA_V1: &str =
    "magician.app-reviewed-interactive-capability-grant.v1";

/// Durable result of owner review for one exact locked interactive dependency.
/// `requested` preserves what the package asked for; `granted` is an explicit
/// subset and never gains implicit all-actions semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedInteractiveCapabilityGrant {
    schema: String,
    dependency_ref: AppReference,
    locked_binding_digest: AppDigest,
    primitive_binding_digest: AppDigest,
    requested_request_digest: AppDigest,
    requested: AppInteractiveCapabilityRequest,
    granted: AppInteractiveCapabilityRequest,
    action: AppInteractiveLockedActionContract,
    grant_digest: AppDigest,
}

impl AppReviewedInteractiveCapabilityGrant {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_owner_review(
        dependency_ref: AppReference,
        locked_binding_digest: AppDigest,
        primitive_binding_digest: AppDigest,
        requested_request_digest: AppDigest,
        requested: AppInteractiveCapabilityRequest,
        granted: AppInteractiveCapabilityRequest,
        action: AppInteractiveLockedActionContract,
    ) -> Result<Self, AppInteractiveError> {
        if requested.request_digest()? != requested_request_digest
            || !granted.narrows(&requested)
            || action.result_byte_ceiling != granted.resources.max_output_bytes()
            || !granted.action_classes.contains(&action.class)
        {
            return Err(AppInteractiveError::InvalidGrant);
        }
        action.validate()?;
        let mut value = Self {
            schema: APP_INTERACTIVE_REVIEWED_GRANT_SCHEMA_V1.to_owned(),
            dependency_ref,
            locked_binding_digest,
            primitive_binding_digest,
            requested_request_digest,
            requested,
            granted,
            action,
            grant_digest: AppDigest::blake3(b"pending-reviewed-interactive-grant"),
        };
        value.grant_digest = value.compute_digest()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), AppInteractiveError> {
        if self.schema != APP_INTERACTIVE_REVIEWED_GRANT_SCHEMA_V1
            || self.requested.request_digest()? != self.requested_request_digest
            || !self.granted.narrows(&self.requested)
            || self.action.result_byte_ceiling != self.granted.resources.max_output_bytes()
            || !self.granted.action_classes.contains(&self.action.class)
            || self.compute_digest()? != self.grant_digest
        {
            return Err(AppInteractiveError::InvalidGrant);
        }
        self.action.validate()
    }

    fn compute_digest(&self) -> Result<AppDigest, AppInteractiveError> {
        #[derive(Serialize)]
        struct Identity<'a> {
            schema: &'a str,
            dependency_ref: &'a AppReference,
            locked_binding_digest: &'a AppDigest,
            primitive_binding_digest: &'a AppDigest,
            requested_request_digest: &'a AppDigest,
            requested: &'a AppInteractiveCapabilityRequest,
            granted: &'a AppInteractiveCapabilityRequest,
            action: &'a AppInteractiveLockedActionContract,
        }
        AppDigest::blake3_canonical_json(
            &serde_json::to_value(Identity {
                schema: &self.schema,
                dependency_ref: &self.dependency_ref,
                locked_binding_digest: &self.locked_binding_digest,
                primitive_binding_digest: &self.primitive_binding_digest,
                requested_request_digest: &self.requested_request_digest,
                requested: &self.requested,
                granted: &self.granted,
                action: &self.action,
            })
            .map_err(|error| AppInteractiveError::Encoding(error.to_string()))?,
        )
        .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
    }

    pub fn dependency_ref(&self) -> &AppReference {
        &self.dependency_ref
    }

    pub fn locked_binding_digest(&self) -> &AppDigest {
        &self.locked_binding_digest
    }

    pub fn primitive_binding_digest(&self) -> &AppDigest {
        &self.primitive_binding_digest
    }

    pub fn requested_request_digest(&self) -> &AppDigest {
        &self.requested_request_digest
    }

    pub fn requested(&self) -> &AppInteractiveCapabilityRequest {
        &self.requested
    }

    pub fn granted(&self) -> &AppInteractiveCapabilityRequest {
        &self.granted
    }

    pub fn action(&self) -> &AppInteractiveLockedActionContract {
        &self.action
    }

    pub fn grant_digest(&self) -> &AppDigest {
        &self.grant_digest
    }

    pub fn expires_at_from(
        &self,
        approved_at: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, AppInteractiveError> {
        let seconds = i64::try_from(self.granted.expiry_session.grant_lifetime_seconds)
            .map_err(|_| AppInteractiveError::InvalidGrant)?;
        approved_at
            .checked_add_signed(Duration::seconds(seconds))
            .ok_or(AppInteractiveError::InvalidGrant)
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveCancellationReason {
    OwnerStop,
    GrantRevoked,
    RunCancelled,
    Deadline,
}

impl AppInteractiveCancellationReason {
    fn code(self) -> u8 {
        match self {
            Self::OwnerStop => 1,
            Self::GrantRevoked => 2,
            Self::RunCancelled => 3,
            Self::Deadline => 4,
        }
    }

    fn from_code(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::OwnerStop),
            2 => Some(Self::GrantRevoked),
            3 => Some(Self::RunCancelled),
            4 => Some(Self::Deadline),
            _ => None,
        }
    }
}

/// Stop-only capability shared with the physical owner. Cloning it can only
/// reduce authority; it cannot resume a session or clear an earlier reason.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppInteractiveCancellation {
    state: Arc<AtomicU8>,
}

impl AppInteractiveCancellation {
    pub(crate) fn cancel(&self, reason: AppInteractiveCancellationReason) {
        let _ = self
            .state
            .compare_exchange(0, reason.code(), Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn reason(&self) -> Option<AppInteractiveCancellationReason> {
        AppInteractiveCancellationReason::from_code(self.state.load(Ordering::Acquire))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) != 0
    }
}

/// Payload-free metadata retained at the common effect boundary. This is an
/// inspection projection, not a session or provider-I/O capability.
pub(crate) struct AppInteractiveEffectInspection {
    pub(crate) profile: AppInteractiveExecutionProfile,
    pub(crate) capability_grant_digest: AppDigest,
    pub(crate) locked_interactive_binding_digest: AppDigest,
    pub(crate) interactive_request_digest: AppDigest,
    pub(crate) owner_target_digest: AppDigest,
    pub(crate) session_binding_digest: AppDigest,
    pub(crate) action_ref: AppReference,
    pub(crate) action_class: AppInteractiveActionClass,
    pub(crate) sequence: u64,
    pub(crate) claim: AppInteractiveResourceClaim,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) cancellation: AppInteractiveCancellation,
}

#[derive(Clone)]
struct AppInteractiveLiveStopTarget {
    cancellation: AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
}

struct AppInteractiveLiveOwnerSlot {
    session_binding_digest: AppDigest,
    expires_at: DateTime<Utc>,
    owner: Arc<dyn Any + Send + Sync>,
}

fn live_owner_slots() -> &'static Mutex<BTreeMap<String, AppInteractiveLiveOwnerSlot>> {
    static OWNERS: OnceLock<Mutex<BTreeMap<String, AppInteractiveLiveOwnerSlot>>> = OnceLock::new();
    OWNERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Attach one typed physical owner state to the existing common live-session
/// identity. This is process-local acceleration only: every recovered action
/// must still reproduce and pass the current grant/source/target fences.
pub(crate) fn register_live_interactive_owner<T: Any + Send + Sync>(
    stable_owner_key: &AppDigest,
    session_binding_digest: &AppDigest,
    expires_at: DateTime<Utc>,
    owner: Arc<T>,
    now: DateTime<Utc>,
) -> bool {
    const MAX_LIVE_OWNERS: usize = 256;
    let mut owners = live_owner_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.retain(|_, slot| slot.expires_at > now);
    if !owners.contains_key(stable_owner_key.as_str()) && owners.len() >= MAX_LIVE_OWNERS {
        return false;
    }
    owners.insert(
        stable_owner_key.to_string(),
        AppInteractiveLiveOwnerSlot {
            session_binding_digest: session_binding_digest.clone(),
            expires_at,
            owner,
        },
    );
    true
}

#[allow(dead_code)] // Exact-session compatibility lookup; keyed lookup is the live path.
pub(crate) fn live_interactive_owner<T: Any + Send + Sync>(
    stable_owner_key: &AppDigest,
    session_binding_digest: &AppDigest,
    now: DateTime<Utc>,
) -> Option<Arc<T>> {
    let mut owners = live_owner_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.retain(|_, slot| slot.expires_at > now);
    let slot = owners.get(stable_owner_key.as_str())?;
    if &slot.session_binding_digest != session_binding_digest {
        return None;
    }
    Arc::downcast::<T>(Arc::clone(&slot.owner)).ok()
}

pub(crate) fn live_interactive_owner_by_key<T: Any + Send + Sync>(
    stable_owner_key: &AppDigest,
    now: DateTime<Utc>,
) -> Option<(Arc<T>, AppDigest)> {
    let mut owners = live_owner_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.retain(|_, slot| slot.expires_at > now);
    let slot = owners.get(stable_owner_key.as_str())?;
    Some((
        Arc::downcast::<T>(Arc::clone(&slot.owner)).ok()?,
        slot.session_binding_digest.clone(),
    ))
}

/// Payload-free process truth for a previously persisted interactive session.
/// This is inspection only: it cannot recover a session handle or authorize
/// owner I/O, and it deliberately collapses non-owner cancellation reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppInteractiveLiveSessionAvailability {
    Active,
    OwnerStopSignalled,
    Closing,
    Unavailable,
}

fn live_stop_targets() -> &'static Mutex<BTreeMap<String, AppInteractiveLiveStopTarget>> {
    static TARGETS: OnceLock<Mutex<BTreeMap<String, AppInteractiveLiveStopTarget>>> =
        OnceLock::new();
    TARGETS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register only the stop-only half of a live session. The bounded registry is
/// process-local acceleration; durable stop intent remains in the workflow
/// control store and is checked independently before physical I/O.
pub(crate) fn register_live_interactive_stop_target(
    session_binding_digest: &AppDigest,
    cancellation: AppInteractiveCancellation,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    const MAX_LIVE_STOP_TARGETS: usize = 256;
    let mut targets = live_stop_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    targets.retain(|_, target| target.expires_at > now);
    if !targets.contains_key(session_binding_digest.as_str())
        && targets.len() >= MAX_LIVE_STOP_TARGETS
    {
        return false;
    }
    targets.insert(
        session_binding_digest.to_string(),
        AppInteractiveLiveStopTarget {
            cancellation,
            expires_at,
        },
    );
    true
}

/// Best-effort signal to the live physical owner. A false result never means
/// that the durable stop request failed; it means only that this process no
/// longer owns the matching session.
pub(crate) fn signal_live_interactive_stop(
    session_binding_digest: &AppDigest,
    now: DateTime<Utc>,
) -> bool {
    let mut targets = live_stop_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    targets.retain(|_, target| target.expires_at > now);
    let Some(target) = targets.get(session_binding_digest.as_str()) else {
        return false;
    };
    target
        .cancellation
        .cancel(AppInteractiveCancellationReason::OwnerStop);
    true
}

pub(crate) fn live_interactive_session_availability(
    session_binding_digest: &AppDigest,
    now: DateTime<Utc>,
) -> AppInteractiveLiveSessionAvailability {
    let mut targets = live_stop_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    targets.retain(|_, target| target.expires_at > now);
    let Some(target) = targets.get(session_binding_digest.as_str()) else {
        return AppInteractiveLiveSessionAvailability::Unavailable;
    };
    match target.cancellation.reason() {
        None => AppInteractiveLiveSessionAvailability::Active,
        Some(AppInteractiveCancellationReason::OwnerStop) => {
            AppInteractiveLiveSessionAvailability::OwnerStopSignalled
        },
        Some(
            AppInteractiveCancellationReason::GrantRevoked
            | AppInteractiveCancellationReason::RunCancelled
            | AppInteractiveCancellationReason::Deadline,
        ) => AppInteractiveLiveSessionAvailability::Closing,
    }
}

/// Remove process-local stop acceleration only after the canonical terminal
/// receipt is durable. Durable session/audit state remains in the protected
/// workflow control store and is unaffected by this bounded-memory cleanup.
pub(crate) fn retire_live_interactive_stop_target(session_binding_digest: &AppDigest) {
    live_stop_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_binding_digest.as_str());
    live_owner_slots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|_, slot| &slot.session_binding_digest != session_binding_digest);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveResourceCeilings {
    max_sessions: u16,
    max_steps: u32,
    max_duration_seconds: u64,
    max_evidence_bytes: u64,
    max_evidence_nodes: u64,
    max_pixels: u64,
    max_artifact_bytes: u64,
    max_output_bytes: u64,
}

impl AppInteractiveResourceCeilings {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reviewed(
        max_sessions: u16,
        max_steps: u32,
        max_duration_seconds: u64,
        max_evidence_bytes: u64,
        max_evidence_nodes: u64,
        max_pixels: u64,
        max_artifact_bytes: u64,
        max_output_bytes: u64,
    ) -> Result<Self, AppInteractiveError> {
        let value = Self {
            max_sessions,
            max_steps,
            max_duration_seconds,
            max_evidence_bytes,
            max_evidence_nodes,
            max_pixels,
            max_artifact_bytes,
            max_output_bytes,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), AppInteractiveError> {
        if self.max_sessions == 0
            || self.max_sessions > MAX_APP_INTERACTIVE_SESSIONS
            || self.max_steps == 0
            || self.max_steps > MAX_APP_INTERACTIVE_STEPS
            || self.max_duration_seconds == 0
            || self.max_duration_seconds > MAX_APP_INTERACTIVE_DURATION_SECONDS
            || self.max_evidence_bytes == 0
            || self.max_evidence_bytes > MAX_APP_INTERACTIVE_EVIDENCE_BYTES
            || self.max_evidence_nodes > MAX_APP_INTERACTIVE_EVIDENCE_NODES
            || self.max_pixels > MAX_APP_INTERACTIVE_PIXELS
            || self.max_artifact_bytes > MAX_APP_INTERACTIVE_ARTIFACT_BYTES
            || self.max_output_bytes == 0
            || self.max_output_bytes > MAX_APP_INTERACTIVE_OUTPUT_BYTES
        {
            return Err(AppInteractiveError::InvalidResourceCeiling);
        }
        Ok(())
    }

    pub fn narrows(&self, requested: &Self) -> bool {
        self.validate().is_ok()
            && requested.validate().is_ok()
            && self.max_sessions <= requested.max_sessions
            && self.max_steps <= requested.max_steps
            && self.max_duration_seconds <= requested.max_duration_seconds
            && self.max_evidence_bytes <= requested.max_evidence_bytes
            && self.max_evidence_nodes <= requested.max_evidence_nodes
            && self.max_pixels <= requested.max_pixels
            && self.max_artifact_bytes <= requested.max_artifact_bytes
            && self.max_output_bytes <= requested.max_output_bytes
    }

    pub fn max_sessions(&self) -> u16 {
        self.max_sessions
    }

    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    pub fn max_duration_seconds(&self) -> u64 {
        self.max_duration_seconds
    }

    pub fn max_evidence_bytes(&self) -> u64 {
        self.max_evidence_bytes
    }

    pub fn max_evidence_nodes(&self) -> u64 {
        self.max_evidence_nodes
    }

    pub fn max_pixels(&self) -> u64 {
        self.max_pixels
    }

    pub fn max_artifact_bytes(&self) -> u64 {
        self.max_artifact_bytes
    }

    pub fn max_output_bytes(&self) -> u64 {
        self.max_output_bytes
    }
}

/// Exact action reviewed into an interactive grant. The physical adapter may
/// expose only these closed actions, never the underlying transport roster.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveReviewedAction {
    action_ref: AppReference,
    class: AppInteractiveActionClass,
    input_schema_digest: AppDigest,
    result_schema_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    result_byte_ceiling: u64,
    required_observation_kind: Option<AppInteractiveObservationKind>,
    invalidates_observation: bool,
}

impl AppInteractiveReviewedAction {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reviewed(
        action_ref: AppReference,
        class: AppInteractiveActionClass,
        input_schema_digest: AppDigest,
        result_schema_digest: AppDigest,
        owner_implementation_digest: AppDigest,
        result_byte_ceiling: u64,
        required_observation_kind: Option<AppInteractiveObservationKind>,
        invalidates_observation: bool,
    ) -> Result<Self, AppInteractiveError> {
        if result_byte_ceiling == 0 || result_byte_ceiling > MAX_APP_INTERACTIVE_OUTPUT_BYTES {
            return Err(AppInteractiveError::InvalidResourceCeiling);
        }
        if required_observation_kind.is_some() && class == AppInteractiveActionClass::Observe {
            return Err(AppInteractiveError::InvalidReviewedAction);
        }
        Ok(Self {
            action_ref,
            class,
            input_schema_digest,
            result_schema_digest,
            owner_implementation_digest,
            result_byte_ceiling,
            required_observation_kind,
            invalidates_observation,
        })
    }

    pub fn action_ref(&self) -> &AppReference {
        &self.action_ref
    }

    pub fn class(&self) -> AppInteractiveActionClass {
        self.class
    }

    pub fn input_schema_digest(&self) -> &AppDigest {
        &self.input_schema_digest
    }

    pub fn result_schema_digest(&self) -> &AppDigest {
        &self.result_schema_digest
    }

    pub fn owner_implementation_digest(&self) -> &AppDigest {
        &self.owner_implementation_digest
    }

    pub fn result_byte_ceiling(&self) -> u64 {
        self.result_byte_ceiling
    }

    pub fn requires_observation(&self) -> bool {
        self.required_observation_kind.is_some()
    }

    pub fn required_observation_kind(&self) -> Option<AppInteractiveObservationKind> {
        self.required_observation_kind
    }

    pub fn invalidates_observation(&self) -> bool {
        self.invalidates_observation
    }
}

/// Immutable install-review projection. `grant_digest` is the authority
/// owner's digest; `descriptor_digest` independently covers every interactive
/// dimension so a narrowed/reviewed descriptor cannot be reinterpreted later.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveGrantDescriptor {
    schema: &'static str,
    installation_id: AppInstallationId,
    installation_generation: u64,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    capability_grant_digest: AppDigest,
    locked_interactive_binding_digest: AppDigest,
    interactive_request_digest: AppDigest,
    policy_digest: AppDigest,
    profile: AppInteractiveExecutionProfile,
    target_policy_digest: AppDigest,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    background: AppInteractiveBackgroundPosture,
    observation_kinds: BTreeSet<AppInteractiveObservationKind>,
    actions: BTreeMap<AppReference, AppInteractiveReviewedAction>,
    resources: AppInteractiveResourceCeilings,
    revocation_epoch: u64,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    descriptor_digest: AppDigest,
}

#[allow(dead_code)] // Digest inspectors are retained for independent owner verification.
impl AppInteractiveGrantDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_reviewed(
        installation_id: AppInstallationId,
        installation_generation: u64,
        grant_revision: AppRevision,
        grant_digest: AppDigest,
        capability_grant_digest: AppDigest,
        locked_interactive_binding_digest: AppDigest,
        interactive_request_digest: AppDigest,
        policy_digest: AppDigest,
        profile: AppInteractiveExecutionProfile,
        target_policy_digest: AppDigest,
        owner_profile_digest: AppDigest,
        owner_implementation_digest: AppDigest,
        background: AppInteractiveBackgroundPosture,
        observation_kinds: BTreeSet<AppInteractiveObservationKind>,
        reviewed_actions: Vec<AppInteractiveReviewedAction>,
        resources: AppInteractiveResourceCeilings,
        revocation_epoch: u64,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppInteractiveError> {
        resources.validate()?;
        if installation_generation == 0
            || reviewed_actions.is_empty()
            || reviewed_actions.len() > MAX_APP_INTERACTIVE_ACTIONS
            || observation_kinds.is_empty()
            || expires_at <= issued_at
        {
            return Err(AppInteractiveError::InvalidGrant);
        }
        let mut actions = BTreeMap::new();
        for action in reviewed_actions {
            if action.owner_implementation_digest != owner_implementation_digest
                || action
                    .required_observation_kind
                    .is_some_and(|kind| !observation_kinds.contains(&kind))
                || actions.insert(action.action_ref.clone(), action).is_some()
            {
                return Err(AppInteractiveError::InvalidReviewedAction);
            }
        }
        let mut value = Self {
            schema: APP_INTERACTIVE_CONTRACT_V1,
            installation_id,
            installation_generation,
            grant_revision,
            grant_digest,
            capability_grant_digest,
            locked_interactive_binding_digest,
            interactive_request_digest,
            policy_digest,
            profile,
            target_policy_digest,
            owner_profile_digest,
            owner_implementation_digest,
            background,
            observation_kinds,
            actions,
            resources,
            revocation_epoch,
            issued_at,
            expires_at,
            descriptor_digest: AppDigest::blake3(b"pending-interactive-grant"),
        };
        value.descriptor_digest = value.compute_digest()?;
        Ok(value)
    }

    fn compute_digest(&self) -> Result<AppDigest, AppInteractiveError> {
        #[derive(Serialize)]
        struct DigestMaterial<'a> {
            schema: &'static str,
            installation_id: &'a AppInstallationId,
            installation_generation: u64,
            grant_revision: AppRevision,
            grant_digest: &'a AppDigest,
            capability_grant_digest: &'a AppDigest,
            locked_interactive_binding_digest: &'a AppDigest,
            interactive_request_digest: &'a AppDigest,
            policy_digest: &'a AppDigest,
            profile: AppInteractiveExecutionProfile,
            target_policy_digest: &'a AppDigest,
            owner_profile_digest: &'a AppDigest,
            owner_implementation_digest: &'a AppDigest,
            background: AppInteractiveBackgroundPosture,
            observation_kinds: &'a BTreeSet<AppInteractiveObservationKind>,
            actions: &'a BTreeMap<AppReference, AppInteractiveReviewedAction>,
            resources: &'a AppInteractiveResourceCeilings,
            revocation_epoch: u64,
            issued_at: DateTime<Utc>,
            expires_at: DateTime<Utc>,
        }
        let material = DigestMaterial {
            schema: self.schema,
            installation_id: &self.installation_id,
            installation_generation: self.installation_generation,
            grant_revision: self.grant_revision,
            grant_digest: &self.grant_digest,
            capability_grant_digest: &self.capability_grant_digest,
            locked_interactive_binding_digest: &self.locked_interactive_binding_digest,
            interactive_request_digest: &self.interactive_request_digest,
            policy_digest: &self.policy_digest,
            profile: self.profile,
            target_policy_digest: &self.target_policy_digest,
            owner_profile_digest: &self.owner_profile_digest,
            owner_implementation_digest: &self.owner_implementation_digest,
            background: self.background,
            observation_kinds: &self.observation_kinds,
            actions: &self.actions,
            resources: &self.resources,
            revocation_epoch: self.revocation_epoch,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        };
        AppDigest::blake3_canonical_json(
            &serde_json::to_value(material)
                .map_err(|error| AppInteractiveError::Encoding(error.to_string()))?,
        )
        .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
    }

    pub fn profile(&self) -> AppInteractiveExecutionProfile {
        self.profile
    }

    pub fn background(&self) -> AppInteractiveBackgroundPosture {
        self.background
    }

    pub(crate) fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub(crate) fn installation_generation(&self) -> u64 {
        self.installation_generation
    }

    pub(crate) fn grant_revision(&self) -> AppRevision {
        self.grant_revision
    }

    pub(crate) fn grant_digest(&self) -> &AppDigest {
        &self.grant_digest
    }

    pub(crate) fn capability_grant_digest(&self) -> &AppDigest {
        &self.capability_grant_digest
    }

    pub(crate) fn locked_interactive_binding_digest(&self) -> &AppDigest {
        &self.locked_interactive_binding_digest
    }

    pub(crate) fn interactive_request_digest(&self) -> &AppDigest {
        &self.interactive_request_digest
    }

    pub(crate) fn policy_digest(&self) -> &AppDigest {
        &self.policy_digest
    }

    pub fn target_policy_digest(&self) -> &AppDigest {
        &self.target_policy_digest
    }

    pub fn owner_profile_digest(&self) -> &AppDigest {
        &self.owner_profile_digest
    }

    pub fn owner_implementation_digest(&self) -> &AppDigest {
        &self.owner_implementation_digest
    }

    pub fn actions(&self) -> &BTreeMap<AppReference, AppInteractiveReviewedAction> {
        &self.actions
    }

    pub fn resources(&self) -> &AppInteractiveResourceCeilings {
        &self.resources
    }

    pub fn descriptor_digest(&self) -> &AppDigest {
        &self.descriptor_digest
    }

    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// Current, owner-resolved fence. It is not persisted or deserializable and
/// must be reconstructed after every wait before a session or action is used.
#[derive(Clone)]
pub(crate) struct AppInteractiveCurrentFence {
    installation_id: AppInstallationId,
    installation_generation: u64,
    run_ref: AppReference,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    capability_grant_digest: AppDigest,
    locked_interactive_binding_digest: AppDigest,
    interactive_request_digest: AppDigest,
    policy_digest: AppDigest,
    target_policy_digest: AppDigest,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    revocation_epoch: u64,
    active: bool,
}

impl AppInteractiveCurrentFence {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_current(
        installation_id: AppInstallationId,
        installation_generation: u64,
        run_ref: AppReference,
        grant_revision: AppRevision,
        grant_digest: AppDigest,
        capability_grant_digest: AppDigest,
        locked_interactive_binding_digest: AppDigest,
        interactive_request_digest: AppDigest,
        policy_digest: AppDigest,
        target_policy_digest: AppDigest,
        owner_profile_digest: AppDigest,
        owner_implementation_digest: AppDigest,
        revocation_epoch: u64,
        active: bool,
    ) -> Self {
        Self {
            installation_id,
            installation_generation,
            run_ref,
            grant_revision,
            grant_digest,
            capability_grant_digest,
            locked_interactive_binding_digest,
            interactive_request_digest,
            policy_digest,
            target_policy_digest,
            owner_profile_digest,
            owner_implementation_digest,
            revocation_epoch,
            active,
        }
    }

    fn matches_grant(&self, grant: &AppInteractiveGrantDescriptor) -> bool {
        self.active
            && self.installation_id == grant.installation_id
            && self.installation_generation == grant.installation_generation
            && self.grant_revision == grant.grant_revision
            && self.grant_digest == grant.grant_digest
            && self.capability_grant_digest == grant.capability_grant_digest
            && self.locked_interactive_binding_digest == grant.locked_interactive_binding_digest
            && self.interactive_request_digest == grant.interactive_request_digest
            && self.policy_digest == grant.policy_digest
            && self.target_policy_digest == grant.target_policy_digest
            && self.owner_profile_digest == grant.owner_profile_digest
            && self.owner_implementation_digest == grant.owner_implementation_digest
            && self.revocation_epoch == grant.revocation_epoch
    }

    pub(crate) fn run_ref(&self) -> &AppReference {
        &self.run_ref
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveGeometry {
    pub width: u32,
    pub height: u32,
    pub scale_millis: u32,
}

impl AppInteractiveGeometry {
    pub(crate) fn validate(&self) -> Result<(), AppInteractiveError> {
        let pixels = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or(AppInteractiveError::ResourceCeilingExceeded)?;
        if self.width == 0
            || self.height == 0
            || self.scale_millis == 0
            || pixels > MAX_APP_INTERACTIVE_PIXELS
        {
            return Err(AppInteractiveError::InvalidObservation);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveResourceClaim {
    pub evidence_bytes: u64,
    pub evidence_nodes: u64,
    pub pixels: u64,
    pub artifact_bytes: u64,
    pub output_bytes: u64,
}

impl AppInteractiveResourceClaim {
    fn checked_add(&self, other: &Self) -> Option<Self> {
        Some(Self {
            evidence_bytes: self.evidence_bytes.checked_add(other.evidence_bytes)?,
            evidence_nodes: self.evidence_nodes.checked_add(other.evidence_nodes)?,
            pixels: self.pixels.checked_add(other.pixels)?,
            artifact_bytes: self.artifact_bytes.checked_add(other.artifact_bytes)?,
            output_bytes: self.output_bytes.checked_add(other.output_bytes)?,
        })
    }

    fn within(&self, limits: &AppInteractiveResourceCeilings) -> bool {
        self.evidence_bytes <= limits.max_evidence_bytes
            && self.evidence_nodes <= limits.max_evidence_nodes
            && self.pixels <= limits.max_pixels
            && self.artifact_bytes <= limits.max_artifact_bytes
            && self.output_bytes <= limits.max_output_bytes
    }
}

/// Opaque run-owned session. The physical session ID/profile/device/window is
/// held by the owner adapter; this handle retains only its sealed identity.
pub(crate) struct AppInteractiveSessionHandle {
    installation_id: AppInstallationId,
    installation_generation: u64,
    run_ref: AppReference,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    capability_grant_digest: AppDigest,
    locked_interactive_binding_digest: AppDigest,
    interactive_request_digest: AppDigest,
    policy_digest: AppDigest,
    grant_descriptor_digest: AppDigest,
    profile: AppInteractiveExecutionProfile,
    target_policy_digest: AppDigest,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    resource_lease_ref: AppReference,
    session_ordinal: u16,
    acquired_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    session_binding_digest: AppDigest,
    next_action_sequence: u64,
    next_observation_sequence: u64,
    state_generation: u64,
    consumed: AppInteractiveResourceClaim,
    cancellation: AppInteractiveCancellation,
}

#[allow(dead_code)] // Target inspectors remain part of the sealed owner contract.
impl AppInteractiveSessionHandle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acquire(
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        owner_target_ref: AppReference,
        owner_target_digest: AppDigest,
        resource_lease_ref: AppReference,
        session_ordinal: u16,
        cancellation: AppInteractiveCancellation,
        now: DateTime<Utc>,
        requested_expires_at: DateTime<Utc>,
    ) -> Result<Self, AppInteractiveError> {
        if !current.matches_grant(grant)
            || now < grant.issued_at
            || now >= grant.expires_at
            || session_ordinal == 0
            || session_ordinal > grant.resources.max_sessions
            || cancellation.is_cancelled()
        {
            return Err(AppInteractiveError::StaleAuthority);
        }
        let duration_seconds = i64::try_from(grant.resources.max_duration_seconds)
            .map_err(|_| AppInteractiveError::InvalidResourceCeiling)?;
        let duration_deadline = now
            .checked_add_signed(Duration::seconds(duration_seconds))
            .ok_or(AppInteractiveError::InvalidResourceCeiling)?;
        let expires_at = requested_expires_at
            .min(grant.expires_at)
            .min(duration_deadline);
        if expires_at <= now {
            return Err(AppInteractiveError::StaleAuthority);
        }
        let session_binding_digest = session_digest(
            grant,
            &current.run_ref,
            &owner_target_ref,
            &owner_target_digest,
            &resource_lease_ref,
            session_ordinal,
            now,
            expires_at,
        )?;
        Ok(Self {
            installation_id: grant.installation_id.clone(),
            installation_generation: grant.installation_generation,
            run_ref: current.run_ref.clone(),
            grant_revision: grant.grant_revision,
            grant_digest: grant.grant_digest.clone(),
            capability_grant_digest: grant.capability_grant_digest.clone(),
            locked_interactive_binding_digest: grant.locked_interactive_binding_digest.clone(),
            interactive_request_digest: grant.interactive_request_digest.clone(),
            policy_digest: grant.policy_digest.clone(),
            grant_descriptor_digest: grant.descriptor_digest.clone(),
            profile: grant.profile,
            target_policy_digest: grant.target_policy_digest.clone(),
            owner_profile_digest: grant.owner_profile_digest.clone(),
            owner_implementation_digest: grant.owner_implementation_digest.clone(),
            owner_target_ref,
            owner_target_digest,
            resource_lease_ref,
            session_ordinal,
            acquired_at: now,
            expires_at,
            session_binding_digest,
            next_action_sequence: 1,
            next_observation_sequence: 1,
            state_generation: 1,
            consumed: AppInteractiveResourceClaim::default(),
            cancellation,
        })
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn owner_target_ref(&self) -> &AppReference {
        &self.owner_target_ref
    }

    pub(crate) fn owner_target_digest(&self) -> &AppDigest {
        &self.owner_target_digest
    }

    pub(crate) fn binding_digest(&self) -> &AppDigest {
        &self.session_binding_digest
    }

    /// Rotate one run-owned physical owner onto another independently reviewed
    /// leaf grant without changing its opaque physical target. This is the
    /// only cross-alias bridge: the new current fence must be exact, all owner
    /// and target identities must match, and prior action permits remain bound
    /// to their old session digest. Owners must reissue any still-fresh raw
    /// observation under the returned binding before authorizing the new leaf.
    pub(crate) fn rebind_exact_leaf(
        &mut self,
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        now: DateTime<Utc>,
    ) -> Result<(), AppInteractiveError> {
        if self.installation_id != grant.installation_id
            || self.installation_generation != grant.installation_generation
            || self.run_ref != current.run_ref
            || self.profile != grant.profile
            || self.target_policy_digest != grant.target_policy_digest
            || self.owner_profile_digest != grant.owner_profile_digest
            || self.cancellation.is_cancelled()
            || now >= self.expires_at
        {
            return Err(AppInteractiveError::StaleAuthority);
        }
        let rebound = Self::acquire(
            grant,
            current,
            self.owner_target_ref.clone(),
            self.owner_target_digest.clone(),
            self.resource_lease_ref.clone(),
            self.session_ordinal,
            self.cancellation.clone(),
            now,
            self.expires_at,
        )?;
        *self = rebound;
        Ok(())
    }

    pub(crate) fn issue_observation(
        &mut self,
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        kind: AppInteractiveObservationKind,
        geometry: AppInteractiveGeometry,
        content_digest: AppDigest,
        labels_digest: AppDigest,
        evidence_bytes: u64,
        evidence_nodes: u64,
        observed_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<AppInteractiveObservationRef, AppInteractiveError> {
        self.validate_current(grant, current, observed_at)?;
        geometry.validate()?;
        if !grant.observation_kinds.contains(&kind)
            || expires_at <= observed_at
            || expires_at > self.expires_at
        {
            return Err(AppInteractiveError::InvalidObservation);
        }
        let pixels = if kind == AppInteractiveObservationKind::Pixels {
            u64::from(geometry.width)
                .checked_mul(u64::from(geometry.height))
                .ok_or(AppInteractiveError::ResourceCeilingExceeded)?
        } else {
            0
        };
        let claim = AppInteractiveResourceClaim {
            evidence_bytes,
            evidence_nodes,
            pixels,
            artifact_bytes: 0,
            output_bytes: 0,
        };
        // The action which produced this observation already reserved and
        // charged its reviewed worst-case evidence claim. Re-charging the
        // materialized owner observation would double-count one physical read.
        if !claim.within(&grant.resources) {
            return Err(AppInteractiveError::ResourceCeilingExceeded);
        }
        let sequence = self.next_observation_sequence;
        self.next_observation_sequence = self
            .next_observation_sequence
            .checked_add(1)
            .ok_or(AppInteractiveError::ResourceCeilingExceeded)?;
        let observation_digest = observation_digest(
            &self.session_binding_digest,
            &self.owner_target_ref,
            &self.owner_target_digest,
            sequence,
            self.state_generation,
            kind,
            &geometry,
            &content_digest,
            &labels_digest,
            evidence_bytes,
            evidence_nodes,
            observed_at,
            expires_at,
        )?;
        let observation_ref = AppReference::parse(format!(
            "interactive-observation:{}",
            digest_hex(&observation_digest)?
        ))
        .map_err(|_| AppInteractiveError::Encoding("invalid observation identity".to_owned()))?;
        Ok(AppInteractiveObservationRef {
            observation_ref,
            observation_digest,
            session_binding_digest: self.session_binding_digest.clone(),
            owner_target_ref: self.owner_target_ref.clone(),
            owner_target_digest: self.owner_target_digest.clone(),
            sequence,
            state_generation: self.state_generation,
            kind,
            geometry,
            content_digest,
            labels_digest,
            evidence_bytes,
            evidence_nodes,
            observed_at,
            expires_at,
        })
    }

    pub(crate) fn authorize_action(
        &mut self,
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        action_ref: &AppReference,
        canonical_input: &[u8],
        claim: AppInteractiveResourceClaim,
        observation: Option<AppInteractiveObservationRef>,
        now: DateTime<Utc>,
    ) -> Result<AppInteractiveActionPermit, AppInteractiveError> {
        self.validate_current(grant, current, now)?;
        if canonical_input.is_empty() || canonical_input.len() > MAX_APP_INTERACTIVE_INPUT_BYTES {
            return Err(AppInteractiveError::InvalidInput);
        }
        let action = grant
            .actions
            .get(action_ref)
            .ok_or(AppInteractiveError::ActionNotGranted)?;
        if claim.output_bytes > action.result_byte_ceiling {
            return Err(AppInteractiveError::ResourceCeilingExceeded);
        }
        let (observation_ref, observation_digest) =
            match (action.required_observation_kind, observation) {
                (Some(required_kind), Some(observation)) => {
                    observation.validate_for(self, now)?;
                    if observation.kind != required_kind {
                        return Err(AppInteractiveError::ObservationRequired);
                    }
                    (
                        Some(observation.observation_ref),
                        Some(observation.observation_digest),
                    )
                },
                (Some(_), None) | (None, Some(_)) => {
                    return Err(AppInteractiveError::ObservationRequired)
                },
                (None, None) => (None, None),
            };
        let sequence = self.next_action_sequence;
        if sequence > u64::from(grant.resources.max_steps) {
            return Err(AppInteractiveError::ResourceCeilingExceeded);
        }
        self.charge(grant, &claim)?;
        self.next_action_sequence = self
            .next_action_sequence
            .checked_add(1)
            .ok_or(AppInteractiveError::ResourceCeilingExceeded)?;
        let input_bytes =
            u64::try_from(canonical_input.len()).map_err(|_| AppInteractiveError::InvalidInput)?;
        let input_digest = AppDigest::blake3(canonical_input);
        let permit_digest = action_permit_digest(
            &self.session_binding_digest,
            self.grant_revision,
            &self.grant_digest,
            &self.capability_grant_digest,
            &self.locked_interactive_binding_digest,
            &self.interactive_request_digest,
            &self.policy_digest,
            &self.grant_descriptor_digest,
            &self.owner_target_ref,
            &self.owner_target_digest,
            action,
            sequence,
            &input_digest,
            input_bytes,
            observation_ref.as_ref(),
            observation_digest.as_ref(),
            &claim,
            now,
            self.expires_at,
        )?;
        if action.invalidates_observation {
            self.state_generation = self
                .state_generation
                .checked_add(1)
                .ok_or(AppInteractiveError::ResourceCeilingExceeded)?;
        }
        Ok(AppInteractiveActionPermit {
            installation_id: self.installation_id.clone(),
            installation_generation: self.installation_generation,
            run_ref: self.run_ref.clone(),
            grant_revision: self.grant_revision,
            grant_digest: self.grant_digest.clone(),
            capability_grant_digest: self.capability_grant_digest.clone(),
            locked_interactive_binding_digest: self.locked_interactive_binding_digest.clone(),
            interactive_request_digest: self.interactive_request_digest.clone(),
            policy_digest: self.policy_digest.clone(),
            grant_descriptor_digest: self.grant_descriptor_digest.clone(),
            profile: self.profile,
            target_policy_digest: self.target_policy_digest.clone(),
            owner_profile_digest: self.owner_profile_digest.clone(),
            owner_implementation_digest: self.owner_implementation_digest.clone(),
            owner_target_ref: self.owner_target_ref.clone(),
            owner_target_digest: self.owner_target_digest.clone(),
            session_binding_digest: self.session_binding_digest.clone(),
            resource_lease_ref: self.resource_lease_ref.clone(),
            session_ordinal: self.session_ordinal,
            action: action.clone(),
            sequence,
            input_digest,
            input_bytes,
            observation_ref,
            observation_digest,
            claim,
            admitted_at: now,
            expires_at: self.expires_at,
            permit_digest,
            cancellation: self.cancellation.clone(),
        })
    }

    fn validate_current(
        &self,
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
        now: DateTime<Utc>,
    ) -> Result<(), AppInteractiveError> {
        if !current.matches_grant(grant)
            || self.installation_id != current.installation_id
            || self.installation_generation != current.installation_generation
            || self.run_ref != current.run_ref
            || self.grant_descriptor_digest != grant.descriptor_digest
            || self.profile != grant.profile
            || self.target_policy_digest != grant.target_policy_digest
            || self.owner_profile_digest != grant.owner_profile_digest
            || self.owner_implementation_digest != grant.owner_implementation_digest
            || now < self.acquired_at
            || now >= self.expires_at
            || self.cancellation.is_cancelled()
        {
            return Err(AppInteractiveError::StaleAuthority);
        }
        Ok(())
    }

    fn charge(
        &mut self,
        grant: &AppInteractiveGrantDescriptor,
        claim: &AppInteractiveResourceClaim,
    ) -> Result<(), AppInteractiveError> {
        let next = self
            .consumed
            .checked_add(claim)
            .ok_or(AppInteractiveError::ResourceCeilingExceeded)?;
        if !next.within(&grant.resources) {
            return Err(AppInteractiveError::ResourceCeilingExceeded);
        }
        self.consumed = next;
        Ok(())
    }
}

/// Opaque one-use evidence identity. Raw DOM/AX/device-tree bytes are retained
/// by their physical owner and never travel in this ref.
pub(crate) struct AppInteractiveObservationRef {
    observation_ref: AppReference,
    observation_digest: AppDigest,
    session_binding_digest: AppDigest,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    #[allow(dead_code)] // Monotonic evidence retained in the sealed identity.
    sequence: u64,
    state_generation: u64,
    kind: AppInteractiveObservationKind,
    geometry: AppInteractiveGeometry,
    content_digest: AppDigest,
    labels_digest: AppDigest,
    evidence_bytes: u64,
    evidence_nodes: u64,
    observed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[allow(dead_code)] // Opaque evidence inspectors remain available to physical owners.
impl AppInteractiveObservationRef {
    fn validate_for(
        &self,
        session: &AppInteractiveSessionHandle,
        now: DateTime<Utc>,
    ) -> Result<(), AppInteractiveError> {
        if self.session_binding_digest != session.session_binding_digest
            || self.owner_target_ref != session.owner_target_ref
            || self.owner_target_digest != session.owner_target_digest
            || self.state_generation != session.state_generation
            || now < self.observed_at
            || now >= self.expires_at
            || self.evidence_bytes == 0
        {
            return Err(AppInteractiveError::StaleObservation);
        }
        Ok(())
    }

    pub(crate) fn observation_ref(&self) -> &AppReference {
        &self.observation_ref
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn kind(&self) -> AppInteractiveObservationKind {
        self.kind
    }

    pub(crate) fn geometry(&self) -> &AppInteractiveGeometry {
        &self.geometry
    }

    pub(crate) fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub(crate) fn labels_digest(&self) -> &AppDigest {
        &self.labels_digest
    }

    pub(crate) fn evidence_nodes(&self) -> u64 {
        self.evidence_nodes
    }

    pub(crate) fn evidence_bytes(&self) -> u64 {
        self.evidence_bytes
    }

    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// One reviewed owner action. It is first joined to the common effect binding
/// before durable dispatch-start, then consumed with that effect's one-shot
/// provider token. This prevents a browser/host/device adapter from becoming a
/// second executor or bypassing common settlement.
pub(crate) struct AppInteractiveActionPermit {
    installation_id: AppInstallationId,
    installation_generation: u64,
    run_ref: AppReference,
    grant_revision: AppRevision,
    grant_digest: AppDigest,
    capability_grant_digest: AppDigest,
    locked_interactive_binding_digest: AppDigest,
    interactive_request_digest: AppDigest,
    policy_digest: AppDigest,
    grant_descriptor_digest: AppDigest,
    profile: AppInteractiveExecutionProfile,
    target_policy_digest: AppDigest,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    owner_target_ref: AppReference,
    owner_target_digest: AppDigest,
    session_binding_digest: AppDigest,
    resource_lease_ref: AppReference,
    #[allow(dead_code)] // Retained in the sealed action identity.
    session_ordinal: u16,
    action: AppInteractiveReviewedAction,
    sequence: u64,
    input_digest: AppDigest,
    input_bytes: u64,
    observation_ref: Option<AppReference>,
    observation_digest: Option<AppDigest>,
    claim: AppInteractiveResourceClaim,
    admitted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    permit_digest: AppDigest,
    cancellation: AppInteractiveCancellation,
}

#[allow(dead_code)] // Permit inspectors remain available to physical owner adapters.
impl AppInteractiveActionPermit {
    pub(crate) fn bind_effect(
        self,
        binding: &AppEffectBinding,
        canonical_input: &[u8],
        now: DateTime<Utc>,
    ) -> Result<AppInteractiveEffectPermit, AppInteractiveError> {
        if self.cancellation.is_cancelled()
            || now < self.admitted_at
            || now >= self.expires_at
            || binding.action_ref() != &self.action.action_ref
            || binding.physical_target_ref() != &self.owner_target_ref
            || binding.result_byte_ceiling() != self.action.result_byte_ceiling
            || !binding.matches_input_at(canonical_input, &now)
            || self.input_digest != AppDigest::blake3(canonical_input)
            || usize::try_from(self.input_bytes).ok() != Some(canonical_input.len())
        {
            return Err(AppInteractiveError::EffectBindingMismatch);
        }
        Ok(AppInteractiveEffectPermit {
            permit: self,
            effect_binding_digest: binding.binding_digest().clone(),
        })
    }

    pub(crate) fn profile(&self) -> AppInteractiveExecutionProfile {
        self.profile
    }

    pub(crate) fn action(&self) -> &AppInteractiveReviewedAction {
        &self.action
    }

    pub(crate) fn owner_target_ref(&self) -> &AppReference {
        &self.owner_target_ref
    }

    pub(crate) fn owner_target_digest(&self) -> &AppDigest {
        &self.owner_target_digest
    }

    pub(crate) fn session_binding_digest(&self) -> &AppDigest {
        &self.session_binding_digest
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn permit_digest(&self) -> &AppDigest {
        &self.permit_digest
    }

    pub(crate) fn cancellation_reason(&self) -> Option<AppInteractiveCancellationReason> {
        self.cancellation.reason()
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

pub(crate) struct AppInteractiveEffectPermit {
    permit: AppInteractiveActionPermit,
    effect_binding_digest: AppDigest,
}

#[allow(dead_code)] // Cancellation inspection remains available to owner adapters.
impl AppInteractiveEffectPermit {
    pub(crate) fn inspection(&self) -> AppInteractiveEffectInspection {
        AppInteractiveEffectInspection {
            profile: self.permit.profile,
            capability_grant_digest: self.permit.capability_grant_digest.clone(),
            locked_interactive_binding_digest: self
                .permit
                .locked_interactive_binding_digest
                .clone(),
            interactive_request_digest: self.permit.interactive_request_digest.clone(),
            owner_target_digest: self.permit.owner_target_digest.clone(),
            session_binding_digest: self.permit.session_binding_digest.clone(),
            action_ref: self.permit.action.action_ref.clone(),
            action_class: self.permit.action.class,
            sequence: self.permit.sequence,
            claim: self.permit.claim.clone(),
            expires_at: self.permit.expires_at.clone(),
            cancellation: self.permit.cancellation.clone(),
        }
    }

    pub(crate) fn start_owner_io(
        self,
        authorization: AppEffectProviderIoAuthorization,
        now: DateTime<Utc>,
    ) -> Result<AppInteractiveOwnerIoPermit, AppInteractiveOwnerIoStartFailure> {
        let valid = !self.permit.cancellation.is_cancelled()
            && now >= self.permit.admitted_at
            && now < self.permit.expires_at
            && authorization.binding().binding_digest() == &self.effect_binding_digest
            && authorization.binding().action_ref() == &self.permit.action.action_ref
            && authorization.binding().physical_target_ref() == &self.permit.owner_target_ref;
        drop(authorization);
        let permit = AppInteractiveOwnerIoPermit {
            permit: self.permit,
            effect_binding_digest: self.effect_binding_digest,
        };
        if valid {
            Ok(permit)
        } else {
            Err(AppInteractiveOwnerIoStartFailure {
                error: AppInteractiveError::EffectBindingMismatch,
                permit,
            })
        }
    }

    /// The cancellation handle, delegated like `cancellation_reason` above.
    /// A holder of an effect permit has to be able to cancel it, not only to
    /// ask afterwards why it was cancelled.
    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.permit.cancellation()
    }

    pub(crate) fn cancellation_reason(&self) -> Option<AppInteractiveCancellationReason> {
        self.permit.cancellation_reason()
    }

    pub(crate) fn outcome_uncertain(
        self,
    ) -> Result<AppInteractiveSettlementReceipt, AppInteractiveError> {
        AppInteractiveSettlementReceipt::outcome_uncertain(AppInteractiveOwnerIoPermit {
            permit: self.permit,
            effect_binding_digest: self.effect_binding_digest,
        })
    }
}

/// A provider-start rejection after the common one-shot authorization was
/// consumed. The retained permit can only be settled uncertain; it cannot be
/// retried or converted back into pre-I/O authority.
pub(crate) struct AppInteractiveOwnerIoStartFailure {
    error: AppInteractiveError,
    permit: AppInteractiveOwnerIoPermit,
}

impl AppInteractiveOwnerIoStartFailure {
    pub(crate) fn into_parts(self) -> (AppInteractiveError, AppInteractiveOwnerIoPermit) {
        (self.error, self.permit)
    }
}

/// Sole owner-side proof that the common effect path authorized provider I/O.
/// Physical adapters consume it immediately before their first poll.
pub(crate) struct AppInteractiveOwnerIoPermit {
    permit: AppInteractiveActionPermit,
    effect_binding_digest: AppDigest,
}

#[allow(dead_code)] // Digest/sequence inspectors remain part of the move-only owner seam.
impl AppInteractiveOwnerIoPermit {
    pub(crate) fn installation_id(&self) -> &AppInstallationId {
        &self.permit.installation_id
    }

    pub(crate) fn installation_generation(&self) -> u64 {
        self.permit.installation_generation
    }

    pub(crate) fn run_ref(&self) -> &AppReference {
        &self.permit.run_ref
    }

    pub(crate) fn grant_revision(&self) -> AppRevision {
        self.permit.grant_revision
    }

    pub(crate) fn grant_digest(&self) -> &AppDigest {
        &self.permit.grant_digest
    }

    pub(crate) fn capability_grant_digest(&self) -> &AppDigest {
        &self.permit.capability_grant_digest
    }

    pub(crate) fn locked_interactive_binding_digest(&self) -> &AppDigest {
        &self.permit.locked_interactive_binding_digest
    }

    pub(crate) fn interactive_request_digest(&self) -> &AppDigest {
        &self.permit.interactive_request_digest
    }

    pub(crate) fn policy_digest(&self) -> &AppDigest {
        &self.permit.policy_digest
    }

    pub(crate) fn grant_descriptor_digest(&self) -> &AppDigest {
        &self.permit.grant_descriptor_digest
    }

    pub(crate) fn action(&self) -> &AppInteractiveReviewedAction {
        &self.permit.action
    }

    pub(crate) fn profile(&self) -> AppInteractiveExecutionProfile {
        self.permit.profile
    }

    pub(crate) fn owner_target_ref(&self) -> &AppReference {
        &self.permit.owner_target_ref
    }

    pub(crate) fn owner_target_digest(&self) -> &AppDigest {
        &self.permit.owner_target_digest
    }

    pub(crate) fn target_policy_digest(&self) -> &AppDigest {
        &self.permit.target_policy_digest
    }

    pub(crate) fn owner_profile_digest(&self) -> &AppDigest {
        &self.permit.owner_profile_digest
    }

    pub(crate) fn owner_implementation_digest(&self) -> &AppDigest {
        &self.permit.owner_implementation_digest
    }

    pub(crate) fn session_binding_digest(&self) -> &AppDigest {
        &self.permit.session_binding_digest
    }

    pub(crate) fn effect_binding_digest(&self) -> &AppDigest {
        &self.effect_binding_digest
    }

    pub(crate) fn input_digest(&self) -> &AppDigest {
        &self.permit.input_digest
    }

    pub(crate) fn input_bytes(&self) -> u64 {
        self.permit.input_bytes
    }

    pub(crate) fn observation_digest(&self) -> Option<&AppDigest> {
        self.permit.observation_digest.as_ref()
    }

    pub(crate) fn observation_ref(&self) -> Option<&AppReference> {
        self.permit.observation_ref.as_ref()
    }

    pub(crate) fn permit_digest(&self) -> &AppDigest {
        &self.permit.permit_digest
    }

    pub(crate) fn resource_lease_ref(&self) -> &AppReference {
        &self.permit.resource_lease_ref
    }

    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.permit.expires_at
    }

    pub(crate) fn sequence(&self) -> u64 {
        self.permit.sequence
    }

    pub(crate) fn claim(&self) -> &AppInteractiveResourceClaim {
        &self.permit.claim
    }

    pub(crate) fn cancellation_reason(&self) -> Option<AppInteractiveCancellationReason> {
        self.permit.cancellation.reason()
    }

    pub(crate) fn cancellation(&self) -> AppInteractiveCancellation {
        self.permit.cancellation.clone()
    }

    pub(crate) fn correlation_ref(&self) -> Result<AppReference, AppInteractiveError> {
        AppReference::parse(format!(
            "interactive-action:{}",
            digest_hex(&self.permit.permit_digest)?
        ))
        .map_err(|_| AppInteractiveError::Encoding("invalid action correlation".to_owned()))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInteractiveTerminalClass {
    Completed,
    CancelledBeforeIo,
    OutcomeUncertain,
}

/// Payload-minimal owner receipt. `CancelledBeforeIo` is produced only by the
/// outer common-effect abort path; owner adapters that hold an I/O permit may
/// return only `Completed` or `OutcomeUncertain`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveSettlementReceipt {
    correlation_ref: AppReference,
    effect_binding_digest: AppDigest,
    session_binding_digest: AppDigest,
    owner_target_ref: AppReference,
    action_ref: AppReference,
    sequence: u64,
    terminal: AppInteractiveTerminalClass,
    result_digest: Option<AppDigest>,
    result_bytes: u64,
    evidence_digest: Option<AppDigest>,
    evidence_bytes: u64,
}

impl AppInteractiveSettlementReceipt {
    pub(crate) fn preflight_completed(
        permit: &AppInteractiveOwnerIoPermit,
        result_bytes: &[u8],
        evidence_digest: Option<&AppDigest>,
        evidence_bytes: u64,
    ) -> Result<(), AppInteractiveError> {
        let result_len =
            u64::try_from(result_bytes.len()).map_err(|_| AppInteractiveError::ResultTooLarge)?;
        if result_len > permit.action().result_byte_ceiling
            || result_len > permit.claim().output_bytes
            || evidence_bytes > permit.claim().evidence_bytes
            || evidence_digest.is_some() != (evidence_bytes > 0)
        {
            return Err(AppInteractiveError::ResultTooLarge);
        }
        permit.correlation_ref()?;
        Ok(())
    }

    pub(crate) fn completed(
        permit: AppInteractiveOwnerIoPermit,
        result_bytes: &[u8],
        evidence_digest: Option<AppDigest>,
        evidence_bytes: u64,
    ) -> Result<Self, AppInteractiveError> {
        Self::preflight_completed(
            &permit,
            result_bytes,
            evidence_digest.as_ref(),
            evidence_bytes,
        )?;
        let result_len =
            u64::try_from(result_bytes.len()).map_err(|_| AppInteractiveError::ResultTooLarge)?;
        let correlation_ref = permit.correlation_ref()?;
        Ok(Self {
            correlation_ref,
            effect_binding_digest: permit.effect_binding_digest,
            session_binding_digest: permit.permit.session_binding_digest,
            owner_target_ref: permit.permit.owner_target_ref,
            action_ref: permit.permit.action.action_ref,
            sequence: permit.permit.sequence,
            terminal: AppInteractiveTerminalClass::Completed,
            result_digest: Some(AppDigest::blake3(result_bytes)),
            result_bytes: result_len,
            evidence_digest,
            evidence_bytes,
        })
    }

    pub(crate) fn outcome_uncertain(
        permit: AppInteractiveOwnerIoPermit,
    ) -> Result<Self, AppInteractiveError> {
        let correlation_ref = permit.correlation_ref()?;
        Ok(Self {
            correlation_ref,
            effect_binding_digest: permit.effect_binding_digest,
            session_binding_digest: permit.permit.session_binding_digest,
            owner_target_ref: permit.permit.owner_target_ref,
            action_ref: permit.permit.action.action_ref,
            sequence: permit.permit.sequence,
            terminal: AppInteractiveTerminalClass::OutcomeUncertain,
            result_digest: None,
            result_bytes: 0,
            evidence_digest: None,
            evidence_bytes: 0,
        })
    }

    pub fn terminal(&self) -> AppInteractiveTerminalClass {
        self.terminal
    }

    pub fn correlation_ref(&self) -> &AppReference {
        &self.correlation_ref
    }

    pub fn session_binding_digest(&self) -> &AppDigest {
        &self.session_binding_digest
    }

    pub fn action_ref(&self) -> &AppReference {
        &self.action_ref
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn effect_binding_digest(&self) -> &AppDigest {
        &self.effect_binding_digest
    }

    pub fn result_digest(&self) -> Option<&AppDigest> {
        self.result_digest.as_ref()
    }

    pub fn result_bytes(&self) -> u64 {
        self.result_bytes
    }

    pub fn evidence_digest(&self) -> Option<&AppDigest> {
        self.evidence_digest.as_ref()
    }

    pub fn evidence_bytes(&self) -> u64 {
        self.evidence_bytes
    }
}

#[allow(clippy::too_many_arguments)]
fn session_digest(
    grant: &AppInteractiveGrantDescriptor,
    run_ref: &AppReference,
    owner_target_ref: &AppReference,
    owner_target_digest: &AppDigest,
    resource_lease_ref: &AppReference,
    session_ordinal: u16,
    acquired_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<AppDigest, AppInteractiveError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": APP_INTERACTIVE_CONTRACT_V1,
        "grant_descriptor_digest": grant.descriptor_digest(),
        "run_ref": run_ref,
        "profile": grant.profile(),
        "owner_target_ref": owner_target_ref,
        "owner_target_digest": owner_target_digest,
        "resource_lease_ref": resource_lease_ref,
        "session_ordinal": session_ordinal,
        "acquired_at": acquired_at,
        "expires_at": expires_at,
    }))
    .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn observation_digest(
    session_binding_digest: &AppDigest,
    owner_target_ref: &AppReference,
    owner_target_digest: &AppDigest,
    sequence: u64,
    state_generation: u64,
    kind: AppInteractiveObservationKind,
    geometry: &AppInteractiveGeometry,
    content_digest: &AppDigest,
    labels_digest: &AppDigest,
    evidence_bytes: u64,
    evidence_nodes: u64,
    observed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<AppDigest, AppInteractiveError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": APP_INTERACTIVE_CONTRACT_V1,
        "session_binding_digest": session_binding_digest,
        "owner_target_ref": owner_target_ref,
        "owner_target_digest": owner_target_digest,
        "sequence": sequence,
        "state_generation": state_generation,
        "kind": kind,
        "geometry": geometry,
        "content_digest": content_digest,
        "labels_digest": labels_digest,
        "evidence_bytes": evidence_bytes,
        "evidence_nodes": evidence_nodes,
        "observed_at": observed_at,
        "expires_at": expires_at,
    }))
    .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn action_permit_digest(
    session_binding_digest: &AppDigest,
    grant_revision: AppRevision,
    grant_digest: &AppDigest,
    capability_grant_digest: &AppDigest,
    locked_interactive_binding_digest: &AppDigest,
    interactive_request_digest: &AppDigest,
    policy_digest: &AppDigest,
    grant_descriptor_digest: &AppDigest,
    owner_target_ref: &AppReference,
    owner_target_digest: &AppDigest,
    action: &AppInteractiveReviewedAction,
    sequence: u64,
    input_digest: &AppDigest,
    input_bytes: u64,
    observation_ref: Option<&AppReference>,
    observation_digest: Option<&AppDigest>,
    claim: &AppInteractiveResourceClaim,
    admitted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<AppDigest, AppInteractiveError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": APP_INTERACTIVE_CONTRACT_V1,
        "session_binding_digest": session_binding_digest,
        "grant_revision": grant_revision,
        "grant_digest": grant_digest,
        "capability_grant_digest": capability_grant_digest,
        "locked_interactive_binding_digest": locked_interactive_binding_digest,
        "interactive_request_digest": interactive_request_digest,
        "policy_digest": policy_digest,
        "grant_descriptor_digest": grant_descriptor_digest,
        "owner_target_ref": owner_target_ref,
        "owner_target_digest": owner_target_digest,
        "action": action,
        "sequence": sequence,
        "input_digest": input_digest,
        "input_bytes": input_bytes,
        "observation_ref": observation_ref,
        "observation_digest": observation_digest,
        "claim": claim,
        "admitted_at": admitted_at,
        "expires_at": expires_at,
    }))
    .map_err(|error| AppInteractiveError::Encoding(error.to_string()))
}

fn digest_hex(digest: &AppDigest) -> Result<&str, AppInteractiveError> {
    digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or_else(|| AppInteractiveError::Encoding("invalid canonical digest".to_owned()))
}

#[derive(Debug, Error)]
pub enum AppInteractiveError {
    #[error("interactive capability request is malformed")]
    InvalidCapabilityRequest,
    #[error("interactive capability request names physical breadth that is not admitted")]
    CapabilityRequestNotAdmitted,
    #[error("interactive grant is incomplete, stale or malformed")]
    InvalidGrant,
    #[error("interactive action descriptor is inconsistent with its owner")]
    InvalidReviewedAction,
    #[error("interactive resource ceiling is invalid")]
    InvalidResourceCeiling,
    #[error("interactive authority, policy or session is no longer current")]
    StaleAuthority,
    #[error("interactive action is not present in the reviewed grant")]
    ActionNotGranted,
    #[error("interactive action input is empty or exceeds its bound")]
    InvalidInput,
    #[error("interactive action requires one fresh owner observation")]
    ObservationRequired,
    #[error("interactive observation is invalid")]
    InvalidObservation,
    #[error("interactive observation is stale or belongs to another owner/session")]
    StaleObservation,
    #[error("interactive resource claim exceeds the reviewed ceiling")]
    ResourceCeilingExceeded,
    #[error("interactive permit does not match the common effect binding")]
    EffectBindingMismatch,
    #[error("interactive result exceeds the reviewed action/resource bound")]
    ResultTooLarge,
    #[error("failed to encode interactive identity: {0}")]
    Encoding(String),
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 22, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    #[test]
    fn capability_request_binds_every_review_dimension_and_denies_future_breadth() {
        let request =
            AppInteractiveCapabilityRequest::legacy_observe(AppInteractiveOwnerKind::Browser, 4096)
                .unwrap();
        let baseline = request.request_digest().unwrap();

        let mut origin = request.clone();
        origin.allowed_origins = BTreeSet::from(["https://example.test".to_owned()]);
        assert_ne!(origin.request_digest().unwrap(), baseline);

        let mut profile = request.clone();
        profile.target_profile_class = AppInteractiveTargetProfileClass::OwnerReviewedMacosPairing;
        assert!(profile.validate_for_admission().is_err());

        let mut target = request.clone();
        target.target_selectors.current_reviewed_pairing = true;
        assert!(target.validate_for_admission().is_err());

        let mut action = request.clone();
        action
            .action_classes
            .insert(AppInteractiveActionClass::Interact);
        assert!(matches!(
            action.validate_for_admission(),
            Err(AppInteractiveError::CapabilityRequestNotAdmitted)
        ));

        let mut background = request.clone();
        background.background = AppInteractiveBackgroundPosture::ReviewedBoundedBackground;
        assert!(background.validate_for_admission().is_err());

        let mut capture = request.clone();
        capture.capture = AppInteractiveCapturePosture::ReviewedPixels;
        assert!(capture.validate_for_admission().is_err());

        let mut transfer = request.clone();
        transfer.transfer = AppInteractiveTransferPosture::ReviewedArtifacts;
        assert!(transfer.validate_for_admission().is_err());

        let mut resources = request.clone();
        resources.resources.max_output_bytes += 1;
        assert_ne!(resources.request_digest().unwrap(), baseline);

        let mut expiry = request.clone();
        expiry.expiry_session.max_session_seconds -= 1;
        assert_ne!(expiry.request_digest().unwrap(), baseline);
    }

    #[test]
    fn reviewed_capability_narrowing_never_widens_or_substitutes_owner() {
        let requested =
            AppInteractiveCapabilityRequest::legacy_observe(AppInteractiveOwnerKind::Browser, 4096)
                .unwrap();
        let mut narrowed = requested.clone();
        narrowed.resources.max_evidence_bytes /= 2;
        narrowed.expiry_session.grant_lifetime_seconds /= 2;
        narrowed.expiry_session.max_session_seconds /= 2;
        assert!(narrowed.narrows(&requested));
        assert!(!requested.narrows(&narrowed));

        let substituted =
            AppInteractiveCapabilityRequest::legacy_observe(AppInteractiveOwnerKind::Macos, 4096)
                .unwrap();
        assert!(!substituted.narrows(&requested));

        let mut removed_origin = requested.clone();
        removed_origin.allowed_origins.clear();
        assert!(removed_origin.validate_for_admission().is_err());
    }

    fn reviewed_action(
        name: &str,
        requires_observation: bool,
        invalidates_observation: bool,
    ) -> AppInteractiveReviewedAction {
        AppInteractiveReviewedAction::reviewed(
            reference(name),
            if requires_observation {
                AppInteractiveActionClass::Interact
            } else {
                AppInteractiveActionClass::Observe
            },
            AppDigest::blake3(format!("{name}-input").as_bytes()),
            AppDigest::blake3(format!("{name}-result").as_bytes()),
            AppDigest::blake3(b"implementation"),
            1024,
            requires_observation.then_some(AppInteractiveObservationKind::StructuredTree),
            invalidates_observation,
        )
        .unwrap()
    }

    fn grant(max_steps: u32) -> AppInteractiveGrantDescriptor {
        AppInteractiveGrantDescriptor::from_reviewed(
            AppInstallationId::parse("installation_1").unwrap(),
            3,
            revision(4),
            AppDigest::blake3(b"grant"),
            AppDigest::blake3(b"capability-grant"),
            AppDigest::blake3(b"locked-interactive-binding"),
            AppDigest::blake3(b"interactive-request"),
            AppDigest::blake3(b"policy"),
            AppInteractiveExecutionProfile::BrowserSession,
            AppDigest::blake3(b"targets"),
            AppDigest::blake3(b"profile"),
            AppDigest::blake3(b"implementation"),
            AppInteractiveBackgroundPosture::DirectOwner,
            BTreeSet::from([AppInteractiveObservationKind::StructuredTree]),
            vec![
                reviewed_action("interactive:observe", false, false),
                reviewed_action("interactive:mutate", true, true),
            ],
            AppInteractiveResourceCeilings::reviewed(
                1,
                max_steps,
                60,
                8192,
                128,
                1024 * 768,
                4096,
                4096,
            )
            .unwrap(),
            7,
            time(0),
            time(30),
        )
        .unwrap()
    }

    fn fence(grant: &AppInteractiveGrantDescriptor, active: bool) -> AppInteractiveCurrentFence {
        AppInteractiveCurrentFence::from_current(
            grant.installation_id().clone(),
            grant.installation_generation(),
            reference("run:1"),
            grant.grant_revision(),
            grant.grant_digest().clone(),
            grant.capability_grant_digest().clone(),
            grant.locked_interactive_binding_digest().clone(),
            grant.interactive_request_digest().clone(),
            grant.policy_digest().clone(),
            grant.target_policy_digest().clone(),
            grant.owner_profile_digest().clone(),
            grant.owner_implementation_digest().clone(),
            7,
            active,
        )
    }

    fn session(
        grant: &AppInteractiveGrantDescriptor,
        current: &AppInteractiveCurrentFence,
    ) -> AppInteractiveSessionHandle {
        AppInteractiveSessionHandle::acquire(
            grant,
            current,
            reference("browser-session:1"),
            AppDigest::blake3(b"target"),
            reference("resource-lease:1"),
            1,
            AppInteractiveCancellation::default(),
            time(1),
            time(20),
        )
        .unwrap()
    }

    #[test]
    fn stale_observation_is_rejected_after_a_mutation() {
        let grant = grant(4);
        let current = fence(&grant, true);
        let mut session = session(&grant, &current);
        let observation_one = session
            .issue_observation(
                &grant,
                &current,
                AppInteractiveObservationKind::StructuredTree,
                AppInteractiveGeometry {
                    width: 1280,
                    height: 720,
                    scale_millis: 1000,
                },
                AppDigest::blake3(b"tree-one"),
                AppDigest::blake3(b"labels"),
                128,
                3,
                time(2),
                time(10),
            )
            .unwrap();
        let observation_two = session
            .issue_observation(
                &grant,
                &current,
                AppInteractiveObservationKind::StructuredTree,
                AppInteractiveGeometry {
                    width: 1280,
                    height: 720,
                    scale_millis: 1000,
                },
                AppDigest::blake3(b"tree-two"),
                AppDigest::blake3(b"labels"),
                128,
                3,
                time(2),
                time(10),
            )
            .unwrap();
        let claim = AppInteractiveResourceClaim {
            output_bytes: 128,
            ..AppInteractiveResourceClaim::default()
        };
        session
            .authorize_action(
                &grant,
                &current,
                &reference("interactive:mutate"),
                b"{\"action\":\"mutate\"}",
                claim.clone(),
                Some(observation_one),
                time(3),
            )
            .unwrap();
        assert!(matches!(
            session.authorize_action(
                &grant,
                &current,
                &reference("interactive:mutate"),
                b"{\"action\":\"mutate\"}",
                claim,
                Some(observation_two),
                time(4),
            ),
            Err(AppInteractiveError::StaleObservation)
        ));
    }

    #[test]
    fn revoked_current_fence_and_stopped_session_fail_closed() {
        let grant = grant(2);
        let inactive = fence(&grant, false);
        assert!(matches!(
            AppInteractiveSessionHandle::acquire(
                &grant,
                &inactive,
                reference("browser-session:1"),
                AppDigest::blake3(b"target"),
                reference("resource-lease:1"),
                1,
                AppInteractiveCancellation::default(),
                time(1),
                time(20),
            ),
            Err(AppInteractiveError::StaleAuthority)
        ));

        let current = fence(&grant, true);
        let mut session = session(&grant, &current);
        session
            .cancellation()
            .cancel(AppInteractiveCancellationReason::OwnerStop);
        assert!(matches!(
            session.authorize_action(
                &grant,
                &current,
                &reference("interactive:observe"),
                b"{\"action\":\"observe\"}",
                AppInteractiveResourceClaim {
                    output_bytes: 128,
                    ..AppInteractiveResourceClaim::default()
                },
                None,
                time(2),
            ),
            Err(AppInteractiveError::StaleAuthority)
        ));
    }

    #[test]
    fn current_fence_rejects_capability_lock_and_request_substitution() {
        let grant = grant(2);
        for (axis, mut substituted) in [
            fence(&grant, true),
            fence(&grant, true),
            fence(&grant, true),
        ]
        .into_iter()
        .enumerate()
        {
            match axis {
                0 => {
                    substituted.capability_grant_digest =
                        AppDigest::blake3(b"other-capability-grant")
                },
                1 => {
                    substituted.locked_interactive_binding_digest =
                        AppDigest::blake3(b"other-interactive-lock")
                },
                _ => {
                    substituted.interactive_request_digest =
                        AppDigest::blake3(b"other-interactive-request")
                },
            }
            assert!(matches!(
                AppInteractiveSessionHandle::acquire(
                    &grant,
                    &substituted,
                    reference("browser-session:substitution"),
                    AppDigest::blake3(b"target"),
                    reference("resource-lease:substitution"),
                    1,
                    AppInteractiveCancellation::default(),
                    time(1),
                    time(20),
                ),
                Err(AppInteractiveError::StaleAuthority)
            ));
        }
    }

    #[test]
    fn step_ceiling_is_checked_before_a_second_action_is_reserved() {
        let grant = grant(1);
        let current = fence(&grant, true);
        let mut session = session(&grant, &current);
        let claim = AppInteractiveResourceClaim {
            output_bytes: 128,
            ..AppInteractiveResourceClaim::default()
        };
        session
            .authorize_action(
                &grant,
                &current,
                &reference("interactive:observe"),
                b"{\"action\":\"observe\"}",
                claim.clone(),
                None,
                time(2),
            )
            .unwrap();
        assert!(matches!(
            session.authorize_action(
                &grant,
                &current,
                &reference("interactive:observe"),
                b"{\"action\":\"observe\"}",
                claim,
                None,
                time(3),
            ),
            Err(AppInteractiveError::ResourceCeilingExceeded)
        ));
    }

    #[test]
    fn live_stop_registry_only_reduces_authority_and_terminal_cleanup_removes_it() {
        let digest = AppDigest::blake3(b"live-stop-registry-fixture");
        let cancellation = AppInteractiveCancellation::default();
        assert!(register_live_interactive_stop_target(
            &digest,
            cancellation.clone(),
            time(20),
            time(1),
        ));
        assert_eq!(
            live_interactive_session_availability(&digest, time(1)),
            AppInteractiveLiveSessionAvailability::Active
        );
        assert!(signal_live_interactive_stop(&digest, time(2)));
        assert_eq!(
            cancellation.reason(),
            Some(AppInteractiveCancellationReason::OwnerStop)
        );
        assert_eq!(
            live_interactive_session_availability(&digest, time(2)),
            AppInteractiveLiveSessionAvailability::OwnerStopSignalled
        );
        retire_live_interactive_stop_target(&digest);
        assert!(!signal_live_interactive_stop(&digest, time(3)));
        assert_eq!(
            live_interactive_session_availability(&digest, time(3)),
            AppInteractiveLiveSessionAvailability::Unavailable
        );
        assert_eq!(
            cancellation.reason(),
            Some(AppInteractiveCancellationReason::OwnerStop)
        );
    }
}
