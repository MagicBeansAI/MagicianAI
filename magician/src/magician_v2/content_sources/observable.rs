//! Declarative, user-meaningful sources projected onto Observe.
//!
//! Acquisition actions answer "how". These manifests answer "what" and bind
//! an explicitly Observe-safe profile to exact registered actions. Projection
//! is performed by the backend on every read and run so clients cannot infer
//! eligibility or widen authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    public_http::validate_public_http_url, types::validate_content_scope_component,
    ContentAcquisitionCatalog, ContentSourceDescriptor, RetrievalAuthority, RetrievalOperation,
};
use crate::magician_v2::skills::embedded_extensions::{
    discover_skill_markdown_paths_isolated, load_skill_magician_extension,
};

pub const OBSERVE_SOURCE_SCHEMA_VERSION: u32 = 1;
pub const OBSERVE_SOURCE_EXTENSION: &str = "observe_source";
const MAX_SOURCE_ID_CHARS: usize = 96;
const MAX_PROFILE_ID_CHARS: usize = 96;
const MAX_DISPLAY_NAME_CHARS: usize = 160;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const MAX_CATEGORY_CHARS: usize = 64;
const MAX_PROFILES: usize = 16;
const MAX_ACTIONS_PER_PROFILE: usize = 8;
const MAX_TARGETS_PER_PROFILE: usize = 16;
const MAX_TARGET_CHARS: usize = 8 * 1024;
const MAX_CATALOG_DEFINITIONS: usize = 2_048;
const MAX_CANDIDATES_PER_RUN: usize = 200;
const MAX_SELECTED_PER_RUN: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableSourceSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_scheduler_interval_secs")]
    pub scheduler_interval_secs: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_lease_ttl_secs")]
    pub lease_ttl_secs: u64,
    #[serde(default)]
    pub source_catalog: ObservableSourcePolicy,
}

impl Default for ObservableSourceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            scheduler_interval_secs: default_scheduler_interval_secs(),
            max_concurrency: default_max_concurrency(),
            lease_ttl_secs: default_lease_ttl_secs(),
            source_catalog: ObservableSourcePolicy::default(),
        }
    }
}

