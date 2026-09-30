//! Revision-CAS and fence-aware app-widget slot assignments.
//!
//! The store owns layout data, not manifest interpretation. Callers supply a
//! bounded, runtime-owned inventory whose package bindings were resolved from
//! current installation truth. In particular, `trusted_system_provenance` may
//! only be set by the host's digest-pinned boot-admission path; a manifest's
//! `distribution: system` spelling is not provenance.
//!
//! Workspace defaults are the *pinned system default set*. Only trusted-system
//! boot admission writes them, every user of the scope who has not customized a
//! slot sees them, and they are maintained as a whole set:
//! `AppSlotSystemDefaultsMaintainer` is the single host-owned writer, so a
//! package that leaves the deployment's admitted inventory loses its slots
//! instead of leaving behind a pinned default no user can clear.
//!
//! One scope document holds workspace defaults and per-user choices. Reads used
//! for rendering are non-mutating. A settings read acquires a monotonically
//! increasing write fence; every mutation must present both that fence and the
//! exact revision returned by the read. This prevents an older settings editor
//! from winning after a newer editor acquired the document, while ordinary
//! revision CAS prevents two writes by the same editor from silently replacing
//! one another.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::magician_v2::apps::authority::AuthenticatedAppScope;
use crate::magician_v2::apps::lifecycle::AppInstallationStatus;
use crate::magician_v2::apps::models::{
    AppContractError, AppContractLimits, AppDigest, AppInstallationId, AppName, AppReference,
    ValidateAppContract,
};
use crate::magician_v2::apps::records::AppScope;
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::execution::file_edit::transaction::acquire_record_decision_lock;

pub const DEFAULT_APP_SLOT_ASSIGNMENT_LIMIT: usize = 32;
pub const MAX_APP_SLOT_ASSIGNMENT_LIMIT: usize = 128;
pub const DEFAULT_APP_SLOT_PICKER_LIMIT: usize = 32;
pub const MAX_APP_SLOT_PICKER_LIMIT: usize = 100;
pub const MAX_APP_SLOT_INVENTORY_PACKAGES: usize = 256;
pub const MAX_APP_SLOT_INVENTORY_WIDGETS: usize = 512;
pub const APP_SLOT_RESOLUTION_BATCH_MAX_ITEMS: usize = 12;
pub const APP_SLOT_RESOLUTION_BATCH_MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const APP_SLOT_RESOLUTION_BATCH_MAX_RESPONSE_BYTES: usize = 128 * 1024;

const SLOT_ASSIGNMENT_SCHEMA_VERSION: u32 = 1;
const MAX_SLOT_STATE_BYTES: u64 = 512 * 1024;
const MAX_SLOT_PAGE_BYTES: usize = 256;
const MAX_SLOT_PAGE_SEGMENTS: usize = 16;
const MAX_SLOT_ID_BYTES: usize = 600;
const MAX_SLOT_CURSOR_BYTES: usize = 640;
const MAX_WIDGET_TITLE_BYTES: usize = 256;
const MAX_SUGGESTED_SLOTS_PER_WIDGET: usize = 16;
const MAX_USERS_PER_SCOPE: usize = 64;
const MAX_CHOICES_PER_USER: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppSlotId(String);

impl AppSlotId {
    pub fn parse(value: impl Into<String>) -> Result<Self, AppSlotAssignmentError> {
        let slot_id = Self::parse_token(value.into())?;
        if !slot_id.is_page_qualified() {
            return Err(AppSlotAssignmentError::Invalid(
                "slot id must be a canonical page-qualified slot",
            ));
        }
        Ok(slot_id)
    }

    fn parse_token(value: String) -> Result<Self, AppSlotAssignmentError> {
        if value.is_empty()
            || value.len() > MAX_SLOT_ID_BYTES
            || value.starts_with('.')
            || value.ends_with('.')
            || value.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
            })
        {
            return Err(AppSlotAssignmentError::Invalid(
                "slot id must be a bounded ASCII slot token",
            ));
        }
        Ok(Self(value))
    }

    /// Construct the canonical page-qualified identity for one host slot.
    ///
    /// The static page route is encoded byte-for-byte as lowercase hex rather
    /// than normalized or hashed. The encoding is therefore injective while
    /// remaining safe as one HTTP path segment: the same region on two pages
    /// can never collapse to one assignment key.
    pub fn for_page_region(page: &str, region: &AppName) -> Result<Self, AppSlotAssignmentError> {
        validate_static_slot_page(page)?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(5 + page.len() * 2 + 1 + region.as_str().len());
        value.push_str("page:");
        for byte in page.bytes() {
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 0x0f) as usize] as char);
        }
        value.push(':');
        value.push_str(region.as_str());
        Self::parse_token(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_page_qualified(&self) -> bool {
        let Some(encoded) = self.0.strip_prefix("page:") else {
            return false;
        };
        let Some((page_hex, region_raw)) = encoded.rsplit_once(':') else {
            return false;
        };
        if page_hex.is_empty()
            || page_hex.len() > MAX_SLOT_PAGE_BYTES * 2
            || page_hex.len() % 2 != 0
        {
            return false;
        }
        let mut page_bytes = Vec::with_capacity(page_hex.len() / 2);
        for pair in page_hex.as_bytes().chunks_exact(2) {
            let Some(high) = decode_lower_hex(pair[0]) else {
                return false;
            };
            let Some(low) = decode_lower_hex(pair[1]) else {
                return false;
            };
            page_bytes.push((high << 4) | low);
        }
        let Ok(page) = String::from_utf8(page_bytes) else {
            return false;
        };
        let Ok(region) = AppName::parse(region_raw) else {
            return false;
        };
        Self::for_page_region(&page, &region).is_ok_and(|expected| expected == *self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotSuggestion {
    pub page: String,
    pub region: AppName,
    pub slot_id: AppSlotId,
    pub system_default: bool,
}

impl AppSlotSuggestion {
    pub fn for_page_region(
        page: impl Into<String>,
        region: AppName,
        system_default: bool,
    ) -> Result<Self, AppSlotAssignmentError> {
        let page = page.into();
        let slot_id = AppSlotId::for_page_region(&page, &region)?;
        Ok(Self {
            page,
            region,
            slot_id,
            system_default,
        })
    }

    fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        let expected = AppSlotId::for_page_region(&self.page, &self.region)?;
        if self.slot_id != expected {
            return Err(AppSlotAssignmentError::Invalid(
                "widget suggestion slot does not match its page and region",
            ));
        }
        Ok(())
    }
}

fn decode_lower_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn validate_static_slot_page(page: &str) -> Result<(), AppSlotAssignmentError> {
    if page.is_empty()
        || page.len() > MAX_SLOT_PAGE_BYTES
        || !page.is_ascii()
        || !page.starts_with('/')
        || page
            .bytes()
            .any(|byte| matches!(byte, b'\\' | b'?' | b'#' | b'%' | b':'))
        || page.chars().any(char::is_control)
    {
        return Err(AppSlotAssignmentError::Invalid(
            "slot page must be a bounded canonical static route",
        ));
    }
    if page == "/" {
        return Ok(());
    }
    let segments: Vec<_> = page[1..].split('/').collect();
    if segments.is_empty()
        || segments.len() > MAX_SLOT_PAGE_SEGMENTS
        || segments.iter().any(|segment| {
            segment.is_empty()
                || matches!(*segment, "." | "..")
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(AppSlotAssignmentError::Invalid(
            "slot page must be a bounded canonical static route",
        ));
    }
    Ok(())
}

impl Serialize for AppSlotId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AppSlotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppSlotRevision(u64);

impl AppSlotRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, AppSlotAssignmentError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(AppSlotAssignmentError::RevisionExhausted)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppSlotWriteFence(u64);

impl AppSlotWriteFence {
    pub const INITIAL: Self = Self(0);

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    fn next(self) -> Result<Self, AppSlotAssignmentError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(AppSlotAssignmentError::FenceExhausted)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotWriteHead {
    pub revision: AppSlotRevision,
    pub fence: AppSlotWriteFence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AppSlotPackageBinding {
    pub installation_id: AppInstallationId,
    pub package_id: AppReference,
    pub package_revision_ref: AppReference,
    pub package_content_digest: AppDigest,
    pub installation_generation: u64,
}

impl AppSlotPackageBinding {
    fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        if self.installation_generation == 0 {
            return Err(AppSlotAssignmentError::Invalid(
                "slot package generation must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AppSlotWidgetBinding {
    pub package: AppSlotPackageBinding,
    pub widget_id: AppName,
}

impl AppSlotWidgetBinding {
    fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        self.package.validate()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSlotPackageAvailability {
    Enabled,
    Disabled,
    UninstalledRetained,
    Quarantined,
    UpdatePending,
    Unavailable,
}

impl From<AppInstallationStatus> for AppSlotPackageAvailability {
    fn from(status: AppInstallationStatus) -> Self {
        match status {
            AppInstallationStatus::Enabled => Self::Enabled,
            AppInstallationStatus::Disabled => Self::Disabled,
            AppInstallationStatus::UninstalledRetained => Self::UninstalledRetained,
            AppInstallationStatus::Quarantined => Self::Quarantined,
            AppInstallationStatus::UpdatePending => Self::UpdatePending,
            AppInstallationStatus::ReadyForReview | AppInstallationStatus::Purged => {
                Self::Unavailable
            },
        }
    }
}

/// Current package truth supplied by a host-owned inventory resolver.
///
/// `trusted_system_provenance` is deliberately independent from manifest
/// distribution. It means the resolver proved digest-pinned trusted-directory
/// boot admission for these exact bytes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppSlotPackageState {
    pub binding: AppSlotPackageBinding,
    pub availability: AppSlotPackageAvailability,
    pub declared_widget_ids: Vec<AppName>,
    trusted_system_provenance: bool,
}

impl AppSlotPackageState {
    pub fn installable(
        binding: AppSlotPackageBinding,
        availability: AppSlotPackageAvailability,
        declared_widget_ids: Vec<AppName>,
    ) -> Self {
        Self {
            binding,
            availability,
            declared_widget_ids,
            trusted_system_provenance: false,
        }
    }

    /// Only the host-owned, digest-pinned boot admission path may call this.
    /// Manifest distribution and ordinary registry metadata are insufficient.
    pub(crate) fn trusted_system(
        binding: AppSlotPackageBinding,
        availability: AppSlotPackageAvailability,
        declared_widget_ids: Vec<AppName>,
    ) -> Self {
        Self {
            binding,
            availability,
            declared_widget_ids,
            trusted_system_provenance: true,
        }
    }

    pub fn has_trusted_system_provenance(&self) -> bool {
        self.trusted_system_provenance
    }

    fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        self.binding.validate()?;
        if self.declared_widget_ids.len() > MAX_APP_SLOT_INVENTORY_WIDGETS {
            return Err(AppSlotAssignmentError::Invalid(
                "package declares too many widgets for slot inventory",
            ));
        }
        let mut unique = HashSet::with_capacity(self.declared_widget_ids.len());
        if self
            .declared_widget_ids
            .iter()
            .any(|widget_id| !unique.insert(widget_id.as_str()))
        {
            return Err(AppSlotAssignmentError::Invalid(
                "package slot inventory repeats a widget id",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotPickerCandidate {
    pub widget: AppSlotWidgetBinding,
    pub title: String,
    pub suggested_slots: Vec<AppSlotSuggestion>,
    /// Derived only from host-controlled package provenance.
    pub system_class: bool,
}

impl AppSlotPickerCandidate {
    fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        self.widget.validate()?;
        if self.title.trim().is_empty()
            || self.title.len() > MAX_WIDGET_TITLE_BYTES
            || self.title.chars().any(char::is_control)
        {
            return Err(AppSlotAssignmentError::Invalid(
                "widget picker title is empty, too large, or contains controls",
            ));
        }
        if self.suggested_slots.len() > MAX_SUGGESTED_SLOTS_PER_WIDGET {
            return Err(AppSlotAssignmentError::Invalid(
                "widget has too many suggested slots",
            ));
        }
        for suggestion in &self.suggested_slots {
            suggestion.validate()?;
            if suggestion.system_default && !self.system_class {
                return Err(AppSlotAssignmentError::Invalid(
                    "untrusted widget cannot claim a system-default slot",
                ));
            }
        }
        let unique: BTreeSet<_> = self
            .suggested_slots
            .iter()
            .map(|suggestion| &suggestion.slot_id)
            .collect();
        if unique.len() != self.suggested_slots.len() {
            return Err(AppSlotAssignmentError::Invalid(
                "widget repeats a suggested slot",
            ));
        }
        Ok(())
    }

    fn cursor_key(&self) -> String {
        format!(
            "{}:{}",
            self.widget.package.installation_id, self.widget.widget_id
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct AppSlotInventorySnapshot {
    pub packages: Vec<AppSlotPackageState>,
    pub picker: Vec<AppSlotPickerCandidate>,
}

impl AppSlotInventorySnapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        if self.packages.len() > MAX_APP_SLOT_INVENTORY_PACKAGES
            || self.picker.len() > MAX_APP_SLOT_INVENTORY_WIDGETS
        {
            return Err(AppSlotAssignmentError::Invalid(
                "slot inventory exceeds its package or widget ceiling",
            ));
        }
        let mut installations = HashSet::with_capacity(self.packages.len());
        for package in &self.packages {
            package.validate()?;
            if !installations.insert(package.binding.installation_id.as_str()) {
                return Err(AppSlotAssignmentError::Invalid(
                    "slot inventory repeats an installation",
                ));
            }
        }
        let mut widgets = BTreeSet::new();
        for candidate in &self.picker {
            candidate.validate()?;
            let Some(package) = self.packages.iter().find(|package| {
                package.binding.installation_id == candidate.widget.package.installation_id
            }) else {
                return Err(AppSlotAssignmentError::Invalid(
                    "picker widget has no current package state",
                ));
            };
            if package.availability != AppSlotPackageAvailability::Enabled
                || package.binding != candidate.widget.package
                || package.trusted_system_provenance != candidate.system_class
                || !package
                    .declared_widget_ids
                    .contains(&candidate.widget.widget_id)
            {
                return Err(AppSlotAssignmentError::Invalid(
                    "picker widget is not an enabled current declaration",
                ));
            }
            if !widgets.insert((
                candidate.widget.package.installation_id.as_str(),
                candidate.widget.widget_id.as_str(),
            )) {
                return Err(AppSlotAssignmentError::Invalid(
                    "slot picker repeats a widget",
                ));
            }
        }
        Ok(())
    }

    /// A resolver-order-independent digest of the exact bounded inventory used
    /// to build a settings page. Clients compare this across cursor pages and
    /// restart pagination when package/widget truth changes between requests.
    fn revision(&self) -> Result<AppDigest, AppSlotAssignmentError> {
        self.validate()?;
        let mut packages = self.packages.clone();
        packages.sort_by(|left, right| left.binding.cmp(&right.binding));
        for package in &mut packages {
            package.declared_widget_ids.sort();
        }
        let mut picker = self.picker.clone();
        picker.sort_by_key(AppSlotPickerCandidate::cursor_key);
        let bytes = serde_json::to_vec(&("magician.app-slot-inventory.v1", packages, picker))
            .map_err(|error| {
                AppSlotAssignmentError::Storage(format!(
                    "encoding the app-slot inventory revision: {error}"
                ))
            })?;
        Ok(AppDigest::blake3(&bytes))
    }

    fn current_package(&self, installation_id: &AppInstallationId) -> Option<&AppSlotPackageState> {
        self.packages
            .iter()
            .find(|package| &package.binding.installation_id == installation_id)
    }

    fn assignable_widget(
        &self,
        installation_id: &AppInstallationId,
        widget_id: &AppName,
    ) -> Option<&AppSlotPickerCandidate> {
        self.picker.iter().find(|candidate| {
            &candidate.widget.package.installation_id == installation_id
                && &candidate.widget.widget_id == widget_id
        })
    }
}

#[derive(Debug, Error)]
pub enum AppSlotInventoryError {
    #[error("the app-widget slot inventory is temporarily unavailable")]
    Unavailable,
    #[error("the app-widget slot inventory is invalid: {0}")]
    Invalid(String),
}

#[async_trait]
pub trait AppSlotInventoryResolver: Send + Sync {
    async fn snapshot(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError>;

    /// Page rendering needs only the Apps referenced by that page's saved
    /// slots. Full inventory remains available to settings and boot admission.
    async fn snapshot_for_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_ids: &[AppInstallationId],
        now: DateTime<Utc>,
    ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
        let _ = installation_ids;
        self.snapshot(authenticated, now).await
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSlotAssignmentSource {
    User,
    WorkspaceDefault,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSlotHiddenReason {
    PackageUnavailable,
    Disabled,
    Quarantined,
    UpdatePending,
    PackageIdentityChanged,
    PackageDigestChanged,
    GenerationRollback,
    WidgetNoLongerDeclared,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSlotAssignmentCompatibility {
    /// This slice intentionally does not invent compatibility migrations.
    ExactDigestOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotEffectiveWidget {
    pub pinned: AppSlotWidgetBinding,
    pub current: AppSlotWidgetBinding,
    pub restored_across_generation: bool,
    pub assignment_compatibility: AppSlotAssignmentCompatibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResolvedSlotAssignment {
    pub slot_id: AppSlotId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AppSlotAssignmentSource>,
    pub pinned_system_default: bool,
    pub opted_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub widget: Option<AppSlotEffectiveWidget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_reason: Option<AppSlotHiddenReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotResolutionBatchRequest {
    pub slot_ids: Vec<AppSlotId>,
}

impl ValidateAppContract for AppSlotResolutionBatchRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.slot_ids.is_empty() || self.slot_ids.len() > APP_SLOT_RESOLUTION_BATCH_MAX_ITEMS {
            return Err(AppContractError::invalid(
                "slot_ids",
                "must contain between one and twelve page-qualified slots",
            ));
        }
        let mut unique = HashSet::with_capacity(self.slot_ids.len());
        if self.slot_ids.iter().any(|slot_id| !unique.insert(slot_id)) {
            return Err(AppContractError::invalid(
                "slot_ids",
                "must not repeat a slot",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotResolutionBatchResponse {
    pub assignments: Vec<AppResolvedSlotAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotSettingsPage {
    pub head: AppSlotWriteHead,
    /// Exact digest of the package/widget inventory used for this page.
    pub inventory_revision: AppDigest,
    pub assignments: Vec<AppResolvedSlotAssignment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_assignment_cursor: Option<String>,
    pub assignments_truncated: bool,
    pub picker: Vec<AppSlotPickerCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_picker_cursor: Option<String>,
    pub picker_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppSlotAssignmentCommand {
    Assign {
        slot_id: AppSlotId,
        installation_id: AppInstallationId,
        widget_id: AppName,
        /// Exact picker-row binding observed by the settings client. This
        /// closes update/reinstall races between picker read and assignment.
        expected_candidate: AppSlotWidgetBinding,
    },
    /// Removing a user assignment records an opt-out. A pinned workspace
    /// default remains intact and therefore can be restored explicitly.
    OptOut { slot_id: AppSlotId },
    /// The only operation that clears customization history for a slot.
    RestoreWorkspaceDefault { slot_id: AppSlotId },
}

impl AppSlotAssignmentCommand {
    fn slot_id(&self) -> &AppSlotId {
        match self {
            Self::Assign { slot_id, .. }
            | Self::OptOut { slot_id }
            | Self::RestoreWorkspaceDefault { slot_id } => slot_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotAssignmentWriteRequest {
    pub expected_revision: AppSlotRevision,
    pub write_fence: AppSlotWriteFence,
    pub mutation_id: AppReference,
    pub command: AppSlotAssignmentCommand,
}

impl ValidateAppContract for AppSlotAssignmentWriteRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.write_fence == AppSlotWriteFence::INITIAL {
            return Err(AppContractError::invalid(
                "write_fence",
                "must come from a settings read",
            ));
        }
        if let AppSlotAssignmentCommand::Assign {
            installation_id,
            widget_id,
            expected_candidate,
            ..
        } = &self.command
        {
            expected_candidate.validate().map_err(|_| {
                AppContractError::invalid(
                    "command.expected_candidate",
                    "must contain a valid positive-generation package/widget binding",
                )
            })?;
            if &expected_candidate.package.installation_id != installation_id
                || &expected_candidate.widget_id != widget_id
            {
                return Err(AppContractError::invalid(
                    "command.expected_candidate",
                    "must identify the command installation_id and widget_id exactly",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSlotAssignmentMutationReceipt {
    pub mutation_id: AppReference,
    pub head: AppSlotWriteHead,
    pub assignment: AppResolvedSlotAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSlotSettingsQuery {
    pub assignment_limit: usize,
    pub assignment_cursor: Option<String>,
    pub picker_limit: usize,
    pub picker_cursor: Option<String>,
}

impl AppSlotSettingsQuery {
    pub fn validate(&self) -> Result<(), AppSlotAssignmentError> {
        if self.assignment_limit == 0
            || self.assignment_limit > MAX_APP_SLOT_ASSIGNMENT_LIMIT
            || self.picker_limit == 0
            || self.picker_limit > MAX_APP_SLOT_PICKER_LIMIT
        {
            return Err(AppSlotAssignmentError::Invalid(
                "slot settings limits are outside their bounded ranges",
            ));
        }
        for cursor in [&self.assignment_cursor, &self.picker_cursor]
            .into_iter()
            .flatten()
        {
            if cursor.is_empty()
                || cursor.len() > MAX_SLOT_CURSOR_BYTES
                || cursor.chars().any(char::is_control)
            {
                return Err(AppSlotAssignmentError::Invalid(
                    "slot settings cursor is invalid",
                ));
            }
        }
        Ok(())
    }
}

impl Default for AppSlotSettingsQuery {
    fn default() -> Self {
        Self {
            assignment_limit: DEFAULT_APP_SLOT_ASSIGNMENT_LIMIT,
            assignment_cursor: None,
            picker_limit: DEFAULT_APP_SLOT_PICKER_LIMIT,
            picker_cursor: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AppSlotAssignmentError {
    #[error("invalid app slot assignment: {0}")]
    Invalid(&'static str),
    #[error("slot assignment revision conflict: expected {expected:?}, found {found:?}")]
    RevisionConflict {
        expected: AppSlotRevision,
        found: AppSlotRevision,
    },
    #[error("slot assignment write fence {expected:?} was superseded by {found:?}")]
    FenceLost {
        expected: AppSlotWriteFence,
        found: AppSlotWriteFence,
    },
    #[error("the requested widget is not an enabled current picker candidate")]
    WidgetUnavailable,
    #[error("the slot assignment revision counter is exhausted")]
    RevisionExhausted,
    #[error("the slot assignment write-fence counter is exhausted")]
    FenceExhausted,
    #[error("corrupt app slot assignment state: {0}")]
    Corrupt(&'static str),
    #[error("app slot assignment storage failed: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "choice", rename_all = "snake_case", deny_unknown_fields)]
enum StoredUserChoice {
    Assigned {
        widget: AppSlotWidgetBinding,
        updated_at: DateTime<Utc>,
    },
    OptedOut {
        updated_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredWorkspaceDefault {
    widget: AppSlotWidgetBinding,
    pinned_system_default: bool,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredAppliedMutation {
    request_fingerprint: AppDigest,
    applied_at: DateTime<Utc>,
    receipt: AppSlotAssignmentMutationReceipt,
}

/// Which boot admission last owned this scope's pinned default set.
///
/// Recorded so maintenance is durable rather than re-derived on every boot: an
/// admission whose inventory digest matches this record and whose pinned slots
/// already match the document writes nothing at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredSystemDefaultsMaintenance {
    inventory_digest: AppDigest,
    maintained_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredSlotAssignmentState {
    schema_version: u32,
    scope: AppScope,
    revision: AppSlotRevision,
    fence: AppSlotWriteFence,
    workspace_defaults: BTreeMap<AppSlotId, StoredWorkspaceDefault>,
    users: BTreeMap<String, BTreeMap<AppSlotId, StoredUserChoice>>,
    #[serde(default)]
    applied_mutations: BTreeMap<String, StoredAppliedMutation>,
    /// Absent on a document written before any admission maintained it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_defaults: Option<StoredSystemDefaultsMaintenance>,
}

/// One immutable slot-state read, retained while the referenced Apps are
/// reopened. This is a display snapshot, never execution authority.
pub struct AppSlotResolutionSnapshot {
    state: StoredSlotAssignmentState,
    user_ref: String,
    slot_ids: Vec<AppSlotId>,
}

impl AppSlotResolutionSnapshot {
    pub fn installation_ids(&self) -> Vec<AppInstallationId> {
        let mut ids = self
            .slot_ids
            .iter()
            .filter_map(|slot_id| {
                selected_slot_widget(&self.state, &self.user_ref, slot_id)
                    .0
                    .map(|widget| widget.package.installation_id.clone())
            })
            .collect::<Vec<_>>();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        ids
    }

    pub fn resolve(
        &self,
        inventory: &AppSlotInventorySnapshot,
    ) -> Result<AppSlotResolutionBatchResponse, AppSlotAssignmentError> {
        inventory.validate()?;
        Ok(AppSlotResolutionBatchResponse {
            assignments: self
                .slot_ids
                .iter()
                .map(|slot_id| {
                    resolve_slot_from_state(&self.state, &self.user_ref, slot_id, inventory)
                })
                .collect(),
        })
    }
}

impl StoredSlotAssignmentState {
    fn empty(scope: AppScope) -> Self {
        Self {
            schema_version: SLOT_ASSIGNMENT_SCHEMA_VERSION,
            scope,
            revision: AppSlotRevision::INITIAL,
            fence: AppSlotWriteFence::INITIAL,
            workspace_defaults: BTreeMap::new(),
            users: BTreeMap::new(),
            applied_mutations: BTreeMap::new(),
            system_defaults: None,
        }
    }

    fn validate(&self, scope: &AppScope) -> Result<(), AppSlotAssignmentError> {
        if self.schema_version != SLOT_ASSIGNMENT_SCHEMA_VERSION || &self.scope != scope {
            return Err(AppSlotAssignmentError::Corrupt(
                "schema or scope identity differs from its storage path",
            ));
        }
        if self.workspace_defaults.len() > MAX_APP_SLOT_ASSIGNMENT_LIMIT
            || self.users.len() > MAX_USERS_PER_SCOPE
            || self.applied_mutations.len() > MAX_APP_SLOT_ASSIGNMENT_LIMIT
            || self
                .users
                .values()
                .any(|choices| choices.len() > MAX_CHOICES_PER_USER)
        {
            return Err(AppSlotAssignmentError::Corrupt(
                "state exceeds its slot or user ceiling",
            ));
        }
        for default in self.workspace_defaults.values() {
            default.widget.validate()?;
            if !default.pinned_system_default {
                return Err(AppSlotAssignmentError::Corrupt(
                    "workspace default lacks trusted-system pin provenance",
                ));
            }
        }
        for user_ref in self.users.keys() {
            if validate_user_ref(user_ref).is_err() {
                return Err(AppSlotAssignmentError::Corrupt(
                    "state contains an invalid user reference",
                ));
            }
        }
        for choice in self.users.values().flat_map(BTreeMap::values) {
            if let StoredUserChoice::Assigned { widget, .. } = choice {
                widget.validate()?;
            }
        }
        for (mutation_id, applied) in &self.applied_mutations {
            if applied.receipt.mutation_id.as_str() != mutation_id {
                return Err(AppSlotAssignmentError::Corrupt(
                    "mutation receipt key differs from its receipt id",
                ));
            }
            if applied.receipt.head.revision > self.revision
                || applied.receipt.head.fence > self.fence
            {
                return Err(AppSlotAssignmentError::Corrupt(
                    "mutation receipt head is newer than slot state",
                ));
            }
            if let Some(widget) = &applied.receipt.assignment.widget {
                widget.pinned.validate()?;
                widget.current.validate()?;
            }
        }
        Ok(())
    }
}

/// Sealed input for the boot-admission owner. It can only be constructed after
/// a resolver explicitly proves trusted system provenance for the current
/// package bytes; manifest distribution alone cannot create this value.
#[derive(Debug, Clone)]
pub struct TrustedSystemSlotDefaults {
    package: AppSlotPackageBinding,
    defaults: BTreeMap<AppSlotId, AppSlotWidgetBinding>,
}

impl TrustedSystemSlotDefaults {
    pub fn from_boot_admission(
        package: &AppSlotPackageState,
        defaults: impl IntoIterator<Item = (AppSlotId, AppName)>,
    ) -> Result<Self, AppSlotAssignmentError> {
        if !package.trusted_system_provenance {
            return Err(AppSlotAssignmentError::Invalid(
                "pinned defaults require trusted system package provenance",
            ));
        }
        package.validate()?;
        let declared: BTreeSet<_> = package.declared_widget_ids.iter().collect();
        let mut admitted = BTreeMap::new();
        for (slot_id, widget_id) in defaults {
            if !declared.contains(&widget_id) {
                return Err(AppSlotAssignmentError::Invalid(
                    "pinned default names an undeclared widget",
                ));
            }
            if admitted
                .insert(
                    slot_id,
                    AppSlotWidgetBinding {
                        package: package.binding.clone(),
                        widget_id,
                    },
                )
                .is_some()
            {
                return Err(AppSlotAssignmentError::Invalid(
                    "system package repeats a pinned default slot",
                ));
            }
        }
        if admitted.len() > MAX_APP_SLOT_ASSIGNMENT_LIMIT {
            return Err(AppSlotAssignmentError::Invalid(
                "system package declares too many pinned defaults",
            ));
        }
        Ok(Self {
            package: package.binding.clone(),
            defaults: admitted,
        })
    }
}

/// The complete pinned-default set one boot admission admitted, keyed by the
/// package that owns each default.
///
/// Maintenance is a set operation and not a per-package one for two reasons. A
/// package that left the deployment's seed inventory must lose its pinned
/// slots — a default set is "never deletable" by users, so a default whose app
/// no longer exists would hold a slot nobody can clear — and only the complete
/// admitted set knows which packages those are. And package-at-a-time applies
/// could not be atomic: a boot that died between two of them would leave the
/// workspace half-maintained.
#[derive(Debug, Clone)]
pub struct TrustedSystemSlotDefaultSet {
    inventory_digest: AppDigest,
    packages: BTreeMap<AppReference, TrustedSystemSlotDefaults>,
    contested: BTreeMap<AppSlotId, BTreeSet<AppReference>>,
}

impl TrustedSystemSlotDefaultSet {
    /// `inventory_digest` must come from a *successful* system-package
    /// inventory resolve — the same digest that pins the admitted bytes.
    ///
    /// An empty set is a legitimate deployment state and retires every pinned
    /// default, which is exactly why a failed inventory resolve must mint no
    /// set at all rather than an empty one: "I could not read the seed root"
    /// and "this deployment pins nothing" are different facts.
    ///
    /// A slot two admitted packages both claim is *contested* and is pinned to
    /// nobody. Refusing the whole set instead would let one authoring conflict
    /// in one seed manifest cost the deployment every other pinned default in
    /// every scope — the same blast radius `admit_system_packages_at_boot`
    /// already rejects for admission itself, where one malformed package must
    /// not cost the others. Dropping the slot is the fail-closed half: no
    /// package wins a slot it does not exclusively own, the choice does not
    /// depend on the order the resolver read packages in, and the suggestion
    /// still reaches the picker so a user can settle it.
    pub fn from_boot_admission(
        inventory_digest: AppDigest,
        admitted: impl IntoIterator<Item = TrustedSystemSlotDefaults>,
    ) -> Result<Self, AppSlotAssignmentError> {
        let admitted: Vec<TrustedSystemSlotDefaults> = admitted.into_iter().collect();
        // Gathered over the whole admission before anything is kept: which slot
        // is contested is not knowable until every package has been read, and
        // resolving it as we go would make the deployment's layout depend on
        // the order packages happened to arrive in.
        let mut claims: BTreeMap<AppSlotId, BTreeSet<AppReference>> = BTreeMap::new();
        for defaults in &admitted {
            for slot_id in defaults.defaults.keys() {
                claims
                    .entry(slot_id.clone())
                    .or_default()
                    .insert(defaults.package.package_id.clone());
            }
        }
        // Claimants rather than a count, because only the author of the
        // conflicting manifests can settle a contested slot and a count names
        // nobody for the boot owner's warning to point at.
        let contested: BTreeMap<AppSlotId, BTreeSet<AppReference>> = claims
            .into_iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .collect();

        let mut packages: BTreeMap<AppReference, TrustedSystemSlotDefaults> = BTreeMap::new();
        // Repeats are tracked separately from what survives, so a package whose
        // every slot was contested still cannot appear twice unnoticed.
        let mut seen: BTreeSet<AppReference> = BTreeSet::new();
        let mut pinned = 0usize;
        for mut defaults in admitted {
            if !seen.insert(defaults.package.package_id.clone()) {
                return Err(AppSlotAssignmentError::Invalid(
                    "boot admission repeats a system package's pinned defaults",
                ));
            }
            defaults
                .defaults
                .retain(|slot_id, _| !contested.contains_key(slot_id));
            if defaults.defaults.is_empty() {
                // Still part of the deployment, it simply owns no slot now. An
                // empty entry would only add a name to the set.
                continue;
            }
            pinned += defaults.defaults.len();
            packages.insert(defaults.package.package_id.clone(), defaults);
        }
        if pinned > MAX_APP_SLOT_ASSIGNMENT_LIMIT {
            return Err(AppSlotAssignmentError::Invalid(
                "boot admission pins more default slots than one scope admits",
            ));
        }
        Ok(Self {
            inventory_digest,
            packages,
            contested,
        })
    }

    /// Slots more than one admitted system package claimed, which this set
    /// therefore pins to nobody, each mapped to every package that claimed it.
    ///
    /// Exposed so the boot owner can name the conflicting deployment manifests
    /// in its log: a contested slot is a seed-authoring bug that only its
    /// author can fix, and a silently empty slot reads exactly like a widget
    /// that was never declared.
    ///
    /// Called by `AppPlatformApi::admit_system_packages_at_boot`, which warns
    /// once per contested slot per scope. The seed root shipped a real
    /// conflict for two increments — `meetings` and `town_square` both pinned
    /// `page: /, slot: ambient`, so that slot was dropped on every boot of
    /// every deployment and this warning was the only thing that said so.
    /// `town_square` gave the slot up; the bytes are now pinned by
    /// `no_two_shipped_system_widgets_pin_the_same_slot_default`, because a
    /// warning nobody reads is not a guard.
    pub fn contested_slots(&self) -> &BTreeMap<AppSlotId, BTreeSet<AppReference>> {
        &self.contested
    }

    /// Derive one scope's admitted set from the inventory the host's own
    /// resolver produced for it, which is how the boot owner mints a set
    /// without hand-listing slots it cannot see.
    ///
    /// Only trusted-system packages contribute, and a widget contributes only
    /// through a suggestion the inventory itself already validated as a
    /// system default — `AppSlotPickerCandidate` validation refuses
    /// `system_default` on an untrusted widget, so an installable package can
    /// never reach this path by spelling it in a manifest.
    ///
    /// A package that pins nothing is skipped rather than admitted with an
    /// empty map: it is still part of the deployment, it simply owns no slot,
    /// and an empty entry would only add a name to the set.
    ///
    /// `inventory` must be the snapshot for the *same* scope this set will
    /// maintain. Package bindings carry that scope's installation generation,
    /// and a binding pinned from another scope would read there as a
    /// generation rollback and hide the widget it was meant to surface.
    pub fn from_boot_inventory(
        inventory_digest: AppDigest,
        inventory: &AppSlotInventorySnapshot,
    ) -> Result<Self, AppSlotAssignmentError> {
        inventory.validate()?;
        let mut admitted = Vec::new();
        for package in &inventory.packages {
            if !package.trusted_system_provenance {
                continue;
            }
            let defaults: Vec<_> = inventory
                .picker
                .iter()
                .filter(|candidate| candidate.widget.package == package.binding)
                .flat_map(|candidate| {
                    candidate
                        .suggested_slots
                        .iter()
                        .filter(|suggestion| suggestion.system_default)
                        .map(|suggestion| {
                            (
                                suggestion.slot_id.clone(),
                                candidate.widget.widget_id.clone(),
                            )
                        })
                })
                .collect();
            if defaults.is_empty() {
                continue;
            }
            admitted.push(TrustedSystemSlotDefaults::from_boot_admission(
                package, defaults,
            )?);
        }
        Self::from_boot_admission(inventory_digest, admitted)
    }

    pub fn inventory_digest(&self) -> &AppDigest {
        &self.inventory_digest
    }

    /// Every pinned slot the admitted packages own, flattened. Slot ids are
    /// unique across the set by construction, so no default can be shadowed.
    fn pinned_defaults(&self) -> BTreeMap<AppSlotId, AppSlotWidgetBinding> {
        self.packages
            .values()
            .flat_map(|defaults| {
                defaults
                    .defaults
                    .iter()
                    .map(|(slot_id, widget)| (slot_id.clone(), widget.clone()))
            })
            .collect()
    }
}

/// What maintenance did to one scope's pinned default set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSlotSystemDefaultsMaintenance {
    pub head: AppSlotWriteHead,
    pub pinned_slots: usize,
    pub retired_slots: usize,
    /// False when the admitted set was already in place, which is the ordinary
    /// case on every boot after the first.
    pub changed: bool,
}

#[derive(Debug, Clone)]
pub struct AppSlotAssignmentStore {
    workspace: ArtifactV2Workspace,
}

impl AppSlotAssignmentStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    fn root(&self, scope: &AppScope) -> PathBuf {
        self.workspace
            .scope_root(scope.principal.as_str(), scope.workspace.as_str())
            .join("apps")
            .join("slot-assignments")
    }

    fn state_path(&self, scope: &AppScope) -> PathBuf {
        self.root(scope).join("state.json")
    }

    fn load_unlocked(
        &self,
        scope: &AppScope,
    ) -> Result<StoredSlotAssignmentState, AppSlotAssignmentError> {
        let path = self.state_path(scope);
        let Some(metadata) = self
            .workspace
            .metadata_path_sync(&path)
            .map_err(storage_error)?
        else {
            return Ok(StoredSlotAssignmentState::empty(scope.clone()));
        };
        if metadata.len() > MAX_SLOT_STATE_BYTES {
            return Err(AppSlotAssignmentError::Corrupt(
                "state exceeds its byte ceiling",
            ));
        }
        let state: StoredSlotAssignmentState = self
            .workspace
            .read_json_path_sync(&path)
            .map_err(storage_error)?;
        state.validate(scope)?;
        Ok(state)
    }

    fn write_unlocked(
        &self,
        scope: &AppScope,
        state: &StoredSlotAssignmentState,
    ) -> Result<(), AppSlotAssignmentError> {
        state.validate(scope)?;
        let bytes = serde_json::to_vec(state).map_err(|error| {
            AppSlotAssignmentError::Storage(format!("encoding slot state: {error}"))
        })?;
        if bytes.len() as u64 > MAX_SLOT_STATE_BYTES {
            return Err(AppSlotAssignmentError::Invalid(
                "slot assignment state exceeds its byte ceiling",
            ));
        }
        self.workspace
            .write_atomic_path_sync(self.state_path(scope), &bytes)
            .map_err(storage_error)
    }

    pub fn prepare_resolution(
        &self,
        scope: &AppScope,
        user_ref: &str,
        slot_ids: &[AppSlotId],
    ) -> Result<AppSlotResolutionSnapshot, AppSlotAssignmentError> {
        if slot_ids.is_empty() || slot_ids.len() > APP_SLOT_RESOLUTION_BATCH_MAX_ITEMS {
            return Err(AppSlotAssignmentError::Invalid(
                "slot resolution batch must contain between one and twelve slots",
            ));
        }
        let mut unique = HashSet::with_capacity(slot_ids.len());
        if slot_ids.iter().any(|slot_id| !unique.insert(slot_id)) {
            return Err(AppSlotAssignmentError::Invalid(
                "slot resolution batch repeats a slot",
            ));
        }
        validate_user_ref(user_ref)?;
        Ok(AppSlotResolutionSnapshot {
            state: self.load_unlocked(scope)?,
            user_ref: user_ref.to_owned(),
            slot_ids: slot_ids.to_vec(),
        })
    }

    pub fn resolve_slot(
        &self,
        scope: &AppScope,
        user_ref: &str,
        slot_id: &AppSlotId,
        inventory: &AppSlotInventorySnapshot,
    ) -> Result<AppResolvedSlotAssignment, AppSlotAssignmentError> {
        inventory.validate()?;
        validate_user_ref(user_ref)?;
        let state = self.load_unlocked(scope)?;
        Ok(resolve_slot_from_state(
            &state, user_ref, slot_id, inventory,
        ))
    }

    /// Resolve an exact ordered slot set with one inventory validation and one
    /// state load. This is the rendering path for pages with multiple regions;
    /// duplicate inputs are rejected so callers cannot amplify response work.
    pub fn resolve_slots(
        &self,
        scope: &AppScope,
        user_ref: &str,
        slot_ids: &[AppSlotId],
        inventory: &AppSlotInventorySnapshot,
    ) -> Result<AppSlotResolutionBatchResponse, AppSlotAssignmentError> {
        self.prepare_resolution(scope, user_ref, slot_ids)?
            .resolve(inventory)
    }

    /// Acquire/renew the write fence and return a bounded settings snapshot.
    pub fn acquire_settings_page(
        &self,
        scope: &AppScope,
        user_ref: &str,
        query: &AppSlotSettingsQuery,
        inventory: &AppSlotInventorySnapshot,
    ) -> Result<AppSlotSettingsPage, AppSlotAssignmentError> {
        query.validate()?;
        inventory.validate()?;
        validate_user_ref(user_ref)?;
        let root = self.root(scope);
        let _guard =
            acquire_record_decision_lock(&root, "state", "slot assignment").map_err(|error| {
                AppSlotAssignmentError::Storage(format!("locking slot state: {error:#}"))
            })?;
        let mut state = self.load_unlocked(scope)?;
        state.fence = state.fence.next()?;
        self.write_unlocked(scope, &state)?;
        settings_page_from_state(&state, user_ref, query, inventory)
    }

    pub fn apply_user_assignment(
        &self,
        scope: &AppScope,
        user_ref: &str,
        request: &AppSlotAssignmentWriteRequest,
        inventory: &AppSlotInventorySnapshot,
        now: DateTime<Utc>,
    ) -> Result<AppSlotAssignmentMutationReceipt, AppSlotAssignmentError> {
        inventory.validate()?;
        validate_user_ref(user_ref)?;
        let root = self.root(scope);
        let _guard =
            acquire_record_decision_lock(&root, "state", "slot assignment").map_err(|error| {
                AppSlotAssignmentError::Storage(format!("locking slot state: {error:#}"))
            })?;
        let mut state = self.load_unlocked(scope)?;
        let request_fingerprint =
            AppDigest::blake3(&serde_json::to_vec(request).map_err(|error| {
                AppSlotAssignmentError::Storage(format!(
                    "encoding slot mutation fingerprint: {error}"
                ))
            })?);
        if let Some(applied) = state.applied_mutations.get(request.mutation_id.as_str()) {
            if applied.request_fingerprint != request_fingerprint {
                return Err(AppSlotAssignmentError::Invalid(
                    "mutation id replay substituted its slot request",
                ));
            }
            return Ok(applied.receipt.clone());
        }
        compare_head(&state, request.expected_revision, request.write_fence)?;
        if !state.users.contains_key(user_ref) && state.users.len() >= MAX_USERS_PER_SCOPE {
            return Err(AppSlotAssignmentError::Invalid(
                "slot assignment scope has too many users",
            ));
        }
        let user_choices = state.users.entry(user_ref.to_owned()).or_default();
        match &request.command {
            AppSlotAssignmentCommand::Assign {
                slot_id,
                installation_id,
                widget_id,
                expected_candidate,
            } => {
                let candidate = inventory
                    .assignable_widget(installation_id, widget_id)
                    .ok_or(AppSlotAssignmentError::WidgetUnavailable)?;
                if &candidate.widget != expected_candidate {
                    return Err(AppSlotAssignmentError::WidgetUnavailable);
                }
                user_choices.insert(
                    slot_id.clone(),
                    StoredUserChoice::Assigned {
                        widget: candidate.widget.clone(),
                        updated_at: now.clone(),
                    },
                );
            },
            AppSlotAssignmentCommand::OptOut { slot_id } => {
                user_choices.insert(
                    slot_id.clone(),
                    StoredUserChoice::OptedOut {
                        updated_at: now.clone(),
                    },
                );
            },
            AppSlotAssignmentCommand::RestoreWorkspaceDefault { slot_id } => {
                user_choices.remove(slot_id);
            },
        }
        if user_choices.len() > MAX_CHOICES_PER_USER {
            return Err(AppSlotAssignmentError::Invalid(
                "user has too many customized slots",
            ));
        }
        state.revision = state.revision.next()?;
        let assignment =
            resolve_slot_from_state(&state, user_ref, request.command.slot_id(), inventory);
        let receipt = AppSlotAssignmentMutationReceipt {
            mutation_id: request.mutation_id.clone(),
            head: AppSlotWriteHead {
                revision: state.revision,
                fence: state.fence,
            },
            assignment,
        };
        state.applied_mutations.insert(
            request.mutation_id.to_string(),
            StoredAppliedMutation {
                request_fingerprint,
                applied_at: now,
                receipt: receipt.clone(),
            },
        );
        while state.applied_mutations.len() > MAX_APP_SLOT_ASSIGNMENT_LIMIT {
            let Some(oldest) = state
                .applied_mutations
                .iter()
                .min_by(|(left_id, left), (right_id, right)| {
                    left.applied_at
                        .cmp(&right.applied_at)
                        .then_with(|| left_id.cmp(right_id))
                })
                .map(|(mutation_id, _)| mutation_id.clone())
            else {
                break;
            };
            state.applied_mutations.remove(&oldest);
        }
        self.write_unlocked(scope, &state)?;
        Ok(receipt)
    }

    /// Replace this scope's whole pinned system default set in one transaction.
    ///
    /// This is the only writer of `workspace_defaults`. It pins every slot the
    /// admission owns, retires a slot whose package is no longer admitted, and
    /// never touches a user's own choices: an opt-out stays an opt-out and an
    /// explicit assignment keeps winning over the default.
    ///
    /// There is deliberately no caller-supplied expected head. A settings
    /// editor must CAS because its decision was formed outside the lock from
    /// the document it read; the admitted set is formed from package
    /// provenance and does not depend on the document at all. The lock makes
    /// read, diff and write atomic, and the revision bump on a real change
    /// makes an in-flight editor that read the *old* defaults lose its CAS
    /// rather than write over the new ones. Recording a new inventory digest
    /// over an identical set is not such a change and does not bump it.
    pub fn maintain_system_defaults(
        &self,
        scope: &AppScope,
        admitted: &TrustedSystemSlotDefaultSet,
        now: DateTime<Utc>,
    ) -> Result<AppSlotSystemDefaultsMaintenance, AppSlotAssignmentError> {
        let root = self.root(scope);
        let _guard =
            acquire_record_decision_lock(&root, "state", "slot assignment").map_err(|error| {
                AppSlotAssignmentError::Storage(format!("locking slot state: {error:#}"))
            })?;
        let mut state = self.load_unlocked(scope)?;
        let desired = admitted.pinned_defaults();
        let held: BTreeMap<AppSlotId, AppSlotWidgetBinding> = state
            .workspace_defaults
            .iter()
            .map(|(slot_id, held)| (slot_id.clone(), held.widget.clone()))
            .collect();
        let changed = held != desired;
        let already_recorded = state
            .system_defaults
            .as_ref()
            .is_some_and(|record| record.inventory_digest == admitted.inventory_digest);
        if !changed && already_recorded {
            return Ok(AppSlotSystemDefaultsMaintenance {
                head: AppSlotWriteHead {
                    revision: state.revision,
                    fence: state.fence,
                },
                pinned_slots: desired.len(),
                retired_slots: 0,
                changed: false,
            });
        }
        let retired_slots = held
            .keys()
            .filter(|slot_id| !desired.contains_key(*slot_id))
            .count();
        let mut next_defaults = BTreeMap::new();
        for (slot_id, widget) in &desired {
            // An unchanged binding keeps its original pin time, so a boot that
            // re-applies the same set is not recorded as a fresh pin.
            let updated_at = state
                .workspace_defaults
                .get(slot_id)
                .filter(|previous| &previous.widget == widget)
                .map_or(now, |previous| previous.updated_at);
            next_defaults.insert(
                slot_id.clone(),
                StoredWorkspaceDefault {
                    widget: widget.clone(),
                    pinned_system_default: true,
                    updated_at,
                },
            );
        }
        state.workspace_defaults = next_defaults;
        state.system_defaults = Some(StoredSystemDefaultsMaintenance {
            inventory_digest: admitted.inventory_digest.clone(),
            maintained_at: now,
        });
        if changed {
            state.revision = state.revision.next()?;
        }
        self.write_unlocked(scope, &state)?;
        Ok(AppSlotSystemDefaultsMaintenance {
            head: AppSlotWriteHead {
                revision: state.revision,
                fence: state.fence,
            },
            pinned_slots: state.workspace_defaults.len(),
            retired_slots,
            changed,
        })
    }
}

/// Supplies one scope's admitted pinned defaults.
///
/// Per scope, not once for the deployment, because only part of a system
/// package's slot binding is content-derived. `installation_id`,
/// `package_revision_ref` and `package_content_digest` are the same wherever
/// the same bytes were admitted, but `installation_generation` comes from that
/// scope's own installation record — pinning one scope's generation into
/// another would hide the widget there as a generation rollback.
///
/// Implemented by the boot owner, which alone can prove trusted-system
/// provenance for a scope's installations. A scope whose set cannot be
/// resolved must return `Err`, never an empty set: maintenance retires the
/// defaults a set omits, and "I could not resolve this scope" must not be
/// spelled the same way as "this scope pins nothing".
pub trait TrustedSystemSlotDefaultSource: Send + Sync {
    fn admitted_defaults(
        &self,
        scope: &AppScope,
    ) -> Result<TrustedSystemSlotDefaultSet, AppSlotAssignmentError>;
}

/// The per-scope sets one boot actually resolved.
///
/// Resolution is asynchronous and happens a scope at a time, while maintenance
/// is synchronous and enumerates scopes itself; this carries the resolved sets
/// across that boundary so the boot owner can do its await-ing work first and
/// hand the maintainer a source that never blocks.
///
/// A scope the boot could not resolve is simply absent, which is exactly the
/// refusal the trait demands: maintenance records the error and leaves that
/// scope's pinned defaults untouched, rather than reading "nothing resolved"
/// as "this deployment pins nothing" and retiring them on a bad boot.
#[derive(Debug, Default)]
pub struct BootAdmittedSlotDefaults {
    resolved: BTreeMap<(String, String), TrustedSystemSlotDefaultSet>,
}

impl BootAdmittedSlotDefaults {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what one scope admitted. Keyed by the scope's reference strings
    /// because `AppScope` is not ordered; a boot resolves each scope once, so
    /// a repeat record is the same boot's later answer and replaces the first.
    pub fn record(&mut self, scope: &AppScope, admitted: TrustedSystemSlotDefaultSet) {
        self.resolved.insert(
            (
                scope.principal.as_str().to_owned(),
                scope.workspace.as_str().to_owned(),
            ),
            admitted,
        );
    }

    /// Whether any scope resolved. A boot that resolved none must not run
    /// maintenance at all: every scope would refuse — correct, but it buys a
    /// page of warnings for a guaranteed no-op.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty()
    }
}

impl TrustedSystemSlotDefaultSource for BootAdmittedSlotDefaults {
    fn admitted_defaults(
        &self,
        scope: &AppScope,
    ) -> Result<TrustedSystemSlotDefaultSet, AppSlotAssignmentError> {
        self.resolved
            .get(&(
                scope.principal.as_str().to_owned(),
                scope.workspace.as_str().to_owned(),
            ))
            .cloned()
            .ok_or(AppSlotAssignmentError::Invalid(
                "this boot resolved no admitted default set for the scope",
            ))
    }
}

/// The host-owned owner of every workspace's pinned system default set.
///
/// This is what a shared workspace owner can mean in this storage topology. A
/// workspace is identified by its `scopes/<principal>/<workspace>` directory,
/// so there is no location a second principal could share and no registry that
/// says two directories are one workspace; sharing has to come from the
/// *writer*. The host mints an admitted set for every scope the deployment
/// stores, and inside each scope document the pinned defaults are already
/// shared by every `user_ref` that has not customized the slot. A per-request
/// path can never take this role: a set can only be minted from trusted-system
/// package provenance, which no HTTP caller can produce.
///
/// Synchronous by design — scope enumeration and the per-scope file lock are
/// blocking work, so the boot caller runs this on a blocking worker.
#[derive(Debug, Clone)]
pub struct AppSlotSystemDefaultsMaintainer {
    store: AppSlotAssignmentStore,
}

impl AppSlotSystemDefaultsMaintainer {
    pub fn new(store: AppSlotAssignmentStore) -> Self {
        Self { store }
    }

    /// Maintain every scope on disk, plus the default scope.
    ///
    /// The default scope exists conceptually before anything has written to it
    /// and is the scope a single-user deployment actually uses, so discovering
    /// zero scopes on a first boot must not mean the deployment pins nothing.
    ///
    /// One scope's failure is recorded, never propagated: a single corrupt,
    /// unrepresentable or unresolvable scope must not cost every other
    /// workspace its defaults, and a boot path that returned `Err` on the first
    /// problem would do exactly that. This mirrors `admit_system_packages`,
    /// whose per-scope fan-out produced the installations these defaults pin.
    pub fn maintain_every_scope(
        &self,
        source: &dyn TrustedSystemSlotDefaultSource,
        now: DateTime<Utc>,
    ) -> Vec<AppSlotSystemDefaultsOutcome> {
        let mut scopes = self.store.workspace.list_tenant_scopes();
        let default_scope = (
            DEFAULT_SCOPE_PRINCIPAL.to_owned(),
            DEFAULT_SCOPE_WORKSPACE.to_owned(),
        );
        if !scopes.contains(&default_scope) {
            scopes.push(default_scope);
        }
        scopes.sort();
        scopes.dedup();
        scopes
            .into_iter()
            .map(|(principal, workspace)| {
                let result = Self::scope_from_directory(&principal, &workspace).and_then(|scope| {
                    // Resolve first and abandon the scope on failure: an
                    // unresolved scope must keep the defaults it already has.
                    let admitted = source.admitted_defaults(&scope)?;
                    self.store.maintain_system_defaults(&scope, &admitted, now)
                });
                AppSlotSystemDefaultsOutcome {
                    principal,
                    workspace,
                    result,
                }
            })
            .collect()
    }

    /// A scope directory name that is not a valid reference is refused rather
    /// than skipped: passing over a scope silently is indistinguishable from a
    /// deployment that has no such scope.
    ///
    /// Directory names are also normalised (`safe_segment`), so a scope whose
    /// principal contained a normalised character reconstructs as a different
    /// reference than the one that created it. That document's own scope check
    /// then refuses the write as corrupt, which is the intended outcome: the
    /// maintainer must not adopt a document it cannot name exactly.
    fn scope_from_directory(
        principal: &str,
        workspace: &str,
    ) -> Result<AppScope, AppSlotAssignmentError> {
        let (Ok(principal), Ok(workspace)) = (
            AppReference::parse(principal),
            AppReference::parse(workspace),
        ) else {
            return Err(AppSlotAssignmentError::Invalid(
                "scope directory name is not a valid app reference",
            ));
        };
        Ok(AppScope {
            principal,
            workspace,
        })
    }
}

/// What maintenance did for one scope.
#[derive(Debug)]
pub struct AppSlotSystemDefaultsOutcome {
    pub principal: String,
    pub workspace: String,
    pub result: Result<AppSlotSystemDefaultsMaintenance, AppSlotAssignmentError>,
}

impl AppSlotSystemDefaultsOutcome {
    pub fn succeeded(&self) -> bool {
        self.result.is_ok()
    }
}

fn compare_head(
    state: &StoredSlotAssignmentState,
    expected_revision: AppSlotRevision,
    expected_fence: AppSlotWriteFence,
) -> Result<(), AppSlotAssignmentError> {
    if state.fence != expected_fence {
        return Err(AppSlotAssignmentError::FenceLost {
            expected: expected_fence,
            found: state.fence,
        });
    }
    if state.revision != expected_revision {
        return Err(AppSlotAssignmentError::RevisionConflict {
            expected: expected_revision,
            found: state.revision,
        });
    }
    Ok(())
}

fn validate_user_ref(user_ref: &str) -> Result<(), AppSlotAssignmentError> {
    if user_ref.is_empty()
        || user_ref.len() > 192
        || user_ref.chars().any(|character| character.is_control())
    {
        return Err(AppSlotAssignmentError::Invalid(
            "slot assignment user reference is invalid",
        ));
    }
    Ok(())
}

fn settings_page_from_state(
    state: &StoredSlotAssignmentState,
    user_ref: &str,
    query: &AppSlotSettingsQuery,
    inventory: &AppSlotInventorySnapshot,
) -> Result<AppSlotSettingsPage, AppSlotAssignmentError> {
    let inventory_revision = inventory.revision()?;
    let mut slots: BTreeSet<AppSlotId> = state.workspace_defaults.keys().cloned().collect();
    if let Some(choices) = state.users.get(user_ref) {
        slots.extend(choices.keys().cloned());
    }
    let after_assignment = query.assignment_cursor.as_deref();
    let matching_slots: Vec<_> = slots
        .into_iter()
        .filter(|slot| after_assignment.is_none_or(|after| slot.as_str() > after))
        .collect();
    let assignments_truncated = matching_slots.len() > query.assignment_limit;
    let assignments: Vec<_> = matching_slots
        .into_iter()
        .take(query.assignment_limit)
        .map(|slot_id| resolve_slot_from_state(state, user_ref, &slot_id, inventory))
        .collect();
    let next_assignment_cursor = assignments_truncated
        .then(|| {
            assignments
                .last()
                .map(|assignment| assignment.slot_id.as_str().to_owned())
        })
        .flatten();

    let mut picker = inventory.picker.clone();
    picker.sort_by_key(AppSlotPickerCandidate::cursor_key);
    let after_picker = query.picker_cursor.as_deref();
    let matching_picker: Vec<_> = picker
        .into_iter()
        .filter(|candidate| {
            after_picker.is_none_or(|after| candidate.cursor_key().as_str() > after)
        })
        .collect();
    let picker_truncated = matching_picker.len() > query.picker_limit;
    let picker: Vec<_> = matching_picker
        .into_iter()
        .take(query.picker_limit)
        .collect();
    let next_picker_cursor = picker_truncated
        .then(|| picker.last().map(AppSlotPickerCandidate::cursor_key))
        .flatten();

    Ok(AppSlotSettingsPage {
        head: AppSlotWriteHead {
            revision: state.revision,
            fence: state.fence,
        },
        inventory_revision,
        assignments,
        next_assignment_cursor,
        assignments_truncated,
        picker,
        next_picker_cursor,
        picker_truncated,
    })
}

fn selected_slot_widget<'a>(
    state: &'a StoredSlotAssignmentState,
    user_ref: &str,
    slot_id: &AppSlotId,
) -> (
    Option<&'a AppSlotWidgetBinding>,
    Option<AppSlotAssignmentSource>,
    bool,
) {
    let default = state.workspace_defaults.get(slot_id);
    let choice = state
        .users
        .get(user_ref)
        .and_then(|choices| choices.get(slot_id));
    match choice {
        Some(StoredUserChoice::Assigned { widget, .. }) => {
            (Some(widget), Some(AppSlotAssignmentSource::User), false)
        },
        Some(StoredUserChoice::OptedOut { .. }) => (None, None, true),
        None => (
            default.map(|default| &default.widget),
            default.map(|_| AppSlotAssignmentSource::WorkspaceDefault),
            false,
        ),
    }
}

fn resolve_slot_from_state(
    state: &StoredSlotAssignmentState,
    user_ref: &str,
    slot_id: &AppSlotId,
    inventory: &AppSlotInventorySnapshot,
) -> AppResolvedSlotAssignment {
    let default = state.workspace_defaults.get(slot_id);
    let (widget, source, opted_out) = selected_slot_widget(state, user_ref, slot_id);
    let pinned_system_default = default.is_some_and(|default| default.pinned_system_default);
    let Some(widget) = widget else {
        return AppResolvedSlotAssignment {
            slot_id: slot_id.clone(),
            source,
            pinned_system_default,
            opted_out,
            widget: None,
            hidden_reason: None,
        };
    };
    let resolution = resolve_widget(widget, inventory);
    AppResolvedSlotAssignment {
        slot_id: slot_id.clone(),
        source,
        pinned_system_default,
        opted_out,
        widget: resolution.as_ref().ok().cloned(),
        hidden_reason: resolution.err(),
    }
}

fn resolve_widget(
    pinned: &AppSlotWidgetBinding,
    inventory: &AppSlotInventorySnapshot,
) -> Result<AppSlotEffectiveWidget, AppSlotHiddenReason> {
    let Some(current) = inventory.current_package(&pinned.package.installation_id) else {
        return Err(AppSlotHiddenReason::PackageUnavailable);
    };
    if current.binding.package_id != pinned.package.package_id {
        return Err(AppSlotHiddenReason::PackageIdentityChanged);
    }
    if current.binding.package_content_digest != pinned.package.package_content_digest {
        return Err(AppSlotHiddenReason::PackageDigestChanged);
    }
    if current.binding.installation_generation < pinned.package.installation_generation {
        return Err(AppSlotHiddenReason::GenerationRollback);
    }
    match current.availability {
        AppSlotPackageAvailability::Enabled => {},
        AppSlotPackageAvailability::Disabled => return Err(AppSlotHiddenReason::Disabled),
        AppSlotPackageAvailability::Quarantined => return Err(AppSlotHiddenReason::Quarantined),
        AppSlotPackageAvailability::UpdatePending => {
            return Err(AppSlotHiddenReason::UpdatePending)
        },
        AppSlotPackageAvailability::UninstalledRetained
        | AppSlotPackageAvailability::Unavailable => {
            return Err(AppSlotHiddenReason::PackageUnavailable)
        },
    }
    if !current.declared_widget_ids.contains(&pinned.widget_id) {
        return Err(AppSlotHiddenReason::WidgetNoLongerDeclared);
    }
    Ok(AppSlotEffectiveWidget {
        pinned: pinned.clone(),
        current: AppSlotWidgetBinding {
            package: current.binding.clone(),
            widget_id: pinned.widget_id.clone(),
        },
        restored_across_generation: current.binding.installation_generation
            != pinned.package.installation_generation,
        assignment_compatibility: AppSlotAssignmentCompatibility::ExactDigestOnly,
    })
}

fn storage_error(error: impl std::fmt::Display) -> AppSlotAssignmentError {
    AppSlotAssignmentError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: &str) -> AppDigest {
        AppDigest::blake3(seed.as_bytes())
    }

    fn scope() -> AppScope {
        AppScope {
            principal: AppReference::parse("owner").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        }
    }

    fn binding(generation: u64, package_digest: AppDigest) -> AppSlotPackageBinding {
        AppSlotPackageBinding {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_id: AppReference::parse("package:claims").unwrap(),
            package_revision_ref: AppReference::parse(format!("package:{generation}")).unwrap(),
            package_content_digest: package_digest,
            installation_generation: generation,
        }
    }

    fn slot() -> AppSlotId {
        AppSlotId::for_page_region("/", &AppName::parse("review").unwrap()).unwrap()
    }

    fn inventory(
        generation: u64,
        package_digest: AppDigest,
        availability: AppSlotPackageAvailability,
        trusted_system_provenance: bool,
    ) -> AppSlotInventorySnapshot {
        let package = binding(generation, package_digest);
        let widget_id = AppName::parse("pending_claims").unwrap();
        AppSlotInventorySnapshot {
            packages: vec![AppSlotPackageState {
                binding: package.clone(),
                availability,
                declared_widget_ids: vec![widget_id.clone()],
                trusted_system_provenance,
            }],
            picker: if availability == AppSlotPackageAvailability::Enabled {
                vec![AppSlotPickerCandidate {
                    widget: AppSlotWidgetBinding { package, widget_id },
                    title: "Pending claims".to_owned(),
                    suggested_slots: vec![AppSlotSuggestion::for_page_region(
                        "/",
                        AppName::parse("review").unwrap(),
                        trusted_system_provenance,
                    )
                    .unwrap()],
                    system_class: trusted_system_provenance,
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn page_qualified_slot_ids_are_injective_and_reject_dynamic_routes() {
        let region = AppName::parse("review").unwrap();
        let home = AppSlotId::for_page_region("/", &region).unwrap();
        let observe = AppSlotId::for_page_region("/observe", &region).unwrap();
        assert_ne!(home, observe);
        assert_eq!(home.as_str(), "page:2f:review");
        assert!(home.is_page_qualified());
        assert!(observe.is_page_qualified());
        assert!(AppSlotId::for_page_region("/entities/:entity_id", &region).is_err());
        assert!(AppSlotId::for_page_region("/observe?mode=all", &region).is_err());
        assert!(AppSlotId::parse("review").is_err());
    }

    #[test]
    fn prepared_page_resolution_only_reopens_selected_apps_and_keeps_one_state_snapshot() {
        let (_temporary, store) = store();
        let primary = slot();
        let other =
            AppSlotId::for_page_region("/other", &AppName::parse("other").unwrap()).unwrap();
        let live = inventory(1, digest("v1"), AppSlotPackageAvailability::Enabled, true);
        let widget = live.picker[0].widget.clone();
        let mut state = StoredSlotAssignmentState::empty(scope());
        state.workspace_defaults.insert(
            primary.clone(),
            StoredWorkspaceDefault {
                widget: widget.clone(),
                pinned_system_default: true,
                updated_at: Utc::now(),
            },
        );
        let mut unrelated = widget.clone();
        unrelated.package.installation_id = AppInstallationId::parse("install_unrelated").unwrap();
        state.workspace_defaults.insert(
            other,
            StoredWorkspaceDefault {
                widget: unrelated,
                pinned_system_default: true,
                updated_at: Utc::now(),
            },
        );
        store.write_unlocked(&scope(), &state).unwrap();
        let prepared = store
            .prepare_resolution(&scope(), "owner", &[primary.clone()])
            .unwrap();
        assert_eq!(
            prepared.installation_ids(),
            vec![widget.package.installation_id]
        );
        // An assignment change after the read cannot mix a new saved binding
        // with the earlier inventory request. A following read sees the change.
        state.users.entry("owner".to_owned()).or_default().insert(
            primary.clone(),
            StoredUserChoice::OptedOut {
                updated_at: Utc::now(),
            },
        );
        store.write_unlocked(&scope(), &state).unwrap();
        assert!(prepared.resolve(&live).unwrap().assignments[0]
            .widget
            .is_some());
        let next = store
            .prepare_resolution(&scope(), "owner", &[primary])
            .unwrap();
        assert!(next.installation_ids().is_empty());
        assert!(
            next.resolve(&AppSlotInventorySnapshot::empty())
                .unwrap()
                .assignments[0]
                .opted_out
        );
        // Saved state is only display preference; live disablement still wins.
        let disabled = inventory(1, digest("v1"), AppSlotPackageAvailability::Disabled, true);
        assert_eq!(
            prepared.resolve(&disabled).unwrap().assignments[0].hidden_reason,
            Some(AppSlotHiddenReason::Disabled)
        );
    }

    #[test]
    fn settings_queries_reject_empty_cursors() {
        let query = AppSlotSettingsQuery {
            assignment_cursor: Some(String::new()),
            ..AppSlotSettingsQuery::default()
        };
        assert!(query.validate().is_err());
    }

    #[test]
    fn batch_resolution_preserves_order_and_rejects_duplicates() {
        let (_tmp, store) = store();
        let inventory = inventory(
            1,
            digest("same"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        let primary = slot();
        let secondary =
            AppSlotId::for_page_region("/", &AppName::parse("secondary").unwrap()).unwrap();
        let response = store
            .resolve_slots(
                &scope(),
                "owner",
                &[secondary.clone(), primary.clone()],
                &inventory,
            )
            .unwrap();
        assert_eq!(response.assignments[0].slot_id, secondary);
        assert_eq!(response.assignments[1].slot_id, primary.clone());
        assert!(store
            .resolve_slots(&scope(), "owner", &[primary.clone(), primary], &inventory,)
            .is_err());
    }

    #[test]
    fn picker_pages_expose_inventory_revision_and_stale_candidates_cannot_assign() {
        let (_tmp, store) = store();
        let observed = inventory(
            1,
            digest("observed"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        let page = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &observed,
            )
            .unwrap();
        assert_eq!(page.inventory_revision, observed.revision().unwrap());

        let current = inventory(
            2,
            digest("updated"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        assert_ne!(page.inventory_revision, current.revision().unwrap());
        let request = AppSlotAssignmentWriteRequest {
            expected_revision: page.head.revision,
            write_fence: page.head.fence,
            mutation_id: AppReference::parse("slot-mutation:stale-picker").unwrap(),
            command: AppSlotAssignmentCommand::Assign {
                slot_id: slot(),
                installation_id: AppInstallationId::parse("install_1").unwrap(),
                widget_id: AppName::parse("pending_claims").unwrap(),
                expected_candidate: observed.picker[0].widget.clone(),
            },
        };
        assert!(matches!(
            store.apply_user_assignment(&scope(), "owner", &request, &current, Utc::now()),
            Err(AppSlotAssignmentError::WidgetUnavailable)
        ));
    }

    fn store() -> (tempfile::TempDir, AppSlotAssignmentStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = AppSlotAssignmentStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store)
    }

    #[test]
    fn stale_revision_and_superseded_fence_both_refuse() {
        let (_tmp, store) = store();
        let inventory = inventory(
            1,
            digest("same"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        let first = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &inventory,
            )
            .unwrap();
        let second = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &inventory,
            )
            .unwrap();
        let request = AppSlotAssignmentWriteRequest {
            expected_revision: first.head.revision,
            write_fence: first.head.fence,
            mutation_id: AppReference::parse("slot-mutation:1").unwrap(),
            command: AppSlotAssignmentCommand::Assign {
                slot_id: slot(),
                installation_id: AppInstallationId::parse("install_1").unwrap(),
                widget_id: AppName::parse("pending_claims").unwrap(),
                expected_candidate: inventory.picker[0].widget.clone(),
            },
        };
        assert!(matches!(
            store.apply_user_assignment(&scope(), "owner", &request, &inventory, Utc::now()),
            Err(AppSlotAssignmentError::FenceLost { .. })
        ));

        let mut current = request;
        current.write_fence = second.head.fence;
        let applied = store
            .apply_user_assignment(&scope(), "owner", &current, &inventory, Utc::now())
            .unwrap();
        current.mutation_id = AppReference::parse("slot-mutation:2").unwrap();
        assert!(matches!(
            store.apply_user_assignment(&scope(), "owner", &current, &inventory, Utc::now()),
            Err(AppSlotAssignmentError::RevisionConflict { .. })
        ));
        assert_eq!(applied.head.revision.as_u64(), 1);
    }

    #[test]
    fn disabled_hides_and_exact_digest_generation_advance_restores() {
        let (_tmp, store) = store();
        let same_digest = digest("same");
        let enabled = inventory(
            3,
            same_digest.clone(),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        let page = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &enabled,
            )
            .unwrap();
        store
            .apply_user_assignment(
                &scope(),
                "owner",
                &AppSlotAssignmentWriteRequest {
                    expected_revision: page.head.revision,
                    write_fence: page.head.fence,
                    mutation_id: AppReference::parse("slot-mutation:assign").unwrap(),
                    command: AppSlotAssignmentCommand::Assign {
                        slot_id: slot(),
                        installation_id: AppInstallationId::parse("install_1").unwrap(),
                        widget_id: AppName::parse("pending_claims").unwrap(),
                        expected_candidate: enabled.picker[0].widget.clone(),
                    },
                },
                &enabled,
                Utc::now(),
            )
            .unwrap();

        let disabled = inventory(
            4,
            same_digest.clone(),
            AppSlotPackageAvailability::Disabled,
            false,
        );
        let hidden = store
            .resolve_slot(&scope(), "owner", &slot(), &disabled)
            .unwrap();
        assert_eq!(hidden.hidden_reason, Some(AppSlotHiddenReason::Disabled));
        assert!(hidden.widget.is_none());

        let restored = store
            .resolve_slot(
                &scope(),
                "owner",
                &slot(),
                &inventory(5, same_digest, AppSlotPackageAvailability::Enabled, false),
            )
            .unwrap();
        assert!(restored.widget.unwrap().restored_across_generation);
    }

    #[test]
    fn reinstall_with_different_digest_never_inherits_assignment() {
        let (_tmp, store) = store();
        let original = inventory(
            1,
            digest("original"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        let page = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &original,
            )
            .unwrap();
        store
            .apply_user_assignment(
                &scope(),
                "owner",
                &AppSlotAssignmentWriteRequest {
                    expected_revision: page.head.revision,
                    write_fence: page.head.fence,
                    mutation_id: AppReference::parse("slot-mutation:assign").unwrap(),
                    command: AppSlotAssignmentCommand::Assign {
                        slot_id: slot(),
                        installation_id: AppInstallationId::parse("install_1").unwrap(),
                        widget_id: AppName::parse("pending_claims").unwrap(),
                        expected_candidate: original.picker[0].widget.clone(),
                    },
                },
                &original,
                Utc::now(),
            )
            .unwrap();
        let changed = store
            .resolve_slot(
                &scope(),
                "owner",
                &slot(),
                &inventory(
                    2,
                    digest("different"),
                    AppSlotPackageAvailability::Enabled,
                    false,
                ),
            )
            .unwrap();
        assert_eq!(
            changed.hidden_reason,
            Some(AppSlotHiddenReason::PackageDigestChanged)
        );
    }

    /// One package's admitted defaults, wrapped in the whole-boot set the
    /// maintainer consumes.
    fn admitted_set(
        inventory_seed: &str,
        inventory: &AppSlotInventorySnapshot,
        slots: impl IntoIterator<Item = (AppSlotId, AppName)>,
    ) -> TrustedSystemSlotDefaultSet {
        let defaults =
            TrustedSystemSlotDefaults::from_boot_admission(&inventory.packages[0], slots).unwrap();
        TrustedSystemSlotDefaultSet::from_boot_admission(digest(inventory_seed), [defaults])
            .unwrap()
    }

    fn pinned_claims_widget() -> AppName {
        AppName::parse("pending_claims").unwrap()
    }

    /// Two trusted-system packages that both pin one slot and each own another.
    ///
    /// This is the shape the deployment's own seed root shipped until
    /// `town_square` gave the slot up: `meetings` and `town_square` each
    /// declared `page: /, slot: ambient, system_default: true`, while the rest
    /// of their pinned slots were theirs alone. The seed is settled, so this
    /// fixture is now the only place the shape survives — which is the point,
    /// since the behaviour must hold for the next conflicting manifest too.
    fn contested_inventory() -> AppSlotInventorySnapshot {
        let first = binding(1, digest("system"));
        let second = AppSlotPackageBinding {
            installation_id: AppInstallationId::parse("install_2").unwrap(),
            package_id: AppReference::parse("package:rival").unwrap(),
            ..binding(1, digest("rival"))
        };
        let first_widget = pinned_claims_widget();
        let second_widget = AppName::parse("rival_feed").unwrap();
        let review = AppName::parse("review").unwrap();
        let rival_region = AppName::parse("digest").unwrap();
        AppSlotInventorySnapshot {
            packages: vec![
                AppSlotPackageState {
                    binding: first.clone(),
                    availability: AppSlotPackageAvailability::Enabled,
                    declared_widget_ids: vec![first_widget.clone()],
                    trusted_system_provenance: true,
                },
                AppSlotPackageState {
                    binding: second.clone(),
                    availability: AppSlotPackageAvailability::Enabled,
                    declared_widget_ids: vec![second_widget.clone()],
                    trusted_system_provenance: true,
                },
            ],
            picker: vec![
                AppSlotPickerCandidate {
                    widget: AppSlotWidgetBinding {
                        package: first,
                        widget_id: first_widget,
                    },
                    title: "Pending claims".to_owned(),
                    suggested_slots: vec![
                        AppSlotSuggestion::for_page_region("/", review.clone(), true).unwrap(),
                        AppSlotSuggestion::for_page_region("/observe", review.clone(), true)
                            .unwrap(),
                    ],
                    system_class: true,
                },
                AppSlotPickerCandidate {
                    widget: AppSlotWidgetBinding {
                        package: second,
                        widget_id: second_widget,
                    },
                    title: "Rival feed".to_owned(),
                    suggested_slots: vec![
                        AppSlotSuggestion::for_page_region("/", review, true).unwrap(),
                        AppSlotSuggestion::for_page_region("/observe", rival_region, true).unwrap(),
                    ],
                    system_class: true,
                },
            ],
        }
    }

    /// Stands in for the boot owner: answers for exactly the scopes it was
    /// given and refuses every other one, the way an unresolvable scope must.
    struct FixtureDefaultSource {
        admitted: BTreeMap<(String, String), TrustedSystemSlotDefaultSet>,
    }

    impl FixtureDefaultSource {
        fn refusing() -> Self {
            Self {
                admitted: BTreeMap::new(),
            }
        }

        fn answering(
            scopes: impl IntoIterator<Item = (AppScope, TrustedSystemSlotDefaultSet)>,
        ) -> Self {
            Self {
                admitted: scopes
                    .into_iter()
                    .map(|(scope, admitted)| {
                        (
                            (
                                scope.principal.as_str().to_owned(),
                                scope.workspace.as_str().to_owned(),
                            ),
                            admitted,
                        )
                    })
                    .collect(),
            }
        }
    }

    impl TrustedSystemSlotDefaultSource for FixtureDefaultSource {
        fn admitted_defaults(
            &self,
            scope: &AppScope,
        ) -> Result<TrustedSystemSlotDefaultSet, AppSlotAssignmentError> {
            self.admitted
                .get(&(
                    scope.principal.as_str().to_owned(),
                    scope.workspace.as_str().to_owned(),
                ))
                .cloned()
                .ok_or(AppSlotAssignmentError::Invalid(
                    "fixture source admits nothing for this scope",
                ))
        }
    }

    #[test]
    fn pinned_default_survives_user_opt_out_and_requires_trusted_provenance() {
        let (_tmp, store) = store();
        let untrusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            false,
        );
        assert!(TrustedSystemSlotDefaults::from_boot_admission(
            &untrusted.packages[0],
            [(slot(), pinned_claims_widget())],
        )
        .is_err());

        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let admitted = admitted_set("inventory-1", &trusted, [(slot(), pinned_claims_widget())]);
        let maintained = store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();
        assert!(maintained.changed);
        assert_eq!(maintained.pinned_slots, 1);
        assert_eq!(maintained.retired_slots, 0);

        let page = store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &trusted,
            )
            .unwrap();
        let opted_out = store
            .apply_user_assignment(
                &scope(),
                "owner",
                &AppSlotAssignmentWriteRequest {
                    expected_revision: page.head.revision,
                    write_fence: page.head.fence,
                    mutation_id: AppReference::parse("slot-mutation:opt-out").unwrap(),
                    command: AppSlotAssignmentCommand::OptOut { slot_id: slot() },
                },
                &trusted,
                Utc::now(),
            )
            .unwrap();
        assert!(opted_out.assignment.opted_out);
        assert!(opted_out.assignment.pinned_system_default);
        assert!(opted_out.assignment.widget.is_none());
        let fresh_user = store
            .resolve_slot(&scope(), "another-owner", &slot(), &trusted)
            .unwrap();
        assert_eq!(
            fresh_user.source,
            Some(AppSlotAssignmentSource::WorkspaceDefault)
        );
        assert!(fresh_user.widget.is_some());
    }

    #[test]
    fn maintenance_retires_the_defaults_of_a_package_that_left_the_inventory() {
        let (_tmp, store) = store();
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let admitted = admitted_set("inventory-1", &trusted, [(slot(), pinned_claims_widget())]);
        store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();

        // The package is no longer in the next boot's admitted inventory. Its
        // pinned slot must not survive as a default no user is able to clear.
        let departed =
            TrustedSystemSlotDefaultSet::from_boot_admission(digest("inventory-2"), Vec::new())
                .unwrap();
        let maintained = store
            .maintain_system_defaults(&scope(), &departed, Utc::now())
            .unwrap();
        assert!(maintained.changed);
        assert_eq!(maintained.retired_slots, 1);
        assert_eq!(maintained.pinned_slots, 0);
        let resolved = store
            .resolve_slot(&scope(), "owner", &slot(), &trusted)
            .unwrap();
        assert!(!resolved.pinned_system_default);
        assert_eq!(resolved.source, None);
        assert!(resolved.widget.is_none());
    }

    #[test]
    fn re_maintaining_an_identical_admitted_set_changes_no_head() {
        let (_tmp, store) = store();
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let admitted = admitted_set("inventory-1", &trusted, [(slot(), pinned_claims_widget())]);
        let first = store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();
        let second = store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();
        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.head, second.head);
    }

    #[test]
    fn an_admitted_set_refuses_a_repeated_package_and_unpins_a_contested_slot() {
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let defaults = TrustedSystemSlotDefaults::from_boot_admission(
            &trusted.packages[0],
            [(slot(), pinned_claims_widget())],
        )
        .unwrap();

        // Same package id, disjoint slots: the repeat itself is what refuses,
        // not an incidental slot collision.
        let mut repeated = defaults.clone();
        repeated.defaults = BTreeMap::from([(
            AppSlotId::for_page_region("/observe", &AppName::parse("review").unwrap()).unwrap(),
            defaults.defaults.values().next().unwrap().clone(),
        )]);
        assert!(TrustedSystemSlotDefaultSet::from_boot_admission(
            digest("inventory-1"),
            [defaults.clone(), repeated],
        )
        .is_err());

        // Two packages over one slot is a deployment authoring conflict, not a
        // malformed admission: the slot goes to nobody and the set still mints.
        let mut rival = defaults.clone();
        rival.package.package_id = AppReference::parse("package:rival").unwrap();
        let contested = TrustedSystemSlotDefaultSet::from_boot_admission(
            digest("inventory-1"),
            [defaults, rival],
        )
        .expect("a contested slot is not a malformed admission");
        assert_eq!(
            contested.contested_slots(),
            &BTreeMap::from([(
                slot(),
                BTreeSet::from([
                    AppReference::parse("package:claims").unwrap(),
                    AppReference::parse("package:rival").unwrap(),
                ])
            )])
        );
        assert!(contested.pinned_defaults().is_empty());
    }

    #[test]
    fn the_maintainer_reaches_every_stored_scope_and_the_default_scope() {
        let (_tmp, store) = store();
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        // Materialize a second scope on disk the way a deployment does — by
        // writing its slot document — so the fan-out has something to find.
        let other = AppScope {
            principal: AppReference::parse("other-principal").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        };
        let owner = scope();
        for target in [&owner, &other] {
            store
                .acquire_settings_page(target, "owner", &AppSlotSettingsQuery::default(), &trusted)
                .unwrap();
        }

        let default_scope = AppScope {
            principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).unwrap(),
            workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).unwrap(),
        };
        let source =
            FixtureDefaultSource::answering([&owner, &other, &default_scope].map(|target| {
                (
                    target.clone(),
                    admitted_set("inventory-1", &trusted, [(slot(), pinned_claims_widget())]),
                )
            }));
        let outcomes = AppSlotSystemDefaultsMaintainer::new(store.clone())
            .maintain_every_scope(&source, Utc::now());
        assert!(outcomes.iter().all(AppSlotSystemDefaultsOutcome::succeeded));
        let reached: BTreeSet<_> = outcomes
            .iter()
            .map(|outcome| (outcome.principal.as_str(), outcome.workspace.as_str()))
            .collect();
        assert!(reached.contains(&("owner", "default")));
        assert!(reached.contains(&("other-principal", "default")));
        assert!(reached.contains(&(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)));
        for outcome in &outcomes {
            assert_eq!(outcome.result.as_ref().unwrap().pinned_slots, 1);
        }
        // Every user of a scope the maintainer reached sees the pinned default,
        // including one that has never opened the settings page.
        assert_eq!(
            store
                .resolve_slot(&other, "someone-else", &slot(), &trusted)
                .unwrap()
                .source,
            Some(AppSlotAssignmentSource::WorkspaceDefault)
        );
    }

    #[test]
    fn a_scope_whose_admitted_set_cannot_be_resolved_keeps_its_defaults() {
        let (_tmp, store) = store();
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let admitted = admitted_set("inventory-1", &trusted, [(slot(), pinned_claims_widget())]);
        store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();

        // A boot that cannot resolve the scope must retire nothing. Retiring on
        // an unresolved inventory would delete a pinned default every time the
        // seed root was briefly unreadable.
        let outcomes = AppSlotSystemDefaultsMaintainer::new(store.clone())
            .maintain_every_scope(&FixtureDefaultSource::refusing(), Utc::now());
        assert!(outcomes.iter().all(|outcome| !outcome.succeeded()));
        let resolved = store
            .resolve_slot(&scope(), "owner", &slot(), &trusted)
            .unwrap();
        assert!(resolved.pinned_system_default);
        assert_eq!(
            resolved.source,
            Some(AppSlotAssignmentSource::WorkspaceDefault)
        );
    }

    #[test]
    fn boot_admitted_defaults_answer_only_the_scopes_the_boot_resolved() {
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        let mut resolved = BootAdmittedSlotDefaults::new();
        assert!(resolved.is_empty());
        resolved.record(
            &scope(),
            TrustedSystemSlotDefaultSet::from_boot_inventory(digest("inventory-1"), &trusted)
                .unwrap(),
        );
        assert!(!resolved.is_empty());
        assert!(resolved.admitted_defaults(&scope()).is_ok());

        // A scope this boot never reached must refuse, not answer an empty
        // set: maintenance retires every default a set omits, so answering
        // for a scope we never resolved would clear it.
        let never_resolved = AppScope {
            principal: AppReference::parse("other-principal").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        };
        assert!(resolved.admitted_defaults(&never_resolved).is_err());
    }

    /// One seed-authoring conflict must not blank the whole deployment.
    ///
    /// The shipped seed root once had two `distribution: system` packages both
    /// declaring `page: /, slot: ambient, system_default: true`, so while this
    /// refused the whole set, `from_boot_inventory` returned `Err` for every
    /// scope on every boot, the boot owner recorded no scope, the maintainer
    /// never ran, and `workspace_defaults` stayed empty in every running
    /// binary — the entire pinned-default mechanism off, for one line in one
    /// manifest. Only the contested slot may cost anything.
    #[test]
    fn a_contested_slot_costs_only_itself_and_the_rest_reach_a_user() {
        let (_tmp, store) = store();
        let inventory = contested_inventory();
        let admitted =
            TrustedSystemSlotDefaultSet::from_boot_inventory(digest("inventory-1"), &inventory)
                .expect("a contested slot is not a failed inventory resolve");

        let review = AppName::parse("review").unwrap();
        let contested = AppSlotId::for_page_region("/", &review).unwrap();
        let uncontested = AppSlotId::for_page_region("/observe", &review).unwrap();
        // Both claimants are named, not just the slot: the boot owner's warning
        // is the operator's only signal here, and it has to point at the two
        // manifests whose author must settle the conflict.
        assert_eq!(
            admitted.contested_slots(),
            &BTreeMap::from([(
                contested.clone(),
                BTreeSet::from([
                    AppReference::parse("package:claims").unwrap(),
                    AppReference::parse("package:rival").unwrap(),
                ])
            )])
        );
        let pinned = admitted.pinned_defaults();
        assert!(!pinned.contains_key(&contested));
        assert_eq!(pinned.len(), 2);

        let maintained = store
            .maintain_system_defaults(&scope(), &admitted, Utc::now())
            .unwrap();
        assert_eq!(maintained.pinned_slots, 2);

        // The slot each package owns alone reaches a user who has never opened
        // settings; the slot they both wanted is pinned to nobody rather than
        // to whichever package the resolver happened to read first.
        let resolved = store
            .resolve_slot(&scope(), "someone", &uncontested, &inventory)
            .unwrap();
        assert!(resolved.pinned_system_default);
        assert_eq!(
            resolved.source,
            Some(AppSlotAssignmentSource::WorkspaceDefault)
        );
        let unpinned = store
            .resolve_slot(&scope(), "someone", &contested, &inventory)
            .unwrap();
        assert!(!unpinned.pinned_system_default);
        assert_eq!(unpinned.source, None);
    }

    /// The boot path end to end, minus the await-ing that fetches the
    /// inventory: a host-resolved snapshot becomes an admitted set, the set
    /// reaches the maintainer through the source the boot owner hands it, and
    /// a user who has never opened settings sees the pinned widget.
    ///
    /// This pins the maintainer and its source trait together, which spent a
    /// review cycle compiling with no production implementor at all. It cannot
    /// pin the boot call site: that lives in `magician-api`, which depends on
    /// this crate, so deleting `AppPlatformApi::maintain_system_slot_defaults`
    /// still leaves this passing. That pin has to be an integration test beside
    /// `admit_system_packages_at_boot`.
    #[test]
    fn a_resolved_inventory_reaches_a_user_as_a_pinned_workspace_default() {
        let (_tmp, store) = store();
        let trusted = inventory(
            1,
            digest("system"),
            AppSlotPackageAvailability::Enabled,
            true,
        );
        // Give the scope a document on disk, the way a deployment that has
        // been opened once has, so the maintainer enumerates it.
        store
            .acquire_settings_page(
                &scope(),
                "owner",
                &AppSlotSettingsQuery::default(),
                &trusted,
            )
            .unwrap();

        let mut resolved = BootAdmittedSlotDefaults::new();
        resolved.record(
            &scope(),
            TrustedSystemSlotDefaultSet::from_boot_inventory(digest("inventory-1"), &trusted)
                .unwrap(),
        );
        let outcomes = AppSlotSystemDefaultsMaintainer::new(store.clone())
            .maintain_every_scope(&resolved, Utc::now());
        let maintained = outcomes
            .iter()
            .find(|outcome| outcome.principal == "owner" && outcome.workspace == "default")
            .expect("the stored scope is enumerated")
            .result
            .as_ref()
            .expect("a recorded scope resolves");
        assert_eq!(maintained.pinned_slots, 1);
        assert!(maintained.changed);

        let resolution = store
            .resolve_slot(
                &scope(),
                "someone-who-never-opened-settings",
                &slot(),
                &trusted,
            )
            .unwrap();
        assert!(resolution.pinned_system_default);
        assert_eq!(
            resolution.source,
            Some(AppSlotAssignmentSource::WorkspaceDefault)
        );
    }
}