impl ObservableSourceSettings {
    pub fn validate_bounds(&self) -> Result<()> {
        if self.scheduler_interval_secs == 0 || self.scheduler_interval_secs > 3_600 {
            bail!("observable source scheduler interval must be between 1 and 3600 seconds");
        }
        if self.max_concurrency == 0 || self.max_concurrency > 16 {
            bail!("observable source concurrency must be between 1 and 16");
        }
        if self.lease_ttl_secs < 30 || self.lease_ttl_secs > 3_600 {
            bail!("observable source lease TTL must be between 30 and 3600 seconds");
        }
        self.source_catalog.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableSourcePolicy {
    #[serde(default = "default_allowed_actions")]
    pub allowed_actions: Vec<String>,
    #[serde(default = "default_maximum_authority")]
    pub maximum_authority: RetrievalAuthority,
    #[serde(default)]
    pub allow_metered: bool,
    #[serde(default)]
    pub allow_browser: bool,
}

impl Default for ObservableSourcePolicy {
    fn default() -> Self {
        Self {
            allowed_actions: default_allowed_actions(),
            maximum_authority: default_maximum_authority(),
            allow_metered: false,
            allow_browser: false,
        }
    }
}

impl ObservableSourcePolicy {
    fn validate(&self) -> Result<()> {
        if self.allowed_actions.is_empty() || self.allowed_actions.len() > 64 {
            bail!("observable source policy requires between 1 and 64 actions");
        }
        let mut unique = BTreeSet::new();
        for action in &self.allowed_actions {
            validate_stable_id(action, "observable source policy action", 128, true)?;
            if !unique.insert(action) {
                bail!("observable source policy has duplicate action `{action}`");
            }
        }
        Ok(())
    }

    pub fn admits(&self, descriptor: &ContentSourceDescriptor) -> bool {
        self.allowed_actions
            .iter()
            .any(|action| action == &descriptor.retrieval.action_id)
            && authority_rank(descriptor.retrieval.authority)
                <= authority_rank(self.maximum_authority)
            && (self.allow_metered || !descriptor.capabilities.metered)
            && (self.allow_browser || !is_browser_descriptor(descriptor))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableSourceManifest {
    pub schema_version: u32,
    pub source: ObservableSourceDefinition,
    pub profiles: Vec<ObservationProfile>,
}

impl ObservableSourceManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != OBSERVE_SOURCE_SCHEMA_VERSION {
            bail!(
                "unsupported observable source schema version {}",
                self.schema_version
            );
        }
        self.source.validate()?;
        if self.profiles.is_empty() || self.profiles.len() > MAX_PROFILES {
            bail!("observable source must declare between 1 and {MAX_PROFILES} profiles");
        }
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(profile.id.as_str()) {
                bail!("observable source has duplicate profile `{}`", profile.id);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableSourceDefinition {
    pub id: String,
    pub display_name: String,
    pub category: String,
    pub description: String,
}

impl ObservableSourceDefinition {
    fn validate(&self) -> Result<()> {
        validate_stable_id(&self.id, "observable source id", MAX_SOURCE_ID_CHARS, false)?;
        validate_text(
            &self.display_name,
            "observable source display name",
            MAX_DISPLAY_NAME_CHARS,
        )?;
        validate_stable_id(
            &self.category,
            "observable source category",
            MAX_CATEGORY_CHARS,
            false,
        )?;
        validate_text(
            &self.description,
            "observable source description",
            MAX_DESCRIPTION_CHARS,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSurface {
    Observe,
    Research,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationEscalation {
    None,
    Ladder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProfile {
    pub id: String,
    #[serde(default)]
    pub surfaces: Vec<ObservationSurface>,
    #[serde(default)]
    pub discoverable: bool,
    #[serde(default)]
    pub unattended: bool,
    #[serde(default)]
    pub read_only: bool,
    pub operation: RetrievalOperation,
    pub acquisition: ObservationProfileAcquisition,
    pub schedule: ObservationSchedule,
    #[serde(default)]
    pub limits: ObservationProfileLimits,
}

impl ObservationProfile {
    fn validate(&self) -> Result<()> {
        validate_stable_id(
            &self.id,
            "observable source profile id",
            MAX_PROFILE_ID_CHARS,
            false,
        )?;
        if self.surfaces.is_empty() {
            bail!("observable source profile must declare at least one surface");
        }
        self.acquisition.validate()?;
        self.schedule.validate()?;
        self.limits.validate()?;
        if self.surfaces.contains(&ObservationSurface::Observe) {
            if self.operation != RetrievalOperation::Discover {
                bail!("Observe profiles must use the discover operation");
            }
            if self.acquisition.escalation != ObservationEscalation::None {
                bail!("Observe profiles must disable acquisition escalation");
            }
            if self.acquisition.allowed_actions.len() != 1 {
                bail!("Observe schema v1 profiles require exactly one action");
            }
            if self.acquisition.ladder.is_some() {
                bail!("Observe profiles cannot reference a retrieval ladder");
            }
            // Empty targets are valid only as a visible needs-setup offer.
            // Subscription creation still requires a concrete validated target.
        }
        Ok(())
    }

    pub fn is_observe_candidate(&self) -> bool {
        self.surfaces.contains(&ObservationSurface::Observe) && self.discoverable
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProfileAcquisition {
    #[serde(default)]
    pub allowed_actions: Vec<String>,
    #[serde(default = "default_escalation")]
    pub escalation: ObservationEscalation,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_authority: Option<RetrievalAuthority>,
}

impl ObservationProfileAcquisition {
    fn validate(&self) -> Result<()> {
        if self.allowed_actions.len() > MAX_ACTIONS_PER_PROFILE {
            bail!("observable source profile declares too many actions");
        }
        let mut actions = BTreeSet::new();
        for action in &self.allowed_actions {
            validate_stable_id(action, "observable source action", 128, true)?;
            if !actions.insert(action) {
                bail!("observable source profile has duplicate action `{action}`");
            }
        }
        if self.targets.len() > MAX_TARGETS_PER_PROFILE {
            bail!("observable source profile declares too many targets");
        }
        for target in &self.targets {
            validate_text(target, "observable source target", MAX_TARGET_CHARS)?;
            let url = url::Url::parse(target).context("observable source target is not a URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                bail!("observable source targets must be credential-free HTTP(S) URLs");
            }
        }
        if let Some(ladder) = self.ladder.as_deref() {
            validate_stable_id(ladder, "observable source ladder", 128, false)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCadence {
    Hourly,
    TwiceDaily,
    Daily,
}

impl ObservationCadence {
    pub fn interval_ms(self) -> i64 {
        match self {
            Self::Hourly => 60 * 60 * 1_000,
            Self::TwiceDaily => 12 * 60 * 60 * 1_000,
            Self::Daily => 24 * 60 * 60 * 1_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSchedule {
    pub default: ObservationCadence,
    pub allowed: Vec<ObservationCadence>,
}

impl ObservationSchedule {
    fn validate(&self) -> Result<()> {
        if self.allowed.is_empty() || self.allowed.len() > 3 {
            bail!("observable source schedule requires between 1 and 3 cadences");
        }
        let unique = self.allowed.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != self.allowed.len() || !unique.contains(&self.default) {
            bail!("observable source schedule must contain its default exactly once");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProfileLimits {
    #[serde(default = "default_max_candidates")]
    pub max_candidates_per_run: usize,
    #[serde(default = "default_max_selected")]
    pub max_selected_per_run: usize,
}

impl Default for ObservationProfileLimits {
    fn default() -> Self {
        Self {
            max_candidates_per_run: default_max_candidates(),
            max_selected_per_run: default_max_selected(),
        }
    }
}

impl ObservationProfileLimits {
    fn validate(&self) -> Result<()> {
        if self.max_candidates_per_run == 0
            || self.max_candidates_per_run > MAX_CANDIDATES_PER_RUN
            || self.max_selected_per_run == 0
            || self.max_selected_per_run > MAX_SELECTED_PER_RUN
            || self.max_selected_per_run > self.max_candidates_per_run
        {
            bail!("observable source profile limits are outside hard bounds");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LoadedObservableSource {
    pub manifest: ObservableSourceManifest,
    pub revision: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservableCatalogIssueClass {
    ScanFailed,
    ManifestInvalid,
    DuplicateSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservableCatalogIssue {
    pub source_id: String,
    pub error_class: ObservableCatalogIssueClass,
}

#[derive(Debug, Clone, Default)]
pub struct ObservableCatalog {
    pub definitions: BTreeMap<String, LoadedObservableSource>,
    pub issues: Vec<ObservableCatalogIssue>,
    pub revision: String,
}

pub fn load_observable_source_manifest(path: &Path) -> Result<ObservableSourceManifest> {
    load_optional_observable_source_manifest(path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "governed skill `{}` does not declare metadata.magician.{OBSERVE_SOURCE_EXTENSION}",
            path.display()
        )
    })
}

fn load_optional_observable_source_manifest(
    path: &Path,
) -> Result<Option<ObservableSourceManifest>> {
    let Some(manifest) =
        load_skill_magician_extension::<ObservableSourceManifest>(path, OBSERVE_SOURCE_EXTENSION)?
    else {
        return Ok(None);
    };
    manifest.validate()?;
    Ok(Some(manifest))
}

/// Discover manifests from roots ordered highest precedence first. A source id
/// in an earlier root shadows the same id in later roots. Invalid definitions
/// are isolated and reported without making healthy sources disappear.
pub fn discover_observable_sources(roots: &[PathBuf]) -> ObservableCatalog {
    let mut catalog = ObservableCatalog::default();
    let mut claimed = BTreeSet::new();
    let (paths, discovery_issues) = discover_skill_markdown_paths_isolated(roots);
    catalog.issues.extend(
        discovery_issues
            .into_iter()
            .map(|issue| ObservableCatalogIssue {
                source_id: issue.skill_name,
                error_class: ObservableCatalogIssueClass::ScanFailed,
            }),
    );
    for path in paths {
        if catalog.definitions.len() >= MAX_CATALOG_DEFINITIONS {
            break;
        }
        let manifest = match load_optional_observable_source_manifest(&path) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => continue,
            Err(_) => {
                catalog.issues.push(ObservableCatalogIssue {
                    source_id: source_hint(&path),
                    error_class: ObservableCatalogIssueClass::ManifestInvalid,
                });
                continue;
            },
        };
        let id = manifest.source.id.clone();
        if !claimed.insert(id.clone()) {
            continue;
        }
        let canonical = serde_json::to_vec(&manifest).unwrap_or_default();
        let revision = blake3::hash(&canonical).to_hex().to_string();
        catalog.definitions.insert(
            id,
            LoadedObservableSource {
                manifest,
                revision,
                path,
            },
        );
    }
    let mut revision_body = Vec::new();
    for (id, source) in &catalog.definitions {
        revision_body.extend_from_slice(id.as_bytes());
        revision_body.extend_from_slice(source.revision.as_bytes());
    }
    catalog.revision = blake3::hash(&revision_body).to_hex().to_string();
    catalog
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservableSourceReadiness {
    Eligible,
    NeedsSetup,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservableUnavailableReason {
    FeatureDisabled,
    ProfileNotUnattended,
    ProfileNotReadOnly,
    OperationMismatch,
    UnknownAction,
    ActionNotAllowed,
    AuthorityDenied,
    MeteredDenied,
    BrowserDenied,
    TargetDenied,
    TargetUnsupported,
    MissingConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableActionBinding {
    pub action_id: String,
    pub adapter_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservableSourceOffer {
    pub offer_id: String,
    pub source_id: String,
    pub source_revision: String,
    pub profile_id: String,
    pub profile_revision: String,
    pub display_name: String,
    pub category: String,
    pub description: String,
    pub readiness: ObservableSourceReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<ObservableUnavailableReason>,
    pub subscribed: bool,
    pub supported_cadence: Vec<ObservationCadence>,
    pub default_cadence: ObservationCadence,
    pub limits: ObservationProfileLimits,
    pub targets: Vec<String>,
    pub action_bindings: Vec<ObservableActionBinding>,
}

pub fn project_observable_source_offers(
    sources: &ObservableCatalog,
    actions: &ContentAcquisitionCatalog,
    policy: &ObservableSourcePolicy,
    subscribed_profiles: &BTreeSet<(String, String)>,
    required_action: Option<&str>,
) -> Vec<ObservableSourceOffer> {
    let descriptors = actions
        .discovery
        .iter()
        .chain(actions.readers.iter())
        .map(|descriptor| (descriptor.retrieval.action_id.as_str(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let mut offers = Vec::new();
    for loaded in sources.definitions.values() {
        for profile in &loaded.manifest.profiles {
            if !profile.is_observe_candidate()
                || required_action.is_some_and(|required| {
                    !profile
                        .acquisition
                        .allowed_actions
                        .iter()
                        .any(|action| action == required)
                })
            {
                continue;
            }
            let (readiness, unavailable_reason, bindings) =
                project_profile(profile, &descriptors, policy);
            let profile_revision =
                blake3::hash(format!("{}:{}", loaded.revision, profile.id).as_bytes())
                    .to_hex()
                    .to_string();
            offers.push(ObservableSourceOffer {
                offer_id: format!("{}:{}", loaded.manifest.source.id, profile.id),
                source_id: loaded.manifest.source.id.clone(),
                source_revision: loaded.revision.clone(),
                profile_id: profile.id.clone(),
                profile_revision,
                display_name: loaded.manifest.source.display_name.clone(),
                category: loaded.manifest.source.category.clone(),
                description: loaded.manifest.source.description.clone(),
                readiness,
                unavailable_reason,
                subscribed: subscribed_profiles
                    .contains(&(loaded.manifest.source.id.clone(), profile.id.clone())),
                supported_cadence: profile.schedule.allowed.clone(),
                default_cadence: profile.schedule.default,
                limits: profile.limits.clone(),
                targets: profile.acquisition.targets.clone(),
                action_bindings: bindings,
            });
        }
    }
    offers.sort_by(|a, b| {
        a.display_name
            .to_ascii_lowercase()
            .cmp(&b.display_name.to_ascii_lowercase())
            .then_with(|| a.profile_id.cmp(&b.profile_id))
    });
    offers
}

fn project_profile(
    profile: &ObservationProfile,
    descriptors: &BTreeMap<&str, &ContentSourceDescriptor>,
    policy: &ObservableSourcePolicy,
) -> (
    ObservableSourceReadiness,
    Option<ObservableUnavailableReason>,
    Vec<ObservableActionBinding>,
) {
    let reject = |reason| {
        (
            ObservableSourceReadiness::Unavailable,
            Some(reason),
            Vec::new(),
        )
    };
    if !profile.unattended {
        return reject(ObservableUnavailableReason::ProfileNotUnattended);
    }
    if !profile.read_only {
        return reject(ObservableUnavailableReason::ProfileNotReadOnly);
    }
    if profile.operation != RetrievalOperation::Discover {
        return reject(ObservableUnavailableReason::OperationMismatch);
    }
    let mut bindings = Vec::new();
    for action in &profile.acquisition.allowed_actions {
        let Some(descriptor) = descriptors.get(action.as_str()).copied() else {
            return reject(ObservableUnavailableReason::UnknownAction);
        };
        if descriptor.retrieval.operation != profile.operation {
            return reject(ObservableUnavailableReason::OperationMismatch);
        }
        if !policy
            .allowed_actions
            .iter()
            .any(|allowed| allowed == action)
        {
            return reject(ObservableUnavailableReason::ActionNotAllowed);
        }
        let profile_authority = profile
            .acquisition
            .maximum_authority
            .unwrap_or(policy.maximum_authority);
        if authority_rank(descriptor.retrieval.authority) > authority_rank(policy.maximum_authority)
            || authority_rank(descriptor.retrieval.authority) > authority_rank(profile_authority)
        {
            return reject(ObservableUnavailableReason::AuthorityDenied);
        }
        if descriptor.capabilities.metered && !policy.allow_metered {
            return reject(ObservableUnavailableReason::MeteredDenied);
        }
        if is_browser_descriptor(descriptor) && !policy.allow_browser {
            return reject(ObservableUnavailableReason::BrowserDenied);
        }
        if !policy.admits(descriptor) {
            return reject(ObservableUnavailableReason::ActionNotAllowed);
        }
        if !profile.acquisition.targets.is_empty() && !descriptor.retrieval.accepts_targets {
            return reject(ObservableUnavailableReason::TargetUnsupported);
        }
        bindings.push(ObservableActionBinding {
            action_id: action.clone(),
            adapter_id: descriptor.adapter_id.clone(),
        });
    }
    if profile.acquisition.targets.is_empty() {
        return (
            ObservableSourceReadiness::NeedsSetup,
            Some(ObservableUnavailableReason::MissingConfiguration),
            bindings,
        );
    }
    if profile
        .acquisition
        .targets
        .iter()
        .any(|target| validate_public_http_url(target).is_err())
    {
        return reject(ObservableUnavailableReason::TargetDenied);
    }
    (ObservableSourceReadiness::Eligible, None, bindings)
}

fn source_hint(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn validate_text(value: &str, label: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        bail!("{label} must be non-empty, trimmed, bounded, and contain no controls");
    }
    Ok(())
}

fn validate_stable_id(value: &str, label: &str, max_chars: usize, allow_dot: bool) -> Result<()> {
    validate_content_scope_component(value, label)?;
    if value.chars().count() > max_chars
        || !value.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || (allow_dot && ch == '.')
        })
    {
        bail!("{label} has an invalid stable identifier");
    }
    Ok(())
}

fn authority_rank(authority: RetrievalAuthority) -> u8 {
    match authority {
        RetrievalAuthority::LocalOnly => 0,
        RetrievalAuthority::PublicRemoteRead => 1,
        RetrievalAuthority::PublicBrowserRead => 2,
        RetrievalAuthority::PublicBrowserInteract => 3,
        RetrievalAuthority::AuthenticatedRead => 4,
        RetrievalAuthority::AuthenticatedInteract => 5,
    }
}

fn is_browser_descriptor(descriptor: &ContentSourceDescriptor) -> bool {
    descriptor.retrieval.action_id.starts_with("browser.")
        || authority_rank(descriptor.retrieval.authority)
            >= authority_rank(RetrievalAuthority::PublicBrowserRead)
}

fn default_allowed_actions() -> Vec<String> {
    vec!["rss.discover".to_string()]
}

fn default_maximum_authority() -> RetrievalAuthority {
    RetrievalAuthority::PublicRemoteRead
}

fn default_escalation() -> ObservationEscalation {
    ObservationEscalation::None
}

fn default_true() -> bool {
    true
}

fn default_scheduler_interval_secs() -> u64 {
    30
}

fn default_max_concurrency() -> usize {
    2
}

fn default_lease_ttl_secs() -> u64 {
    600
}

fn default_max_candidates() -> usize {
    50
}

fn default_max_selected() -> usize {
    10
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::content_sources::{
        AdapterAuth, AdapterExecution, ContentSourceCapabilities, ContentSourceClass,
        RetrievalActionMetadata, RetrievalRung,
    };

    fn rss_descriptor() -> ContentSourceDescriptor {
        let mut retrieval = RetrievalActionMetadata::discovery(
            "rss.discover",
            RetrievalRung::SourceNative,
            RetrievalAuthority::PublicRemoteRead,
            true,
        );
        retrieval.accepts_targets = true;
        retrieval.requires_targets = true;
        ContentSourceDescriptor {
            adapter_id: "rss".into(),
            display_name: "RSS".into(),
            class: ContentSourceClass::Syndication,
            capabilities: ContentSourceCapabilities {
                discovery: true,
                full_content: false,
                cursor: false,
                conditional_fetch: false,
                execution: AdapterExecution::RemoteEndpoint,
                auth: AdapterAuth::None,
                sends_user_intent: false,
                metered: false,
            },
            retrieval,
        }
    }

    fn manifest(id: &str, action: &str) -> ObservableSourceManifest {
        ObservableSourceManifest {
            schema_version: 1,
            source: ObservableSourceDefinition {
                id: id.into(),
                display_name: "Product Hunt".into(),
                category: "products".into(),
                description: "New products".into(),
            },
            profiles: vec![ObservationProfile {
                id: "observe-rss".into(),
                surfaces: vec![ObservationSurface::Observe],
                discoverable: true,
                unattended: true,
                read_only: true,
                operation: RetrievalOperation::Discover,
                acquisition: ObservationProfileAcquisition {
                    allowed_actions: vec![action.into()],
                    escalation: ObservationEscalation::None,
                    targets: vec!["https://example.com/feed.xml".into()],
                    ladder: None,
                    maximum_authority: None,
                },
                schedule: ObservationSchedule {
                    default: ObservationCadence::Hourly,
                    allowed: vec![ObservationCadence::Hourly, ObservationCadence::Daily],
                },
                limits: ObservationProfileLimits::default(),
            }],
        }
    }

    fn write_observable_skill(
        directory: &Path,
        skill_name: &str,
        value: &ObservableSourceManifest,
    ) {
        std::fs::create_dir_all(directory).unwrap();
        let extension = serde_yaml::to_string(value)
            .unwrap()
            .lines()
            .map(|line| format!("      {line}\n"))
            .collect::<String>();
        std::fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {skill_name}\ndescription: observable fixture\nmetadata:\n  \
                 magician:\n    observe_source:\n{extension}---\nFixture.\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn scoped_root_shadows_later_source_definition() {
        let scoped = tempdir().unwrap();
        let system = tempdir().unwrap();
        for (root, name) in [(scoped.path(), "Scoped"), (system.path(), "System")] {
            let skill = root.join("producthunt");
            let mut value = manifest("product-hunt", "rss.discover");
            value.source.display_name = name.into();
            write_observable_skill(&skill, "producthunt", &value);
        }
        let catalog = discover_observable_sources(&[
            scoped.path().to_path_buf(),
            system.path().to_path_buf(),
        ]);
        assert_eq!(catalog.definitions.len(), 1);
        assert_eq!(
            catalog.definitions["product-hunt"]
                .manifest
                .source
                .display_name,
            "Scoped"
        );
    }

    #[test]
    fn rss_only_policy_never_projects_search_or_browser_as_eligible() {
        let mut catalog = ObservableCatalog::default();
        let definition = manifest("product-hunt", "exa.discover");
        catalog.definitions.insert(
            "product-hunt".into(),
            LoadedObservableSource {
                manifest: definition,
                revision: "rev".into(),
                path: PathBuf::new(),
            },
        );
        let mut exa = rss_descriptor();
        exa.adapter_id = "exa".into();
        exa.retrieval.action_id = "exa.discover".into();
        exa.retrieval.authority = RetrievalAuthority::PublicRemoteRead;
        exa.capabilities.metered = true;
        let actions = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "c".into(),
            discovery: vec![rss_descriptor(), exa],
            readers: vec![],
            unavailable: vec![],
        };
        let offers = project_observable_source_offers(
            &catalog,
            &actions,
            &ObservableSourcePolicy::default(),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].readiness, ObservableSourceReadiness::Unavailable);
        assert_eq!(
            offers[0].unavailable_reason,
            Some(ObservableUnavailableReason::ActionNotAllowed)
        );
    }

    #[test]
    fn readiness_reasons_distinguish_unknown_metered_browser_and_setup_failures() {
        let mut metered = rss_descriptor();
        metered.adapter_id = "metered".into();
        metered.retrieval.action_id = "metered.discover".into();
        metered.capabilities.metered = true;
        let mut browser = rss_descriptor();
        browser.adapter_id = "browser".into();
        browser.retrieval.action_id = "browser.observe".into();
        browser.retrieval.authority = RetrievalAuthority::PublicBrowserRead;
        let actions = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "c".into(),
            discovery: vec![rss_descriptor(), metered, browser],
            readers: vec![],
            unavailable: vec![],
        };
        let mut catalog = ObservableCatalog::default();
        for (source_id, action) in [
            ("unknown", "missing.discover"),
            ("metered", "metered.discover"),
            ("browser", "browser.observe"),
        ] {
            catalog.definitions.insert(
                source_id.into(),
                LoadedObservableSource {
                    manifest: manifest(source_id, action),
                    revision: format!("{source_id}-revision"),
                    path: PathBuf::new(),
                },
            );
        }
        let mut setup = manifest("setup", "rss.discover");
        setup.profiles[0].acquisition.targets.clear();
        catalog.definitions.insert(
            "setup".into(),
            LoadedObservableSource {
                manifest: setup,
                revision: "setup-revision".into(),
                path: PathBuf::new(),
            },
        );
        let policy = ObservableSourcePolicy {
            allowed_actions: vec![
                "rss.discover".into(),
                "metered.discover".into(),
                "browser.observe".into(),
            ],
            maximum_authority: RetrievalAuthority::PublicBrowserRead,
            allow_metered: false,
            allow_browser: false,
        };

        let offers =
            project_observable_source_offers(&catalog, &actions, &policy, &BTreeSet::new(), None);
        let reason = |source_id: &str| {
            offers
                .iter()
                .find(|offer| offer.source_id == source_id)
                .and_then(|offer| offer.unavailable_reason.clone())
        };
        assert_eq!(
            reason("unknown"),
            Some(ObservableUnavailableReason::UnknownAction)
        );
        assert_eq!(
            reason("metered"),
            Some(ObservableUnavailableReason::MeteredDenied)
        );
        assert_eq!(
            reason("browser"),
            Some(ObservableUnavailableReason::BrowserDenied)
        );
        assert_eq!(
            offers
                .iter()
                .find(|offer| offer.source_id == "setup")
                .unwrap()
                .readiness,
            ObservableSourceReadiness::NeedsSetup
        );
    }

    #[test]
    fn invalid_definition_is_isolated_from_healthy_sibling() {
        let root = tempdir().unwrap();
        let good = root.path().join("good");
        let bad = root.path().join("bad");
        write_observable_skill(&good, "good", &manifest("good", "rss.discover"));
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("SKILL.md"),
            "---\nname: bad\ndescription: invalid fixture\nmetadata:\n  magician:\n    \
             observe_source:\n      schema_version: nope\n---\n",
        )
        .unwrap();
        let catalog = discover_observable_sources(&[root.path().to_path_buf()]);
        assert!(catalog.definitions.contains_key("good"));
        assert_eq!(catalog.issues.len(), 1);
    }

    #[test]
    fn private_target_and_profile_authority_cap_fail_closed_during_projection() {
        let actions = ContentAcquisitionCatalog {
            principal: "p".into(),
            workspace: "w".into(),
            capability_revision: "c".into(),
            discovery: vec![rss_descriptor()],
            readers: vec![],
            unavailable: vec![],
        };
        let mut catalog = ObservableCatalog::default();
        let mut private = manifest("private-feed", "rss.discover");
        private.profiles[0].acquisition.targets = vec!["http://127.0.0.1/feed".into()];
        catalog.definitions.insert(
            "private-feed".into(),
            LoadedObservableSource {
                manifest: private,
                revision: "private-rev".into(),
                path: PathBuf::new(),
            },
        );
        let mut local_only = manifest("local-only", "rss.discover");
        local_only.profiles[0].acquisition.maximum_authority = Some(RetrievalAuthority::LocalOnly);
        catalog.definitions.insert(
            "local-only".into(),
            LoadedObservableSource {
                manifest: local_only,
                revision: "local-rev".into(),
                path: PathBuf::new(),
            },
        );

        let offers = project_observable_source_offers(
            &catalog,
            &actions,
            &ObservableSourcePolicy::default(),
            &BTreeSet::new(),
            None,
        );
        assert_eq!(offers.len(), 2);
        assert_eq!(
            offers
                .iter()
                .find(|offer| offer.source_id == "private-feed")
                .unwrap()
                .unavailable_reason,
            Some(ObservableUnavailableReason::TargetDenied)
        );
        assert_eq!(
            offers
                .iter()
                .find(|offer| offer.source_id == "local-only")
                .unwrap()
                .unavailable_reason,
            Some(ObservableUnavailableReason::AuthorityDenied)
        );
    }

    #[test]
    fn observe_schema_rejects_multiple_actions_instead_of_treating_them_as_fallbacks() {
        let mut value = manifest("multi", "rss.discover");
        value.profiles[0]
            .acquisition
            .allowed_actions
            .push("exa.discover".into());
        assert!(value.validate().is_err());
    }
}
